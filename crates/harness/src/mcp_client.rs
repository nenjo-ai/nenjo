//! MCP client — connects to the backend's `/mcp` endpoint and wraps MCP tools
//! as worker `Tool` trait implementations.
//!
//! The worker uses a full-access API key and filters tools by the role's
//! `platform_scopes` on the client side.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, warn};

use nenjo_tools::{Tool, ToolCategory, ToolResult};

// ---------------------------------------------------------------------------
// JSON-RPC types (mirrors backend)
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
pub struct McpToolDef {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
    /// The scope required for this tool (e.g. "tasks:read").
    /// Provided by the backend in the tools/list response.
    scope: String,
}

// ---------------------------------------------------------------------------
// MCP Client
// ---------------------------------------------------------------------------

/// Client for the backend's `/mcp` endpoint.
pub struct McpClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl McpClient {
    /// Create a new MCP client.
    ///
    /// `base_url` is the backend URL (e.g. `http://localhost:3001`).
    /// `api_key` is the worker's full-access API key.
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

    /// Fetch the tool list from the backend MCP server.
    ///
    /// Returns raw tool definitions including their scopes.
    /// When `agent_scopes` is provided, the backend filters tools server-side
    /// by those scopes (defense in depth).
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

    /// Call a tool on the backend MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<String> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });
        let resp = self.rpc("tools/call", Some(params)).await?;

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
// McpTool — wraps a single MCP tool as a worker Tool
// ---------------------------------------------------------------------------

/// A tool backed by the backend's MCP server.
///
/// Implements the worker `Tool` trait so it can be registered alongside
/// built-in tools. Tool calls are proxied to the backend via JSON-RPC.
pub struct McpTool {
    tool_name: String,
    tool_description: String,
    input_schema: serde_json::Value,
    client: Arc<McpClient>,
    tool_category: ToolCategory,
}

impl McpTool {
    fn new(def: McpToolDef, client: Arc<McpClient>) -> Self {
        let category = if def.scope.ends_with(":read") {
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
        match self.client.call_tool(&self.tool_name, args).await {
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
// Public helpers
// ---------------------------------------------------------------------------

/// Fetch MCP tools from the backend and return them as worker `Tool` instances,
/// filtered by the given `platform_scopes`.
///
/// If `platform_scopes` is empty, no MCP tools are returned (the role has no
/// platform access).
pub async fn mcp_tools_for_agent(
    base_url: &str,
    api_key: &str,
    platform_scopes: &[String],
) -> Vec<Box<dyn Tool>> {
    if platform_scopes.is_empty() || api_key.is_empty() || base_url.is_empty() {
        return Vec::new();
    }

    let client = Arc::new(McpClient::new(base_url, api_key));

    let all_tools = match client.list_tools(Some(platform_scopes)).await {
        Ok(tools) => tools,
        Err(e) => {
            warn!(error = %e, "Failed to fetch MCP tools from backend");
            return Vec::new();
        }
    };

    // Filter by the role's platform_scopes using the scope provided by the
    // backend in each tool definition. No inference needed.
    let filtered: Vec<Box<dyn Tool>> = all_tools
        .into_iter()
        .filter(|def| has_scope(platform_scopes, &def.scope))
        .map(|def| -> Box<dyn Tool> { Box::new(McpTool::new(def, client.clone())) })
        .collect();

    debug!(
        total = filtered.len(),
        scopes = ?platform_scopes,
        "MCP tools added to agent"
    );

    filtered
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
    use super::*;

    #[test]
    fn has_scope_empty_is_full_access() {
        assert!(has_scope(&[], "tasks:read"));
        assert!(has_scope(&[], "tasks:write"));
    }

    #[test]
    fn has_scope_write_implies_read() {
        let scopes = vec!["tasks:write".to_string()];
        assert!(has_scope(&scopes, "tasks:read"));
        assert!(has_scope(&scopes, "tasks:write"));
        assert!(!has_scope(&scopes, "projects:read"));
    }

    #[test]
    fn has_scope_exact_match() {
        let scopes = vec!["tasks:read".to_string()];
        assert!(has_scope(&scopes, "tasks:read"));
        assert!(!has_scope(&scopes, "tasks:write"));
    }
}
