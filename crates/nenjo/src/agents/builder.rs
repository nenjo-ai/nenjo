//! Builder for creating an [`AgentRunner`] from manifest data.

use std::sync::Arc;

use nenjo_models::ModelProvider;
use nenjo_tools::Tool;
use nenjo_tools::security::SecurityPolicy;

use super::instance::AgentInstance;
use super::prompts::{PromptConfig, PromptContext};
use super::runner::AgentRunner;
use crate::config::AgentConfig;
use crate::context::ContextRenderer;
use crate::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use crate::memory::Memory;
use crate::memory::MemoryScope;
use crate::provider::{ModelProviderFactory, ToolFactory};
use crate::routines::LambdaRunner;

/// Required parameters for constructing an [`AgentBuilder`].
pub(crate) struct AgentBuilderParams {
    pub agent: AgentManifest,
    pub model: ModelManifest,
    pub provider: Arc<dyn ModelProvider>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub prompt_config: PromptConfig,
    pub prompt_context: PromptContext,
    pub agent_config: AgentConfig,
    pub context_renderer: ContextRenderer,
}

/// Builder for constructing an [`AgentRunner`].
///
/// Pre-filled by [`Provider`](crate::provider::Provider) with
/// manifest data. Callers can override individual fields before building.
pub struct AgentBuilder {
    agent: AgentManifest,
    model: ModelManifest,
    provider: Arc<dyn ModelProvider>,
    tools: Vec<Arc<dyn Tool>>,
    prompt_config: PromptConfig,
    prompt_context: PromptContext,
    agent_config: AgentConfig,
    context_renderer: ContextRenderer,
    memory_xml: String,
    memory: Option<Arc<dyn Memory>>,
    memory_scope: Option<MemoryScope>,
    // For DelegateToTool construction — set by Provider::build_agent().
    manifest: Option<Arc<Manifest>>,
    model_factory: Option<Arc<dyn ModelProviderFactory>>,
    tool_factory: Option<Arc<dyn ToolFactory>>,
    lambda_runner: Option<Arc<dyn LambdaRunner>>,
    child_delegation_ctx: Option<crate::types::DelegationContext>,
}

impl AgentBuilder {
    /// Create a new builder with required fields (called by Provider).
    pub(crate) fn new(params: AgentBuilderParams) -> Self {
        Self {
            agent: params.agent,
            model: params.model,
            provider: params.provider,
            tools: params.tools,
            prompt_config: params.prompt_config,
            prompt_context: params.prompt_context,
            agent_config: params.agent_config,
            context_renderer: params.context_renderer,
            memory_xml: String::new(),
            memory: None,
            memory_scope: None,
            manifest: None,
            model_factory: None,
            tool_factory: None,
            lambda_runner: None,
            child_delegation_ctx: None,
        }
    }

    /// Set memory backend and scope for this agent.
    ///
    /// When set, the runner will:
    /// 1. Load summaries and inject `<memory>` XML into prompts
    /// 2. Include memory_store/recall/forget tools automatically
    pub fn with_memory(mut self, memory: Arc<dyn Memory>, scope: MemoryScope) -> Self {
        // Add memory tools if not already present
        let has_memory_tools = self.tools.iter().any(|t| t.name() == "memory_store");
        if !has_memory_tools {
            let mem_tools = crate::memory::tools::memory_tools(memory.clone(), scope.clone());
            self.tools.extend(mem_tools);
        }
        self.memory = Some(memory);
        self.memory_scope = Some(scope);
        self
    }

    /// Set pre-computed memory XML for prompt injection.
    ///
    /// Use this instead of `with_memory()` if you want to manage memory
    /// retrieval yourself.
    pub fn with_memory_xml(mut self, xml: impl Into<String>) -> Self {
        self.memory_xml = xml.into();
        self
    }

