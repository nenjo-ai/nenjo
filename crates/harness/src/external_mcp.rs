//! External MCP server pool — manages long-lived connections to user-configured
//! MCP servers (stdio or HTTP transport) and exposes their tools via the worker
//! `Tool` trait.
//!
//! Lifecycle:
//! - On bootstrap (and `bootstrap.changed`), the pool is reconciled:
//!   servers that are no longer referenced are shut down, new ones are spawned.
//! - Stdio servers are child processes invoked via their `command` + `args`.
//! - HTTP servers are accessed via their `url` using JSON-RPC over HTTP.
//! - Tools are discovered via `tools/list` and cached per server.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use nenjo::manifest::McpServerManifest;
use nenjo_tools::{Tool, ToolCategory, ToolResult};

/// The name used for the Nenjo platform MCP server in the DB.
pub const PLATFORM_SERVER_NAME: &str = "app.nenjo.platform";

/// Find the platform server's UUID from a list of bootstrap servers.
pub fn platform_server_id(servers: &[McpServerManifest]) -> Option<Uuid> {
    servers
        .iter()
        .find(|s| s.name == PLATFORM_SERVER_NAME)
        .map(|s| s.id)
}

// ---------------------------------------------------------------------------
// MCP credential resolution
// ---------------------------------------------------------------------------

/// Resolve credentials for an MCP server from its `env_schema`.
///
/// Checks two sources in order:
/// 1. Environment variable: `NENJO_MCP_{SERVER_NAME}_{FIELD_KEY}` (uppercased,
///    non-alphanumeric replaced with `_`)
/// 2. Local file: `~/.nenjo/credentials.toml` under `[mcp.{server_name}]`
///
/// Returns a map of field key → resolved value for all fields that could be resolved.
fn resolve_mcp_credentials(
    server_name: &str,
    env_schema: &serde_json::Value,
) -> HashMap<String, String> {
    let fields = env_schema
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item["key"].as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if fields.is_empty() {
        return HashMap::new();
    }

    let sanitized_name = server_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();

    let mut credentials = HashMap::new();

    for field in &fields {
        // 0. Platform server shortcut: use NENJO_API_KEY directly for the
        //    built-in platform MCP server. This avoids requiring users to
        //    set a separate NENJO_MCP_APP_NENJO_PLATFORM_API_KEY env var.
        if server_name == PLATFORM_SERVER_NAME && field == "api_key" {
            if let Ok(val) = std::env::var("NENJO_API_KEY") {
                if !val.is_empty() {
                    debug!(server = %server_name, field = %field, source = "NENJO_API_KEY", "Platform MCP credential resolved");
                    credentials.insert(field.clone(), val);
                    continue;
                }
            }
        }

        // 1. Try env var: NENJO_MCP_{SERVER_NAME}_{FIELD_KEY}
        let env_key = format!("NENJO_MCP_{}_{}", sanitized_name, field.to_uppercase());
        if let Ok(val) = std::env::var(&env_key) {
            if !val.is_empty() {
                debug!(server = %server_name, field = %field, source = "env", env_key = %env_key, "MCP credential resolved");
                credentials.insert(field.clone(), val);
                continue;
            }
        }

        // 2. Try credentials.toml: [mcp.{server_name}] / field
        if let Some(val) = resolve_from_toml(server_name, field) {
            debug!(server = %server_name, field = %field, source = "local", "MCP credential resolved");
            credentials.insert(field.clone(), val);
            continue;
        }

        debug!(server = %server_name, field = %field, "MCP credential not found");
    }

    credentials
}

fn resolve_from_toml(server_name: &str, field: &str) -> Option<String> {
    let path = directories::UserDirs::new()
        .map(|u| u.home_dir().join(".nenjo").join("credentials.toml"))?;
    let content = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = content.parse().ok()?;
    let mcp_section = table.get("mcp")?.as_table()?;
    let server_section = mcp_section.get(server_name)?.as_table()?;
    server_section.get(field)?.as_str().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

// ---------------------------------------------------------------------------
// MCP tool definition (from tools/list response)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct ExternalMcpToolDef {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
    /// MCP scope required for this tool (e.g. "tasks:read").
    /// Only present for Nenjo platform tools — external servers don't have scopes.
    #[serde(default)]
    scope: Option<String>,
}

// ---------------------------------------------------------------------------
// Transport abstraction
// ---------------------------------------------------------------------------

