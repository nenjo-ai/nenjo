//! Tool re-exports and factory for the harness.
//!
//! Re-exports the `Tool` trait and built-in tools from `nenjo-tools`, and
//! provides a `HarnessToolFactory` that builds per-agent tool sets.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::ToolFactory;
use nenjo::manifest::AgentManifest;
use nenjo_tools::security::SecurityPolicy;

// Re-export core tool types.
pub use nenjo_tools::{Tool, Tool as ToolTrait, ToolCategory, ToolResult, ToolSpec};

// Re-export built-in tool implementations.
pub use nenjo_tools::{
    BrowserOpenTool, BrowserTool, ContentSearchTool, FileEditTool, FileReadTool, FileWriteTool,
    GitOperationsTool, GlobSearchTool, HttpRequestTool, MemoryForgetTool, MemoryRecallTool,
    MemoryStoreTool, ScreenshotTool, ShellTool, WebFetchTool, WebSearchTool,
};

// Re-export UseAbilityTool from nenjo SDK.
pub use nenjo::agents::abilities::UseAbilityTool;

/// A tool factory that builds per-agent tool sets for the harness.
///
/// Uses the agent's configuration, security policy, MCP server pool, and
/// platform MCP client to build a complete tool set per agent.
pub struct HarnessToolFactory {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter>,
    config: crate::config::Config,
    external_mcp: Arc<crate::external_mcp::ExternalMcpPool>,
}

impl HarnessToolFactory {
    pub fn new(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter>,
        config: crate::config::Config,
        external_mcp: Arc<crate::external_mcp::ExternalMcpPool>,
    ) -> Self {
        Self {
            security,
            runtime,
            config,
            external_mcp,
        }
    }

    /// Build the base tool set (always included).
    pub fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(ShellTool::new(self.security.clone(), self.runtime.clone())),
            Arc::new(FileReadTool::new(self.security.clone())),
            Arc::new(FileWriteTool::new(self.security.clone())),
            Arc::new(FileEditTool::new(self.security.clone())),
            Arc::new(GitOperationsTool::new(
                self.security.clone(),
                self.config.workspace_dir.clone(),
            )),
            Arc::new(ContentSearchTool::new(self.security.clone())),
            Arc::new(GlobSearchTool::new(self.security.clone())),
        ]
    }
}

#[async_trait]
impl ToolFactory for HarnessToolFactory {
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base_tools();

        // Add MCP tools scoped to this agent's server assignments and platform scopes.
        if !agent.mcp_server_ids.is_empty() {
            let mcp_tools = self
                .external_mcp
                .tools_for_agent(
                    &agent.mcp_server_ids,
                    if agent.platform_scopes.is_empty() {
                        None
                    } else {
                        Some(&agent.platform_scopes)
                    },
                )
                .await;
            // Convert Box<dyn Tool> → Arc<dyn Tool>
            for t in mcp_tools {
                tools.push(Arc::from(t));
            }
        }

        // Add platform MCP tools (backend /mcp endpoint) filtered by agent's scopes.
        if !agent.platform_scopes.is_empty() {
            let platform_tools = crate::mcp_client::mcp_tools_for_agent(
                self.config.backend_api_url(),
                &self.config.api_key,
                &agent.platform_scopes,
            )
            .await;
            for t in platform_tools {
                tools.push(Arc::from(t));
            }
        }

        // Web fetch (always included with config, deny-by-default via allowed_domains)
        if self.config.web_fetch.enabled {
            tools.push(Arc::new(WebFetchTool::new(
                self.security.clone(),
                self.config.web_fetch.allowed_domains.clone(),
                self.config.web_fetch.blocked_domains.clone(),
                self.config.web_fetch.max_response_size,
                self.config.web_fetch.timeout_secs,
            )));
        }

        // Web search
        if self.config.web_search.enabled {
            tools.push(Arc::new(WebSearchTool::new(
                self.config.web_search.provider.clone(),
                self.config.web_search.brave_api_key.clone(),
                self.config.web_search.max_results,
                self.config.web_search.timeout_secs,
            )));
        }

        // HTTP request
        if self.config.http_request.enabled {
            tools.push(Arc::new(HttpRequestTool::new(
                self.security.clone(),
                self.config.http_request.allowed_domains.clone(),
                self.config.http_request.max_response_size,
                self.config.http_request.timeout_secs,
            )));
        }