    /// Add an additional tool to this agent.
    pub fn with_tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Override the agent configuration.
    pub fn with_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = config;
        self
    }

    /// Inject project context so the agent's prompts can reference
    /// `{{ project.name }}`, `{{ project.description }}`, etc.
    ///
    /// Also resolves git context from project settings if the repo is synced.
    /// `working_dir` is derived from `workspace_dir/slug` in `build_prompts()`.
    pub fn with_project_context(mut self, project: &ProjectManifest) -> Self {
        let extra = &mut self.prompt_context.render_ctx_extra;
        extra.project.id = project.id.to_string();
        extra.project.name = project.name.clone();
        extra.project.description = project.description.clone().unwrap_or_default();
        extra.project.metadata = nenjo_xml::types::metadata_json_to_xml(&project.settings);
        extra.project_slug = project.slug.clone();

        // Resolve git context from project settings if repo is synced.
        // Task-level git (worktree) overrides this in from_task().
        let sync_status = project
            .settings
            .get("repo_sync_status")
            .and_then(|v| v.as_str());
        if sync_status == Some("synced") {
            let repo_url = project
                .settings
                .get("repo_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            extra.git = crate::context::types::GitContext {
                repo_url,
                ..Default::default()
            };
        }

        self
    }

    /// Inject routine context so the agent's prompts can reference
    /// `{{ routine.name }}`, `{{ routine.id }}`, `{{ routine.execution_id }}`.
    pub fn with_routine_context(
        mut self,
        routine_id: uuid::Uuid,
        routine_name: &str,
        execution_id: &str,
    ) -> Self {
        let extra = &mut self.prompt_context.render_ctx_extra;
        extra.routine.id = routine_id;
        extra.routine.name = routine_name.to_string();
        extra.routine.execution_id = execution_id.to_string();
        self
    }

    /// Inject step metadata into the render context.
    pub fn with_step_metadata(mut self, metadata: &str) -> Self {
        self.prompt_context.render_ctx_extra.step_metadata = metadata.to_string();
        self
    }

    /// Set the Provider's factory Arcs for delegation support.
    ///
    /// Called by `Provider::build_agent()` so that the runner can construct
    /// `DelegateToTool` with the ability to look up other agents.
    pub(crate) fn with_delegation_support(
        mut self,
        manifest: Arc<Manifest>,
        model_factory: Arc<dyn ModelProviderFactory>,
        tool_factory: Arc<dyn ToolFactory>,
        lambda_runner: Option<Arc<dyn LambdaRunner>>,
    ) -> Self {
        self.manifest = Some(manifest);
        self.model_factory = Some(model_factory);
        self.tool_factory = Some(tool_factory);
        self.lambda_runner = lambda_runner;
        self
    }

    /// Set a pre-built delegation context for the sub-agent.
    ///
    /// Called by `DelegateToTool` to pass the child context so that
    /// depth decrements correctly across nested delegations.
    pub(crate) fn with_child_delegation_ctx(
        mut self,
        ctx: crate::types::DelegationContext,
    ) -> Self {
        self.child_delegation_ctx = Some(ctx);
        self
    }

    /// Build the [`AgentRunner`].
    pub fn build(self) -> Result<AgentRunner, super::error::AgentError> {
        let security = Arc::new(SecurityPolicy::default());

        let delegation_support = match (self.manifest, self.model_factory, self.tool_factory) {
            (Some(m), Some(mf), Some(tf)) => Some(super::runner::DelegationSupport {
                manifest: m,
                model_factory: mf,
                tool_factory: tf,
                memory: self.memory.clone(),
                agent_config: self.agent_config.clone(),
                lambda_runner: self.lambda_runner,
                delegation_ctx: self.child_delegation_ctx,
            }),
            _ => None,
        };

        let instance = AgentInstance {
            name: self.agent.name.clone(),
            description: self.agent.description.clone().unwrap_or_default(),
            agent_id: Some(self.agent.id),
            model: self.model.model.clone(),
            model_id: self.model.id,
            temperature: self.model.temperature.unwrap_or(0.7),
            prompt_config: self.prompt_config,
            prompt_context: self.prompt_context,
            provider: self.provider,
            tools: self.tools,
            security,
            agent_config: self.agent_config,
            context_renderer: self.context_renderer,
            memory_xml: self.memory_xml,
            documents_xml: String::new(),
        };

        AgentRunner::new(instance, self.memory, self.memory_scope, delegation_support)
    }

}
