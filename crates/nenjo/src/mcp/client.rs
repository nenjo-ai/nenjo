//! Generic MCP client — connects to any MCP server over HTTP and wraps
//! discovered tools as [`Tool`] trait implementations.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use nenjo_tools::{Tool, ToolCategory, ToolResult};

// ---------------------------------------------------------------------------
// JSON-RPC wire types
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

/// A tool definition returned by an MCP server's `tools/list` method.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    /// Optional legacy per-tool scope. Generic platform tools may omit this
    /// because scope is resolved dynamically from the call arguments.
    #[serde(default)]
    pub scope: String,
}

// ---------------------------------------------------------------------------
// MCP Client
// ---------------------------------------------------------------------------

/// Generic HTTP client for any MCP server using JSON-RPC.
pub struct McpClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl McpClient {
    /// Create a new MCP client.
    ///
    /// `base_url` is the server URL (e.g. `http://localhost:3001`).
    /// `api_key` is sent as a Bearer token in the Authorization header.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        let endpoint = format!("{}/mcp", base_url.trim_end_matches('/'));
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            endpoint,
            api_key: api_key.to_string(),
        }
    }

    /// Fetch the tool list from the MCP server.
    ///
    /// When `agent_scopes` is provided, it is passed to the server as a hint
    /// for server-side filtering (defense in depth).
    pub async fn list_tools(
        &self,
        agent_scopes: Option<&[String]>,
    ) -> anyhow::Result<Vec<McpToolDef>> {
        let params = agent_scopes.map(|scopes| serde_json::json!({ "agent_scopes": scopes }));
        let resp = self.rpc("tools/list", params).await?;
        let tools = resp
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        let defs: Vec<McpToolDef> = tools
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();

        debug!(tool_count = defs.len(), "MCP tools/list complete");
        Ok(defs)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        agent_scopes: Option<&[String]>,
    ) -> anyhow::Result<String> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
            "agent_scopes": agent_scopes,
        });
        let resp = self.rpc("tools/call", Some(params)).await?;

        // Extract text content from MCP response.
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

    /// Send a JSON-RPC request.
    async fn rpc(
        &self,
        method: &'static str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };

        let response = self
            .http
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MCP request failed (HTTP {status}): {body}");
        }

        let rpc_resp: JsonRpcResponse = response.json().await?;

        if let Some(err) = rpc_resp.error {
            anyhow::bail!("MCP RPC error {}: {}", err.code, err.message);
        }

        Ok(rpc_resp.result.unwrap_or(serde_json::json!({})))
    }
}

// ---------------------------------------------------------------------------
// McpTool — wraps a single MCP tool definition as a Tool impl
// ---------------------------------------------------------------------------

/// A tool backed by an MCP server.
///
/// Implements the [`Tool`] trait so it can be registered alongside built-in
/// tools. Tool calls are proxied to the server via JSON-RPC.
pub struct McpTool {
    tool_name: String,
    tool_description: String,
    input_schema: serde_json::Value,
    client: Arc<McpClient>,
    tool_category: ToolCategory,
    agent_scopes: Vec<String>,
}

impl McpTool {
    /// Create a new MCP tool from a definition and shared client.
    pub fn new(def: McpToolDef, client: Arc<McpClient>, agent_scopes: Vec<String>) -> Self {
        // Categorize by tool name for consolidated tools, fall back to scope suffix.
        let category = if def.name.ends_with("/read") || def.name.ends_with("/graph") {
            ToolCategory::Read
        } else if def.name.ends_with("/write") {
            ToolCategory::Write
        } else if def.scope.ends_with(":read") {
            ToolCategory::Read
        } else {
            ToolCategory::Write
        };

        Self {
            tool_name: def.name,
            tool_description: def.description,
            input_schema: def.input_schema,
            client,
            tool_category: category,
            agent_scopes,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
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
        self.tool_category
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        match self
            .client
            .call_tool(&self.tool_name, args, Some(&self.agent_scopes))
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