enum Transport {
    /// Stdio transport — communicates via stdin/stdout of a child process.
    Stdio {
        child: Child,
        stdin: tokio::process::ChildStdin,
        stdout: BufReader<tokio::process::ChildStdout>,
    },
    /// HTTP (Streamable HTTP) transport — sends JSON-RPC over HTTP POST.
    Http {
        client: reqwest::Client,
        url: String,
        /// Extra headers to send with every request (e.g. Authorization).
        headers: reqwest::header::HeaderMap,
        /// Session ID returned by the server on initialize.
        session_id: Option<String>,
    },
}

impl Transport {
    async fn send_rpc(
        &mut self,
        method: &'static str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        match self {
            Transport::Http {
                client,
                url,
                headers,
                session_id,
            } => {
                let request = JsonRpcRequest {
                    jsonrpc: "2.0",
                    id: 1,
                    method,
                    params,
                };
                let mut req_builder = client
                    .post(url.as_str())
                    .headers(headers.clone())
                    .header("Accept", "application/json, text/event-stream");

                // Attach session ID if we have one
                if let Some(sid) = session_id.as_deref() {
                    req_builder = req_builder.header("Mcp-Session-Id", sid);
                }

                let resp = req_builder.json(&request).send().await?;
                let status = resp.status();

                // Capture session ID from response headers
                if let Some(new_sid) = resp
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
                {
                    *session_id = Some(new_sid.to_string());
                }

                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    anyhow::bail!("HTTP {status}: {body}");
                }

                // The server may respond with application/json or text/event-stream.
                // SSE responses wrap JSON-RPC in "data: " lines.
                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let body = resp.text().await?;

                let rpc: JsonRpcResponse = if content_type.contains("text/event-stream") {
                    let json_str = body
                        .lines()
                        .filter_map(|line| line.strip_prefix("data: "))
                        .rfind(|data| !data.is_empty() && *data != "[DONE]")
                        .ok_or_else(|| anyhow::anyhow!("No data in SSE response"))?;
                    serde_json::from_str(json_str)?
                } else {
                    serde_json::from_str(&body)?
                };

                if let Some(err) = rpc.error {
                    anyhow::bail!("RPC error {}: {}", err.code, err.message);
                }
                Ok(rpc.result.unwrap_or(serde_json::json!({})))
            }
            Transport::Stdio { stdin, stdout, .. } => {
                let request = JsonRpcRequest {
                    jsonrpc: "2.0",
                    id: 1,
                    method,
                    params,
                };
                let mut payload = serde_json::to_string(&request)?;
                payload.push('\n');
                stdin.write_all(payload.as_bytes()).await?;
                stdin.flush().await?;

                let mut line = String::new();
                stdout.read_line(&mut line).await?;
                if line.is_empty() {
                    anyhow::bail!("Stdio process closed stdout unexpectedly");
                }
                let rpc: JsonRpcResponse = serde_json::from_str(line.trim())?;
                if let Some(err) = rpc.error {
                    anyhow::bail!("RPC error {}: {}", err.code, err.message);
                }
                Ok(rpc.result.unwrap_or(serde_json::json!({})))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Connected server — holds transport + cached tool definitions
// ---------------------------------------------------------------------------

struct ConnectedServer {
    server_def: McpServerManifest,
    transport: Transport,
    tools: Vec<ExternalMcpToolDef>,
}

impl ConnectedServer {
    /// Connect to a server and discover its tools.
    async fn connect(def: McpServerManifest) -> anyhow::Result<Self> {
        // Resolve credentials from env vars and credentials.toml
        let credentials = resolve_mcp_credentials(&def.name, &def.env_schema);
        debug!(
            server = %def.name,
            transport = %def.transport,
            url = ?def.url,
            credential_keys = ?credentials.keys().collect::<Vec<_>>(),
            has_api_key = credentials.contains_key("api_key"),
            "Connecting to external MCP server"
        );

        let mut transport = match def.transport.as_str() {
            "http" => {
                let base_url = def
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("HTTP transport requires a URL"))?;

                // Allow URL override via env var: NENJO_MCP_{SERVER_NAME}_URL
                let sanitized = def
                    .name
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() {
                            c.to_ascii_uppercase()
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>();
                let url_env_key = format!("NENJO_MCP_{sanitized}_URL");
                let url = match std::env::var(&url_env_key) {
                    Ok(val) if !val.is_empty() => {
                        info!(server = %def.name, env_key = %url_env_key, "MCP server URL overridden from env");
                        val
                    }
                    _ => base_url.to_string(),
                };
                let url = url.as_str();

                // Build auth headers from resolved credentials
                let mut headers = reqwest::header::HeaderMap::new();
                // Convention: if env_schema has a field named "authorization" or
                // "api_key" or "token", use it as a Bearer token.
                let token = credentials
                    .get("authorization")
                    .or_else(|| credentials.get("api_key"))
                    .or_else(|| credentials.get("token"))
                    .or_else(|| credentials.get("bearer_token"));

                if let Some(token_val) = token {
                    let header_val = if token_val.to_lowercase().starts_with("bearer ") {
                        token_val.clone()
                    } else {
                        format!("Bearer {token_val}")
                    };
                    headers.insert(
                        reqwest::header::AUTHORIZATION,
                        header_val
                            .parse()
                            .map_err(|e| anyhow::anyhow!("Invalid auth header value: {e}"))?,
                    );
                }

                // Also inject any explicitly named headers from env_schema
                for (key, val) in &credentials {
                    if key.starts_with("header_") {
                        let header_name = key.strip_prefix("header_").unwrap();
                        if let (Ok(name), Ok(value)) = (
                            reqwest::header::HeaderName::from_bytes(header_name.as_bytes()),
                            reqwest::header::HeaderValue::from_str(val),
                        ) {
                            headers.insert(name, value);
                        }
                    }
                }

                Transport::Http {
                    client: reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(30))
                        .build()?,
                    url: url.to_string(),
                    headers,
                    session_id: None,
                }
            }
            "stdio" => {
                let cmd = def
                    .command
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Stdio transport requires a command"))?;
                let args = def.args.clone().unwrap_or_default();
                let mut child_cmd = Command::new(cmd);
                child_cmd
                    .args(&args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);

                // Inject resolved credentials as env vars for the child process
                for (key, val) in &credentials {
                    child_cmd.env(key.to_uppercase(), val);
                }

                let mut child = child_cmd
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("Failed to spawn '{}': {}", cmd, e))?;

                let stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("Failed to capture stdin for '{}'", cmd))?;
                let stdout =
                    BufReader::new(child.stdout.take().ok_or_else(|| {
                        anyhow::anyhow!("Failed to capture stdout for '{}'", cmd)
                    })?);

                Transport::Stdio {
                    child,
                    stdin,
                    stdout,
                }
            }
            other => anyhow::bail!("Unsupported transport: {other}"),
        };

