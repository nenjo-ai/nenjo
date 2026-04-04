//! Builder for creating an [`AgentRunner`] from manifest data.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use nenjo_models::ModelProvider;
use nenjo_tools::Tool;
use nenjo_tools::security::SecurityPolicy;

use super::instance::AgentInstance;
use super::prompts::{PromptConfig, PromptContext};
use super::runner::AgentRunner;
use crate::config::AgentConfig;
use crate::context::ContextRenderer;
use crate::context::{ProjectContext, RoutineContext, RoutineStepContext};
use crate::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use crate::memory::Memory;
use crate::memory::types::MemoryScope;
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
    memory_vars: HashMap<String, String>,
    memory: Option<Arc<dyn Memory>>,
    // For DelegateToTool construction — set by Provider::build_agent().
    manifest: Option<Arc<Manifest>>,
    model_factory: Option<Arc<dyn ModelProviderFactory>>,
    tool_factory: Option<Arc<dyn ToolFactory>>,
    lambda_runner: Option<Arc<dyn LambdaRunner>>,
    platform_resolver: Option<Arc<dyn crate::mcp::PlatformToolResolver>>,
    child_delegation_ctx: Option<crate::types::DelegationContext>,
    /// When set, overrides SecurityPolicy.workspace_dir so all tools
    /// (shell, file_read, file_write, git) operate in this directory.
    work_dir: Option<PathBuf>,
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
            memory_vars: HashMap::new(),
            memory: None,
            manifest: None,
            model_factory: None,
            tool_factory: None,
            lambda_runner: None,
            platform_resolver: None,
            child_delegation_ctx: None,
            work_dir: None,
        }
    }

    /// Set memory backend for this agent.
    ///
    /// When set, the runner will:
    /// 1. Load memories and resources and inject them into prompts
    /// 2. Include memory and resource tools automatically
    ///
    /// The memory scope is derived from the agent name and project context
    /// at `build()` time, so call `with_project_context()` before `build()`
    /// to get project-scoped memories.
    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Set pre-computed memory template vars for prompt injection.
    ///
    /// Use this instead of `with_memory()` if you want to manage memory
    /// retrieval yourself. Keys should be `memories`, `memories.core`, etc.
    pub fn with_memory_vars(mut self, vars: HashMap<String, String>) -> Self {
        self.memory_vars = vars;
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
    /// Resolves git context from project settings if the repo is synced.
    /// `working_dir` is derived from `workspace_dir/slug` in `build_prompts()`.
    pub fn with_project_context(mut self, project: &ProjectManifest) -> Self {
        let ctx = ProjectContext::from_manifest(project);
        let extra = &mut self.prompt_context.render_ctx_extra;
        // Resolve git at the top level for backward compat with from_task() merge logic.
        if let Some(ref git) = ctx.git {
            extra.git = git.clone();
        }
        extra.project = ctx;
        self
    }

    /// Inject routine context so the agent's prompts can reference
    /// `{{ routine.name }}`, `{{ routine.id }}`, `{{ routine.execution_id }}`.
    pub fn with_routine_context(mut self, ctx: RoutineContext) -> Self {
        self.prompt_context.render_ctx_extra.routine = ctx;
        self
    }

    /// Inject step context so the agent's prompts can reference
    /// `{{ routine.step.name }}`, `{{ routine.step.type }}`, `{{ routine.step.metadata }}`.
    pub fn with_step_context(mut self, ctx: RoutineStepContext) -> Self {
        self.prompt_context.render_ctx_extra.routine.step = ctx;
        self
    }

    /// Scope the agent's tools to a specific working directory.
    ///
    /// When set, the `SecurityPolicy.workspace_dir` is overridden so all
    /// file and shell tools operate relative to this directory. Used to
    /// confine agents to a git worktree during task execution.
    pub fn with_work_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.work_dir = Some(dir.into());
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
        platform_resolver: Option<Arc<dyn crate::mcp::PlatformToolResolver>>,
    ) -> Self {
        self.manifest = Some(manifest);
        self.model_factory = Some(model_factory);
        self.tool_factory = Some(tool_factory);
        self.lambda_runner = lambda_runner;
        self.platform_resolver = platform_resolver;
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
    pub async fn build(mut self) -> Result<AgentRunner, super::error::AgentError> {
        let mut policy = SecurityPolicy::default();
        if let Some(dir) = &self.work_dir {
            policy.workspace_dir = dir.clone();
            // Agents running in a worktree are autonomous task executions —
            // allow all operations including git push and PR creation.
            policy.autonomy = nenjo_tools::security::AutonomyLevel::Full;
        }
        let security = Arc::new(policy);

        // When work_dir is set, rebuild tools with the scoped security policy
        // so file, shell, and git tools operate in the correct directory.
        if self.work_dir.is_some() {
            if let Some(ref tf) = self.tool_factory {
                self.tools = tf
                    .create_tools_with_security(&self.agent, security.clone())
                    .await;
            }
        }

        // Build memory scope and inject tools. This is the single place
        // where memory/resource tools are added — scope is derived from the
        // agent name and whatever project context was set via with_project_context().
        let memory_scope = if let Some(ref mem) = self.memory {
            let slug = &self.prompt_context.render_ctx_extra.project.slug;
            let project_slug = if slug.is_empty() {
                None
            } else {
                Some(slug.as_str())
            };
            let scope = MemoryScope::new(&self.agent.name, project_slug);
            self.tools.extend(crate::memory::tools::memory_tools(
                mem.clone(),
                scope.clone(),
                &self.agent.name,
            ));
            Some(scope)
        } else {
            None
        };

        let delegation_support = match (self.manifest, self.model_factory, self.tool_factory) {
            (Some(m), Some(mf), Some(tf)) => Some(super::runner::DelegationSupport {
                manifest: m,
                model_factory: mf,
                tool_factory: tf,
                memory: self.memory.clone(),
                agent_config: self.agent_config.clone(),
                lambda_runner: self.lambda_runner,
                platform_resolver: self.platform_resolver,
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
            memory_vars: self.memory_vars,
            resource_vars: HashMap::new(),
            documents_xml: String::new(),
        };

        AgentRunner::new(instance, self.memory, memory_scope, delegation_support)
    }
}
