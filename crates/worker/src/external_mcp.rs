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
use nenjo::manifest::McpServerManifest;
use nenjo::skills::SkillMcpToolInfo;
use nenjo::{Slug, Tool, ToolCategory, ToolOrigin, ToolResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::connector_egress_proxy::{ConnectorEgressProxy, DestinationPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerRuntime {
    Manifest,
    AgentBrowser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkPolicy {
    Direct,
    Proxied(DestinationPolicy),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolPolicy {
    hidden_arguments: &'static [&'static str],
    denied_arguments: &'static [&'static str],
    isolated_namespace_argument: Option<&'static str>,
    forced_arguments: &'static [(&'static str, &'static str)],
    default_arguments: &'static [(&'static str, &'static str)],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ServerPolicy {
    network: NetworkPolicy,
    tools: ToolPolicy,
}

impl ServerPolicy {
    const STANDARD: Self = Self {
        network: NetworkPolicy::Direct,
        tools: ToolPolicy {
            hidden_arguments: &[],
            denied_arguments: &[],
            isolated_namespace_argument: None,
            forced_arguments: &[],
            default_arguments: &[],
        },
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagedConnector {
    None,
    AgentBrowser,
    Unsupported(String),
    Invalid(&'static str),
}

#[derive(Debug, Clone)]
struct ServerSpec {
    manifest: McpServerManifest,
    runtime: ServerRuntime,
    policy: ServerPolicy,
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
        // 1. Try env var: NENJO_MCP_{SERVER_NAME}_{FIELD_KEY}
        let env_key = format!("NENJO_MCP_{}_{}", sanitized_name, field.to_uppercase());
        if let Ok(val) = std::env::var(&env_key)
            && !val.is_empty()
        {
            debug!(server = %server_name, field = %field, source = "env", env_key = %env_key, "MCP credential resolved");
            credentials.insert(field.clone(), val);
            continue;
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

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
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
    /// MCP scope required for this tool (e.g. "projects:read").
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

    async fn send_notification(
        &mut self,
        method: &'static str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        };

        match self {
            Transport::Http {
                client,
                url,
                headers,
                session_id,
            } => {
                let mut request = client
                    .post(url.as_str())
                    .headers(headers.clone())
                    .header("Accept", "application/json, text/event-stream");
                if let Some(session_id) = session_id.as_deref() {
                    request = request.header("Mcp-Session-Id", session_id);
                }
                let response = request.json(&notification).send().await?;
                if !response.status().is_success() {
                    anyhow::bail!("HTTP {} sending MCP notification", response.status());
                }
                if let Some(new_session_id) = response
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|value| value.to_str().ok())
                {
                    *session_id = Some(new_session_id.to_string());
                }
            }
            Transport::Stdio { stdin, .. } => {
                let mut payload = serde_json::to_string(&notification)?;
                payload.push('\n');
                stdin.write_all(payload.as_bytes()).await?;
                stdin.flush().await?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Connected server — holds transport + cached tool definitions
// ---------------------------------------------------------------------------

struct ConnectedServer {
    server_def: McpServerManifest,
    runtime: ServerRuntime,
    policy: ServerPolicy,
    transport: Transport,
    tools: Vec<ExternalMcpToolDef>,
    _egress_proxy: Option<ConnectorEgressProxy>,
    _runtime_config: Option<tempfile::NamedTempFile>,
}

impl ConnectedServer {
    /// Connect to a server and discover its tools.
    async fn connect(spec: ServerSpec) -> anyhow::Result<Self> {
        let ServerSpec {
            manifest: def,
            runtime,
            policy,
        } = spec;
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

        let mut egress_proxy = None;
        let mut runtime_config = None;
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
                if let Some(env) = plugin_mcp_env(&def.metadata) {
                    child_cmd.envs(env);
                }
                if let Some(cwd) = plugin_mcp_cwd(&def.metadata) {
                    child_cmd.current_dir(cwd);
                }
                if let NetworkPolicy::Proxied(destination_policy) = policy.network {
                    let proxy = ConnectorEgressProxy::start(destination_policy).await?;
                    match runtime {
                        ServerRuntime::Manifest => {
                            anyhow::bail!(
                                "Restricted network policies require a connector runtime adapter"
                            );
                        }
                        ServerRuntime::AgentBrowser => {
                            let config = agent_browser_config(&proxy)?;
                            child_cmd
                                .env_remove("AGENT_BROWSER_ALLOWED_DOMAINS")
                                .env_remove("AGENT_BROWSER_PROVIDER")
                                .env_remove("AGENT_BROWSER_AUTO_CONNECT")
                                .env_remove("AGENT_BROWSER_CDP")
                                // The worker host and its state volume are the trust boundary;
                                // persisted browser session JSON is intentionally plaintext.
                                .env_remove("AGENT_BROWSER_ENCRYPTION_KEY")
                                .env_remove("AGENT_BROWSER_HEADED")
                                .env_remove("AGENT_BROWSER_NAMESPACE")
                                .env_remove("AGENT_BROWSER_PLUGINS")
                                .env_remove("AGENT_BROWSER_PROFILE")
                                .env_remove("AGENT_BROWSER_RESTORE")
                                .env_remove("AGENT_BROWSER_RESTORE_CHECK_FN")
                                .env_remove("AGENT_BROWSER_RESTORE_CHECK_TEXT")
                                .env_remove("AGENT_BROWSER_RESTORE_CHECK_URL")
                                .env_remove("AGENT_BROWSER_RESTORE_SAVE")
                                .env_remove("AGENT_BROWSER_SESSION")
                                .env_remove("AGENT_BROWSER_SESSION_NAME")
                                .env_remove("AGENT_BROWSER_STATE")
                                .env("AGENT_BROWSER_CONFIG", config.path())
                                .env("AGENT_BROWSER_PROXY", proxy.url())
                                // Chromium otherwise bypasses proxies for loopback hosts.
                                .env("AGENT_BROWSER_PROXY_BYPASS", "<-loopback>")
                                .env("AGENT_BROWSER_CONTENT_BOUNDARIES", "1")
                                .env("AGENT_BROWSER_MAX_OUTPUT", "50000")
                                // Prevent WebRTC from creating non-proxied UDP sockets.
                                .env(
                                    "AGENT_BROWSER_ARGS",
                                    "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                                );
                            runtime_config = Some(config);
                        }
                    }
                    egress_proxy = Some(proxy);
                }

                let mut child = child_cmd
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("Failed to spawn '{}': {}", cmd, e))?;

                if let Some(stderr) = child.stderr.take() {
                    let server_name = def.name.clone();
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(stderr).lines();
                        loop {
                            match lines.next_line().await {
                                Ok(Some(line)) => {
                                    debug!(server = %server_name, %line, "MCP server stderr")
                                }
                                Ok(None) => break,
                                Err(error) => {
                                    warn!(server = %server_name, %error, "Failed reading MCP server stderr");
                                    break;
                                }
                            }
                        }
                    });
                }

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

        transport
            .send_notification("notifications/initialized", None)
            .await?;

        // Discover every page of tools exposed by the server.
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map(|cursor| serde_json::json!({ "cursor": cursor }));
            let tools_result = transport.send_rpc("tools/list", params).await?;
            tools.extend(
                tools_result
                    .get("tools")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|tool| serde_json::from_value(tool).ok()),
            );
            cursor = tools_result
                .get("nextCursor")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        apply_tool_schema_policy(&mut tools, policy.tools);

        info!(
            server = %def.display_name,
            transport = %def.transport,
            tools = tools.len(),
            "External MCP server connected"
        );

        Ok(Self {
            server_def: def,
            runtime,
            policy,
            transport,
            tools,
            _egress_proxy: egress_proxy,
            _runtime_config: runtime_config,
        })
    }

    fn matches(&self, spec: &ServerSpec) -> bool {
        self.runtime == spec.runtime
            && self.policy == spec.policy
            && serde_json::to_value(&self.server_def).ok()
                == serde_json::to_value(&spec.manifest).ok()
    }

    /// Call a tool on this server.
    async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<String> {
        validate_tool_arguments(self.policy.tools, &arguments)?;
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

fn agent_browser_config(proxy: &ConnectorEgressProxy) -> anyhow::Result<tempfile::NamedTempFile> {
    let mut file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "headed": false,
            "proxy": proxy.url(),
            "proxyBypass": "<-loopback>",
            "contentBoundaries": true,
            "maxOutput": 50_000,
            "args": "--force-webrtc-ip-handling-policy=disable_non_proxied_udp"
        }),
    )?;
    file.flush()?;
    Ok(file)
}

fn apply_tool_schema_policy(tools: &mut [ExternalMcpToolDef], policy: ToolPolicy) {
    for tool in tools {
        if let Some(properties) = tool
            .input_schema
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
        {
            for argument in policy.hidden_arguments {
                properties.remove(*argument);
            }
        }
    }
}

fn validate_tool_arguments(
    policy: ToolPolicy,
    arguments: &serde_json::Value,
) -> Result<(), anyhow::Error> {
    for argument in policy.denied_arguments {
        if arguments.get(*argument).is_some() {
            anyhow::bail!("MCP argument '{argument}' is disabled by the connector security policy");
        }
    }
    Ok(())
}

fn tool_argument_maps(
    policy: ToolPolicy,
    execution_namespace: &str,
) -> (
    serde_json::Map<String, serde_json::Value>,
    serde_json::Map<String, serde_json::Value>,
) {
    let mut forced = policy
        .forced_arguments
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_string(),
                serde_json::Value::String((*value).to_string()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    if let Some(argument) = policy.isolated_namespace_argument {
        forced.insert(
            argument.to_string(),
            serde_json::Value::String(execution_namespace.to_string()),
        );
    }
    let defaults = policy
        .default_arguments
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_string(),
                serde_json::Value::String((*value).to_string()),
            )
        })
        .collect();
    (forced, defaults)
}

fn plugin_mcp_env(metadata: &serde_json::Value) -> Option<Vec<(String, String)>> {
    let env = metadata
        .pointer("/runtime/env")
        .or_else(|| metadata.pointer("/claude/mcp/env"))?
        .as_object()?;
    Some(
        env.iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
            .collect(),
    )
}

fn plugin_mcp_cwd(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .pointer("/runtime/cwd")
        .or_else(|| metadata.pointer("/claude/mcp/cwd"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

impl Drop for ConnectedServer {
    fn drop(&mut self) {
        if let Transport::Stdio { ref mut child, .. } = self.transport {
            // Best-effort kill
            let _ = child.start_kill();
        }
    }
}

fn managed_connector(metadata: &serde_json::Value) -> ManagedConnector {
    if metadata.pointer("/runtime/connector").is_some() {
        return ManagedConnector::Invalid(
            "metadata.runtime.connector is no longer supported; use metadata.nenjo.managed_connector",
        );
    }

    match metadata.pointer("/nenjo/managed_connector") {
        None => ManagedConnector::None,
        Some(serde_json::Value::String(connector)) if connector.trim().is_empty() => {
            ManagedConnector::Invalid("metadata.nenjo.managed_connector cannot be empty")
        }
        Some(serde_json::Value::String(connector))
            if matches!(connector.as_str(), "agent_browser" | "agent-browser") =>
        {
            ManagedConnector::AgentBrowser
        }
        Some(serde_json::Value::String(connector)) => {
            ManagedConnector::Unsupported(connector.to_string())
        }
        Some(_) => ManagedConnector::Invalid(
            "metadata.nenjo.managed_connector must be a string identifier",
        ),
    }
}

fn agent_browser_server_policy() -> ServerPolicy {
    ServerPolicy {
        network: NetworkPolicy::Proxied(DestinationPolicy::PublicOnly),
        tools: ToolPolicy {
            hidden_arguments: &[
                "allowedDomains",
                "extraArgs",
                "headed",
                "namespace",
                "restore",
                "restoreCheckFn",
                "restoreCheckText",
                "restoreCheckUrl",
                "restoreSave",
                "session",
            ],
            denied_arguments: &[
                "allowedDomains",
                "extraArgs",
                "headed",
                "restoreCheckFn",
                "restoreCheckText",
                "restoreCheckUrl",
            ],
            isolated_namespace_argument: Some("namespace"),
            forced_arguments: &[
                ("restore", "default"),
                ("restoreSave", "auto"),
                ("session", "default"),
            ],
            default_arguments: &[],
        },
    }
}

fn resolve_server_spec(manifest: &McpServerManifest) -> Option<ServerSpec> {
    resolve_server_spec_with(manifest, || {
        which::which("agent-browser").map_err(|error| error.to_string())
    })
}

fn resolve_server_spec_with(
    manifest: &McpServerManifest,
    find_agent_browser: impl FnOnce() -> Result<std::path::PathBuf, String>,
) -> Option<ServerSpec> {
    match managed_connector(&manifest.metadata) {
        ManagedConnector::None => Some(ServerSpec {
            manifest: manifest.clone(),
            runtime: ServerRuntime::Manifest,
            policy: ServerPolicy::STANDARD,
        }),
        ManagedConnector::AgentBrowser => {
            let command = match find_agent_browser() {
                Ok(command) => command,
                Err(error) => {
                    warn!(
                        server = %manifest.name,
                        %error,
                        "Agent Browser connector is installed but the agent-browser CLI is not available on PATH"
                    );
                    return None;
                }
            };
            let mut manifest = manifest.clone();
            manifest.transport = "stdio".to_string();
            manifest.command = Some(command.to_string_lossy().into_owned());
            manifest.args = Some(vec![
                "mcp".to_string(),
                "--tools".to_string(),
                "core".to_string(),
            ]);
            manifest.url = None;
            Some(ServerSpec {
                manifest,
                runtime: ServerRuntime::AgentBrowser,
                policy: agent_browser_server_policy(),
            })
        }
        ManagedConnector::Unsupported(connector) => {
            warn!(
                server = %manifest.name,
                %connector,
                "MCP manifest declares an unsupported managed connector"
            );
            None
        }
        ManagedConnector::Invalid(reason) => {
            warn!(
                server = %manifest.name,
                %reason,
                "MCP manifest declares invalid managed connector metadata"
            );
            None
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
    server: Slug,
    pool: Arc<ExternalMcpPool>,
    forced_arguments: serde_json::Map<String, serde_json::Value>,
    default_arguments: serde_json::Map<String, serde_json::Value>,
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

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Mcp
    }

    async fn execute(&self, mut args: serde_json::Value) -> anyhow::Result<ToolResult> {
        apply_argument_policy(&mut args, &self.forced_arguments, &self.default_arguments)?;
        match self
            .pool
            .call_tool(&self.server, &self.tool_name, args)
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

fn apply_argument_policy(
    arguments: &mut serde_json::Value,
    forced: &serde_json::Map<String, serde_json::Value>,
    defaults: &serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    if forced.is_empty() && defaults.is_empty() {
        return Ok(());
    }

    let arguments = arguments
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("MCP tool arguments must be an object"))?;
    for (key, value) in defaults {
        arguments
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    for (key, value) in forced {
        arguments.insert(key.clone(), value.clone());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// External MCP Pool
// ---------------------------------------------------------------------------

/// Manages a pool of connected external MCP servers.
///
/// Servers are keyed by their slug. The pool is reconciled when bootstrap data
/// changes: servers no longer assigned are shut down, new ones are spawned.
pub struct ExternalMcpPool {
    servers: RwLock<HashMap<Slug, Arc<Mutex<ConnectedServer>>>>,
    reconcile_lock: Mutex<()>,
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
            reconcile_lock: Mutex::new(()),
        }
    }

    /// Reconcile the pool with the given set of MCP server definitions.
    ///
    /// - Servers not in `desired` are shut down (dropped).
    /// - Unchanged servers are kept; changed definitions reconnect.
    /// - New servers are connected.
    pub async fn reconcile(&self, desired: &[McpServerManifest]) {
        let _reconcile_guard = self.reconcile_lock.lock().await;
        let desired = self.desired_specs(desired);
        let desired_slugs: HashSet<Slug> = desired
            .iter()
            .map(|server| Slug::derive(&server.manifest.name))
            .collect();

        // Remove servers no longer desired
        {
            let mut servers = self.servers.write().await;
            let to_remove: Vec<Slug> = servers
                .keys()
                .filter(|slug| !desired_slugs.contains(slug))
                .cloned()
                .collect();
            for slug in to_remove {
                info!(server = %slug, "Shutting down removed external MCP server");
                servers.remove(&slug);
            }
        }

        // Connect new or changed servers.
        for spec in desired {
            let server_slug = Slug::derive(&spec.manifest.name);
            let existing = {
                let servers = self.servers.read().await;
                servers.get(&server_slug).cloned()
            };
            if let Some(existing) = existing {
                let matches = existing.lock().await.matches(&spec);
                if matches {
                    continue;
                }
                info!(server = %server_slug, "Reconnecting changed MCP server");
                self.servers.write().await.remove(&server_slug);
            }

            match ConnectedServer::connect(spec).await {
                Ok(server) => {
                    let mut servers = self.servers.write().await;
                    servers.insert(server_slug, Arc::new(Mutex::new(server)));
                }
                Err(e) => {
                    warn!(
                        server = %server_slug,
                        error = %e,
                        "Failed to connect MCP server — skipping"
                    );
                }
            }
        }
    }

    fn desired_specs(&self, manifests: &[McpServerManifest]) -> Vec<ServerSpec> {
        manifests.iter().filter_map(resolve_server_spec).collect()
    }

    /// Call a tool on a specific server.
    pub(crate) async fn call_skill_mcp_tool(
        &self,
        active_servers: &[Slug],
        requested_server: Option<&str>,
        tool_name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<String> {
        if active_servers.is_empty() {
            anyhow::bail!(
                "No skill MCP servers are active. Activate a skill first with use_skill."
            );
        }

        let server_slug = if let Some(requested_server) = requested_server {
            let requested_slug = Slug::derive(requested_server);
            active_servers
                .iter()
                .find(|slug| slug.as_str() == requested_server || **slug == requested_slug)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "MCP server '{requested_server}' is not active for the current skill"
                    )
                })?
        } else {
            self.resolve_active_tool_server(active_servers, tool_name)
                .await?
        };

        self.call_tool(&server_slug, tool_name, args).await
    }

    pub(crate) async fn skill_mcp_tool_inventory(
        &self,
        active_servers: &[Slug],
    ) -> Vec<SkillMcpToolInfo> {
        let servers = {
            let connected = self.servers.read().await;
            active_servers
                .iter()
                .filter_map(|slug| connected.get(slug).cloned().map(|server| (slug, server)))
                .collect::<Vec<_>>()
        };
        let mut inventory = Vec::new();
        for (active_server, server) in servers {
            let server = server.lock().await;
            for tool in &server.tools {
                inventory.push(SkillMcpToolInfo {
                    server: active_server.clone(),
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                });
            }
        }
        inventory
    }

    async fn resolve_active_tool_server(
        &self,
        active_servers: &[Slug],
        tool_name: &str,
    ) -> anyhow::Result<Slug> {
        let servers = {
            let connected = self.servers.read().await;
            active_servers
                .iter()
                .filter_map(|slug| connected.get(slug).cloned().map(|server| (slug, server)))
                .collect::<Vec<_>>()
        };
        let mut matches = Vec::new();
        for (active_server, server) in servers {
            let server = server.lock().await;
            if server.tools.iter().any(|tool| tool.name == tool_name) {
                matches.push(active_server.clone());
            }
        }

        match matches.len() {
            0 => anyhow::bail!("MCP tool '{tool_name}' is not available for the active skill"),
            1 => Ok(matches.remove(0)),
            _ => anyhow::bail!(
                "MCP tool '{tool_name}' exists on multiple active skill servers; pass the server parameter"
            ),
        }
    }

    async fn call_tool(
        &self,
        server_slug: &Slug,
        tool_name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<String> {
        let server = self
            .servers
            .read()
            .await
            .get(server_slug)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Server {server_slug} not connected"))?;
        let mut server = server.lock().await;
        server.call_tool(tool_name, args).await
    }

    /// Get tools from an agent's assigned MCP server slugs.
    ///
    /// When `scopes` is provided, only tools whose `scope` field matches the
    /// given scopes are included. This is used for the internal Nenjo platform
    /// server where each tool has a scope (e.g. "projects:read"). External servers
    /// have no per-tool scopes — pass `None` for those.
    pub async fn tools_for_agent(
        self: &Arc<Self>,
        mcp_servers: &[Slug],
        scopes: Option<&[String]>,
        execution_namespace: &str,
    ) -> Vec<Box<dyn Tool>> {
        let servers = {
            let connected = self.servers.read().await;
            mcp_servers
                .iter()
                .filter_map(|slug| {
                    connected
                        .get(slug)
                        .cloned()
                        .map(|server| (slug.clone(), server))
                })
                .collect::<Vec<_>>()
        };
        let mut tools: Vec<Box<dyn Tool>> = Vec::new();

        for (server_slug, server) in servers {
            let server = server.lock().await;
            for tool_def in &server.tools {
                // Scope filtering: if scopes are provided and the tool has a scope,
                // only include if the agent's scopes grant access.
                if let Some(agent_scopes) = scopes
                    && let Some(ref tool_scope) = tool_def.scope
                    && !has_scope(agent_scopes, tool_scope)
                {
                    continue;
                }

                let (forced_arguments, default_arguments) =
                    tool_argument_maps(server.policy.tools, execution_namespace);
                tools.push(Box::new(ExternalMcpTool {
                    tool_name: tool_def.name.clone(),
                    tool_description: tool_def.description.clone().unwrap_or_default(),
                    input_schema: tool_def.input_schema.clone(),
                    server: server_slug.clone(),
                    pool: Arc::clone(self),
                    forced_arguments,
                    default_arguments,
                }));
            }
        }

        tools
    }

    /// Get metadata about connected external servers (for McpIntegrationContext).
    pub async fn server_info(&self, server_slugs: &[Slug]) -> Vec<(String, String)> {
        let servers = {
            let connected = self.servers.read().await;
            server_slugs
                .iter()
                .filter_map(|slug| connected.get(slug).cloned())
                .collect::<Vec<_>>()
        };
        let mut info = Vec::new();
        for server in servers {
            let server = server.lock().await;
            info.push((
                server.server_def.display_name.clone(),
                server.server_def.description.clone().unwrap_or_default(),
            ));
        }
        info
    }
}

#[async_trait]
impl crate::handlers::manifest::McpRuntime for ExternalMcpPool {
    async fn reconcile_mcp(&self, servers: &[McpServerManifest]) {
        self.reconcile(servers).await;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nenjo::manifest::McpServerManifest;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn claude_plugin_mcp_runtime_uses_plugin_cwd_and_env() {
        let temp = tempfile::tempdir().unwrap();
        let plugin_dir = temp.path().join("plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        let server_script = plugin_dir.join("server.sh");
        tokio::fs::write(&server_script, mcp_fixture_script())
            .await
            .unwrap();

        let server = McpServerManifest {
            name: "ralph_loop__review_server".to_string(),
            display_name: "ralph_loop:review_server".to_string(),
            description: Some("Review server".to_string()),
            transport: "stdio".to_string(),
            command: Some("bash".to_string()),
            args: Some(vec!["server.sh".to_string()]),
            url: None,
            env_schema: json!([]),
            source_type: "package".to_string(),
            read_only: true,
            metadata: json!({
                "runtime": {
                    "cwd": plugin_dir.to_string_lossy().to_string(),
                    "env": {
                        "MODE": "local",
                        "PLUGIN_SENTINEL": "present"
                    }
                },
                "claude": {
                    "plugin": {
                        "slug": "ralph_loop",
                        "name": "Ralph Loop"
                    },
                    "mcp": {
                        "name": "review-server",
                        "slug": "review_server"
                    }
                }
            }),
        };
        let pool = Arc::new(ExternalMcpPool::new());

        pool.reconcile(std::slice::from_ref(&server)).await;

        let tools = pool
            .tools_for_agent(&[Slug::derive(&server.name)], None, "test-agent")
            .await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "review");
        let result = tools[0].execute(json!({})).await.unwrap();
        assert!(result.success);
        let expected_cwd = tokio::fs::canonicalize(&plugin_dir).await.unwrap();
        assert_eq!(
            result.output,
            format!("cwd={};mode=local;sentinel=present", expected_cwd.display())
        );
    }

    #[tokio::test]
    async fn mcp_tools_are_hidden_without_an_agent_assignment() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("server.sh");
        tokio::fs::write(&script, mcp_fixture_script())
            .await
            .unwrap();
        let manifest = fixture_manifest(&script, "global-browser");
        let slug = Slug::derive(&manifest.name);
        let connected = ConnectedServer::connect(ServerSpec {
            manifest,
            runtime: ServerRuntime::Manifest,
            policy: ServerPolicy::STANDARD,
        })
        .await
        .unwrap();
        let pool = Arc::new(ExternalMcpPool::new());
        pool.servers
            .write()
            .await
            .insert(slug, Arc::new(Mutex::new(connected)));

        let tools = pool.tools_for_agent(&[], None, "test-agent").await;

        assert!(tools.is_empty());
    }

    #[test]
    fn agent_browser_connector_resolves_the_worker_local_cli() {
        let mut manifest = fixture_manifest(std::path::Path::new("unused"), "agent-browser");
        manifest.transport = "connector".to_string();
        manifest.command = None;
        manifest.args = None;
        manifest.metadata = json!({
            "nenjo": {
                "managed_connector": "agent_browser"
            }
        });

        let spec = resolve_server_spec_with(&manifest, || {
            Ok(std::path::PathBuf::from("/opt/nenjo/bin/agent-browser"))
        })
        .unwrap();

        assert_eq!(spec.runtime, ServerRuntime::AgentBrowser);
        assert_eq!(
            spec.policy.network,
            NetworkPolicy::Proxied(DestinationPolicy::PublicOnly)
        );
        assert_eq!(
            spec.manifest.command.as_deref(),
            Some("/opt/nenjo/bin/agent-browser")
        );
        assert_eq!(
            spec.manifest.args,
            Some(vec!["mcp".into(), "--tools".into(), "core".into()])
        );
        assert_eq!(spec.manifest.transport, "stdio");
        assert!(spec.manifest.url.is_none());
    }

    #[test]
    fn unknown_managed_connector_is_not_started_as_an_external_command() {
        let mut manifest = fixture_manifest(std::path::Path::new("unused"), "unknown");
        manifest.metadata = json!({
            "nenjo": {
                "managed_connector": "not-supported"
            }
        });

        assert!(resolve_server_spec_with(&manifest, || unreachable!()).is_none());
    }

    #[test]
    fn legacy_connector_metadata_is_not_started_as_an_external_command() {
        let mut manifest = fixture_manifest(std::path::Path::new("unused"), "legacy");
        manifest.metadata = json!({
            "runtime": {
                "connector": "agent_browser"
            }
        });

        assert!(resolve_server_spec_with(&manifest, || unreachable!()).is_none());
    }

    #[test]
    fn connector_tool_policy_hides_denies_and_forces_arguments() {
        let mut tools = vec![ExternalMcpToolDef {
            name: "agent_browser_open".to_string(),
            description: None,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "allowedDomains": {"type": "array", "items": {"type": "string"}},
                    "url": {"type": "string"},
                    "headed": {"type": "boolean"},
                    "namespace": {"type": "string"},
                    "restore": {"oneOf": [{"type": "boolean"}, {"type": "string"}]},
                    "restoreCheckFn": {"type": "string"},
                    "restoreCheckText": {"type": "string"},
                    "restoreCheckUrl": {"type": "string"},
                    "restoreSave": {"type": "string"},
                    "session": {"type": "string"},
                    "extraArgs": {"type": "array", "items": {"type": "string"}}
                }
            }),
            scope: None,
        }];

        let policy = agent_browser_server_policy().tools;
        apply_tool_schema_policy(&mut tools, policy);
        for hidden in policy.hidden_arguments {
            assert!(
                tools[0].input_schema["properties"].get(*hidden).is_none(),
                "{hidden} should be hidden"
            );
        }
        assert!(
            validate_tool_arguments(policy, &json!({"extraArgs": ["--proxy-bypass", "*"]}))
                .unwrap_err()
                .to_string()
                .contains("'extraArgs' is disabled")
        );
        assert!(
            validate_tool_arguments(policy, &json!({"headed": true}))
                .unwrap_err()
                .to_string()
                .contains("'headed' is disabled")
        );
        for argument in [
            "allowedDomains",
            "restoreCheckFn",
            "restoreCheckText",
            "restoreCheckUrl",
        ] {
            assert!(
                validate_tool_arguments(policy, &json!({(argument): "disabled"}))
                    .unwrap_err()
                    .to_string()
                    .contains(&format!("'{argument}' is disabled"))
            );
        }

        let (forced, defaults) = tool_argument_maps(policy, "nenjo-session-safe");
        let mut arguments = json!({
            "namespace": "attacker-selected",
            "restore": false,
            "restoreSave": "never",
            "session": "named-tab"
        });
        apply_argument_policy(&mut arguments, &forced, &defaults).unwrap();
        assert_eq!(arguments["namespace"], "nenjo-session-safe");
        assert_eq!(arguments["restore"], "default");
        assert_eq!(arguments["restoreSave"], "auto");
        assert_eq!(arguments["session"], "default");
    }

    fn fixture_manifest(script: &std::path::Path, name: &str) -> McpServerManifest {
        McpServerManifest {
            name: name.to_string(),
            display_name: name.to_string(),
            description: None,
            transport: "stdio".to_string(),
            command: Some("bash".to_string()),
            args: Some(vec![script.to_string_lossy().into_owned()]),
            url: None,
            env_schema: json!([]),
            source_type: "test".to_string(),
            read_only: false,
            metadata: serde_json::Value::Null,
        }
    }

    fn mcp_fixture_script() -> String {
        r#"#!/usr/bin/env bash
set -euo pipefail
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"fixture","version":"0.1.0"}}}'
      ;;
    *'"method":"notifications/initialized"'*)
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"review","description":"Review","inputSchema":{"type":"object","properties":{}}}]}}'
      ;;
    *'"method":"tools/call"'*)
      text="cwd=$(pwd);mode=${MODE:-};sentinel=${PLUGIN_SENTINEL:-}"
      printf '{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$text"
      ;;
    *)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"unknown method"}}'
      ;;
  esac
done
"#
        .to_string()
    }
}