        // Initialize the server
        let _init = transport
            .send_rpc(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "nenjo-worker",
                        "version": "0.1.0"
                    }
                })),
            )
            .await?;

        // Send initialized notification (ignore response)
        // For stdio, we send but don't expect a response for notifications
        if matches!(transport, Transport::Http { .. }) {
            // HTTP: send notification, expect 204 or ignore
            let _ = transport.send_rpc("notifications/initialized", None).await;
        }

        // Discover tools
        let tools_result = transport.send_rpc("tools/list", None).await?;
        let tools: Vec<ExternalMcpToolDef> = tools_result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();

        info!(
            server = %def.display_name,
            transport = %def.transport,
            tools = tools.len(),
            "External MCP server connected"
        );

        Ok(Self {
            server_def: def,
            transport,
            tools,
        })
    }

    /// Call a tool on this server.
    async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<String> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });
        let resp = self.transport.send_rpc("tools/call", Some(params)).await?;

        // Extract text content from MCP response
        let text = resp
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        Ok(text)
    }
}

impl Drop for ConnectedServer {
    fn drop(&mut self) {
        if let Transport::Stdio { ref mut child, .. } = self.transport {
            // Best-effort kill
            let _ = child.start_kill();
        }
    }
}

// ---------------------------------------------------------------------------
// External MCP Tool — wraps a single tool from an external server
// ---------------------------------------------------------------------------

/// A tool backed by an MCP server (external or platform).
pub struct ExternalMcpTool {
    tool_name: String,
    tool_description: String,
    input_schema: serde_json::Value,
    server_id: Uuid,
    pool: Arc<ExternalMcpPool>,
}

