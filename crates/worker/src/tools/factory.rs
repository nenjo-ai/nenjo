use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use nenjo::manifest::AgentManifest;
use nenjo::{ToolAutonomy, ToolContext, ToolFactory, ToolSecurity};
use nenjo_platform::{
    ManifestAccessPolicy, ManifestMcpBackend,
    tools::{add_manifest_tools, add_project_rest_tools},
};

use crate::config::Config;
use crate::external_mcp::ExternalMcpPool;

use super::platform_services::PlatformToolServices;
use super::{
    AutonomyLevel, BrowserOpenTool, ContentSearchTool, FileDeleteTool, FileEditTool, FileReadTool,
    FileWriteTool, GitOperationsTool, GlobSearchTool, HttpRequestTool, RuntimeAdapter,
    ScreenshotTool, SecurityPolicy, ShellTool, Tool, WebFetchTool, WebSearchTool,
};

/// A tool factory that builds per-agent tool sets for the worker runtime.
///
/// Uses the agent's configuration, security policy, MCP server pool, and
/// manifest backend to build a complete tool set per agent.
pub struct WorkerToolFactory<R>
where
    R: RuntimeAdapter,
{
    security: Arc<SecurityPolicy>,
    runtime: Arc<R>,
    config: Config,
    external_mcp: Arc<ExternalMcpPool>,
    platform: PlatformToolServices,
}

impl<R> WorkerToolFactory<R>
where
    R: RuntimeAdapter + 'static,
{
    pub(crate) fn new(
        security: impl Into<Arc<SecurityPolicy>>,
        runtime: R,
        config: Config,
        platform: PlatformToolServices,
        external_mcp: Arc<ExternalMcpPool>,
    ) -> Self {
        let security = security.into();
        let runtime = Arc::new(runtime);
        Self {
            security,
            runtime,
            config,
            external_mcp,
            platform,
        }
    }

    /// Build the base tool set (always included).
    pub fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.base_tools_with(&self.security)
    }

    /// Build the base tool set with a given security policy.
    fn base_tools_with(&self, security: &Arc<SecurityPolicy>) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(ShellTool::new(security.clone(), self.runtime.clone())),
            Arc::new(FileReadTool::new(security.clone())),
            Arc::new(FileWriteTool::new(security.clone())),
            Arc::new(FileEditTool::new(security.clone())),
            Arc::new(FileDeleteTool::new(security.clone())),
            Arc::new(GitOperationsTool::new(security.clone())),
            Arc::new(ContentSearchTool::new(security.clone())),
            Arc::new(GlobSearchTool::new(security.clone())),
        ]
    }

    /// Build all tools for an agent with a given security policy.
    async fn build_tools(
        &self,
        agent: &AgentManifest,
        security: &Arc<SecurityPolicy>,
        tool_context: ToolContext,
    ) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base_tools_with(security);

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

        let policy = ManifestAccessPolicy::new(agent.platform_scopes.clone());

        let manifest_backend = self.platform.manifest_backend.as_ref().map(|backend| {
            Arc::new(
                backend
                    .as_ref()
                    .clone()
                    .with_access_policy(policy.clone())
                    .with_current_library_slug(tool_context.project_slug.clone()),
            ) as Arc<dyn ManifestMcpBackend>
        });

        if let Some(backend) = manifest_backend.as_ref() {
            add_manifest_tools(&mut tools, backend.clone(), &policy);
        }

        let project_backend = self.platform.project_backend.clone();
        add_project_rest_tools(&mut tools, manifest_backend, project_backend, &policy);

        // Web fetch (always included with config, deny-by-default via allowed_hosts)
        if self.config.web_fetch.enabled {
            tools.push(Arc::new(WebFetchTool::new(
                security.clone(),
                self.config.web_fetch.allowed_hosts.clone(),
                self.config.web_fetch.blocked_hosts.clone(),
                self.config.web.allow_private_hosts,
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
                security.clone(),
                self.config.http_request.allowed_hosts.clone(),
                self.config.web.allow_private_hosts,
                self.config.http_request.max_response_size,
                self.config.http_request.timeout_secs,
            )));
        }

        // Browser
        if self.config.browser.enabled {
            tools.push(Arc::new(BrowserOpenTool::new(
                security.clone(),
                self.config.browser.allowed_hosts.clone(),
                self.config.web.allow_private_hosts,
            )));
            tools.push(Arc::new(ScreenshotTool::new(security.clone())));
        }

        tools
    }
}

#[async_trait]
impl<R> ToolFactory for WorkerToolFactory<R>
where
    R: RuntimeAdapter + 'static,
{
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        self.build_tools(agent, &self.security, ToolContext::default())
            .await
    }

    async fn create_tools_with_security(
        &self,
        agent: &AgentManifest,
        security: Arc<ToolSecurity>,
    ) -> Vec<Arc<dyn Tool>> {
        let security = Arc::new(security_policy_from_sdk(&security));
        self.build_tools(agent, &security, ToolContext::default())
            .await
    }

    async fn create_tools_with_context(
        &self,
        agent: &AgentManifest,
        security: Arc<ToolSecurity>,
        context: ToolContext,
    ) -> Vec<Arc<dyn Tool>> {
        let security = Arc::new(security_policy_from_sdk(&security));
        self.build_tools(agent, &security, context).await
    }

    fn workspace_dir(&self) -> PathBuf {
        self.security.workspace_dir.clone()
    }
}

fn security_policy_from_sdk(policy: &ToolSecurity) -> SecurityPolicy {
    let mut security = SecurityPolicy::with_workspace_dir(policy.workspace_dir.clone());
    security.autonomy = match policy.autonomy {
        ToolAutonomy::ReadOnly => AutonomyLevel::ReadOnly,
        ToolAutonomy::Supervised => AutonomyLevel::Supervised,
        ToolAutonomy::Full => AutonomyLevel::Full,
    };
    for name in &policy.forwarded_env_names {
        if let Ok(value) = std::env::var(name)
            && !security
                .forwarded_env
                .iter()
                .any(|(existing, _)| existing == name)
        {
            security.forwarded_env.push((name.clone(), value));
        }
    }
    security
}