        // Browser
        if self.config.browser.enabled {
            tools.push(Arc::new(BrowserOpenTool::new(
                self.security.clone(),
                self.config.browser.allowed_domains.clone(),
            )));
            tools.push(Arc::new(ScreenshotTool::new(self.security.clone())));
        }

        tools
    }
}

// ---------------------------------------------------------------------------
// Stubs for harness-specific tools (will be fully implemented later)
// ---------------------------------------------------------------------------

/// Stub: progress update tool for reporting step progress during routine execution.
///
/// TODO: restore after full integration
pub struct ProgressUpdateTool {
    _sender: Option<Arc<dyn crate::stream::StreamSender>>,
    _run_id: uuid::Uuid,
    _task_id: Option<uuid::Uuid>,
    _step_name: String,
}

impl ProgressUpdateTool {
    pub fn new(
        sender: Option<Arc<dyn crate::stream::StreamSender>>,
        run_id: uuid::Uuid,
        task_id: Option<uuid::Uuid>,
        step_name: String,
    ) -> Self {
        Self {
            _sender: sender,
            _run_id: run_id,
            _task_id: task_id,
            _step_name: step_name,
        }
    }
}

#[async_trait]
impl Tool for ProgressUpdateTool {
    fn name(&self) -> &str {
        "progress_update"
    }

    fn description(&self) -> &str {
        "Report progress on the current step (stub)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            }
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: "Progress noted.".to_string(),
            error: None,
        })
    }
}

/// Re-export DelegateToTool from the nenjo SDK (first-class implementation).
pub use nenjo::agents::delegation::DelegateToTool;

// ---------------------------------------------------------------------------
// Stub: all_tools and apply_understanding_filter
// TODO: restore after full integration — these will be replaced by
// ToolFactory-based construction
// ---------------------------------------------------------------------------

/// Build the full set of tools for an agent.
///
/// TODO: restore after full integration — parameters preserved for call-site compat
#[allow(clippy::too_many_arguments, unused_variables)]
pub fn all_tools(
    security: &Arc<SecurityPolicy>,
    namespace: &str,
    shared_namespace: Option<&str>,
    core_namespace: Option<&str>,
    delegation_ctx: Option<crate::agent::DelegationContext>,
    progress_sender: Option<()>,
    workspace_dir: &std::path::Path,
    stream_sender: Option<Arc<dyn crate::stream::StreamSender>>,
    config: &crate::config::Config,
) -> Vec<Box<dyn Tool>> {
    let runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter> = Arc::new(NativeRuntime);
    let mcp_pool = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let factory = HarnessToolFactory::new(security.clone(), runtime, config.clone(), mcp_pool);
    factory
        .base_tools()
        .into_iter()
        .map(|arc_tool| {
            // Convert Arc<dyn Tool> to Box<dyn Tool> via a wrapper
            Box::new(ArcToolWrapper(arc_tool)) as Box<dyn Tool>
        })
        .collect()
}

/// Filter tools to read-only (understanding) set.
///
/// TODO: restore after full integration — currently returns tools unchanged
pub fn apply_understanding_filter(tools: Vec<Box<dyn Tool>>) -> Vec<Box<dyn Tool>> {
    tools
}

/// Wrapper to convert Arc<dyn Tool> into Box<dyn Tool>.
struct ArcToolWrapper(Arc<dyn Tool>);

#[async_trait]
impl Tool for ArcToolWrapper {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn description(&self) -> &str {
        self.0.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.0.parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        self.0.execute(args).await
    }
}

// ---------------------------------------------------------------------------
// NativeRuntime — default RuntimeAdapter for local execution
// ---------------------------------------------------------------------------

/// Native runtime that uses local shell and filesystem.
pub struct NativeRuntime;

impl nenjo_tools::runtime::RuntimeAdapter for NativeRuntime {
    fn name(&self) -> &str {
        "native"
    }

    fn has_shell_access(&self) -> bool {
        true
    }

    fn has_filesystem_access(&self) -> bool {
        true
    }

    fn storage_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(".")
    }

    fn supports_long_running(&self) -> bool {
        true
    }

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &std::path::Path,
    ) -> Result<tokio::process::Command> {
        let shell = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        let flag = if cfg!(target_os = "windows") {
            "/C"
        } else {
            "-c"
        };
        let mut cmd = tokio::process::Command::new(shell);
        cmd.arg(flag).arg(command).current_dir(workspace_dir);
        Ok(cmd)
    }
}