#[async_trait]
impl Tool for ExternalMcpTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    fn category(&self) -> ToolCategory {
        // External tools are always write — conservative default
        ToolCategory::Write
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        match self
            .pool
            .call_tool(self.server_id, &self.tool_name, args)
            .await
        {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => {
                error!(tool = %self.tool_name, error = %e, "MCP tool call failed");
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// External MCP Pool
// ---------------------------------------------------------------------------

/// Manages a pool of connected external MCP servers.
///
/// Servers are keyed by their UUID. The pool is reconciled when bootstrap data
/// changes: servers no longer assigned are shut down, new ones are spawned.
pub struct ExternalMcpPool {
    servers: RwLock<HashMap<Uuid, tokio::sync::Mutex<ConnectedServer>>>,
}

impl Default for ExternalMcpPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalMcpPool {
    pub fn new() -> Self {
        Self {
            servers: RwLock::new(HashMap::new()),
        }
    }

    /// Reconcile the pool with the given set of MCP server definitions.
    ///
    /// - Servers not in `desired` are shut down (dropped).
    /// - Servers already connected are kept.
    /// - New servers are connected.
    pub async fn reconcile(&self, desired: &[McpServerManifest]) {
        let desired_ids: std::collections::HashSet<Uuid> = desired.iter().map(|s| s.id).collect();

        // Remove servers no longer desired
        {
            let mut servers = self.servers.write().await;
            let to_remove: Vec<Uuid> = servers
                .keys()
                .filter(|id| !desired_ids.contains(id))
                .cloned()
                .collect();
            for id in to_remove {
                info!(server_id = %id, "Shutting down removed external MCP server");
                servers.remove(&id);
            }
        }

        // Connect new servers
        for def in desired {
            let already_connected = {
                let servers = self.servers.read().await;
                servers.contains_key(&def.id)
            };

            if already_connected {
                continue;
            }

            match ConnectedServer::connect(def.clone()).await {
                Ok(server) => {
                    let mut servers = self.servers.write().await;
                    servers.insert(def.id, tokio::sync::Mutex::new(server));
                }
                Err(e) => {
                    warn!(
                        server = %def.display_name,
                        server_id = %def.id,
                        error = %e,
                        "Failed to connect external MCP server — skipping"
                    );
                }
            }
        }
    }

    /// Call a tool on a specific server.
    async fn call_tool(
        &self,
        server_id: Uuid,
        tool_name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<String> {
        let servers = self.servers.read().await;
        let server_mutex = servers
            .get(&server_id)
            .ok_or_else(|| anyhow::anyhow!("Server {server_id} not connected"))?;
        let mut server = server_mutex.lock().await;
        server.call_tool(tool_name, args).await
    }

    /// Get tools for an agent, given the agent's assigned MCP server IDs.
    ///
    /// When `scopes` is provided, only tools whose `scope` field matches the
    /// given scopes are included. This is used for the internal Nenjo platform
    /// server where each tool has a scope (e.g. "tasks:read"). External servers
    /// have no per-tool scopes — pass `None` for those.
    pub async fn tools_for_agent(
        self: &Arc<Self>,
        mcp_server_ids: &[Uuid],
        scopes: Option<&[String]>,
    ) -> Vec<Box<dyn Tool>> {
        let servers = self.servers.read().await;
        let mut tools: Vec<Box<dyn Tool>> = Vec::new();

        for server_id in mcp_server_ids {
            let server_mutex = match servers.get(server_id) {
                Some(s) => s,
                None => {
                    debug!(server_id = %server_id, "MCP server not connected, skipping tools");
                    continue;
                }
            };

            let server = server_mutex.lock().await;
            let _server_name = &server.server_def.name;

            for tool_def in &server.tools {
                // Scope filtering: if scopes are provided and the tool has a scope,
                // only include if the agent's scopes grant access.
                if let Some(agent_scopes) = scopes {
                    if let Some(ref tool_scope) = tool_def.scope {
                        if !has_scope(agent_scopes, tool_scope) {
                            continue;
                        }
                    }
                }

                tools.push(Box::new(ExternalMcpTool {
                    tool_name: tool_def.name.clone(),
                    tool_description: tool_def.description.clone().unwrap_or_default(),
                    input_schema: tool_def.input_schema.clone(),
                    server_id: *server_id,
                    pool: Arc::clone(self),
                }));
            }
        }

        tools
    }

    /// Get metadata about connected external servers (for McpIntegrationContext).
    /// Skips the internal platform server.
    pub async fn server_info(&self, server_ids: &[Uuid]) -> Vec<(String, String)> {
        let servers = self.servers.read().await;
        let mut info = Vec::new();
        for id in server_ids {
            if let Some(server_mutex) = servers.get(id) {
                let server = server_mutex.lock().await;
                if server.server_def.name == PLATFORM_SERVER_NAME {
                    continue;
                }
                info.push((
                    server.server_def.display_name.clone(),
                    server.server_def.description.clone().unwrap_or_default(),
                ));
            }
        }
        info
    }
}

/// Check if the given scopes grant access to the required scope.
/// Empty scopes = full access. Write implies read.
fn has_scope(scopes: &[String], required: &str) -> bool {
    if scopes.is_empty() {
        return true;
    }
    if scopes.iter().any(|s| s == required) {
        return true;
    }
    // Write implies read
    if required.ends_with(":read") {
        let write_scope = required.replace(":read", ":write");
        return scopes.iter().any(|s| s == &write_scope);
    }
    false
}
