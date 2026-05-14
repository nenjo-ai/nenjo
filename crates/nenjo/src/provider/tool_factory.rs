use std::sync::Arc;

use crate::manifest::AgentManifest;
use crate::tools::{Tool, ToolSecurity};

/// Creates tools for an agent based on its bootstrap configuration.
///
/// Implementations use the agent's `platform_scopes`, `abilities`,
/// and `mcp_server_ids` to decide which tools to provide.
#[async_trait::async_trait]
pub trait ToolFactory: Send + Sync {
    /// Create tools available to the given agent.
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>>;

    /// Create tools with a custom security policy (e.g. scoped to a worktree).
    /// Default implementation delegates to `create_tools` (ignores the override).
    async fn create_tools_with_security(
        &self,
        agent: &AgentManifest,
        _security: Arc<ToolSecurity>,
    ) -> Vec<Arc<dyn Tool>> {
        self.create_tools(agent).await
    }

    /// Create tools with execution context such as the active project.
    ///
    /// Default implementation delegates to `create_tools_with_security`.
    async fn create_tools_with_context(
        &self,
        agent: &AgentManifest,
        security: Arc<ToolSecurity>,
        context: ToolContext,
    ) -> Vec<Arc<dyn Tool>> {
        let _ = context;
        self.create_tools_with_security(agent, security).await
    }

    /// The base workspace directory used by this factory's security policy.
    ///
    /// Used by the agent builder to set the correct `SecurityPolicy.workspace_dir`
    /// so template variables like `{{ project.working_dir }}` resolve correctly
    /// even when no git worktree is set.
    fn workspace_dir(&self) -> std::path::PathBuf {
        ToolSecurity::default().workspace_dir
    }
}

/// Runtime context available while constructing an agent's tools.
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    /// Slug for the active project, when the agent is running in a project.
    pub project_slug: Option<String>,
}

/// A no-op tool factory that returns an empty tool set.
pub struct NoopToolFactory;

#[async_trait::async_trait]
impl ToolFactory for NoopToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }
}

#[async_trait::async_trait]
impl<T> ToolFactory for Arc<T>
where
    T: ToolFactory + ?Sized,
{
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        self.as_ref().create_tools(agent).await
    }

    async fn create_tools_with_security(
        &self,
        agent: &AgentManifest,
        security: Arc<ToolSecurity>,
    ) -> Vec<Arc<dyn Tool>> {
        self.as_ref()
            .create_tools_with_security(agent, security)
            .await
    }

    async fn create_tools_with_context(
        &self,
        agent: &AgentManifest,
        security: Arc<ToolSecurity>,
        context: ToolContext,
    ) -> Vec<Arc<dyn Tool>> {
        self.as_ref()
            .create_tools_with_context(agent, security, context)
            .await
    }

    fn workspace_dir(&self) -> std::path::PathBuf {
        self.as_ref().workspace_dir()
    }
}
