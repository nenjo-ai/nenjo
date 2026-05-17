//! Builder for creating an [`AgentRunner`] from manifest data.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::instance::{AgentInstance, AgentModel, AgentPromptState, AgentRuntime};
use super::prompts::PromptContext;
use super::runner::AgentRunner;
use crate::agents::error::AgentError;
use crate::config::AgentConfig;
use crate::context::ContextRenderer;
use crate::context::{ProjectContext, RoutineContext, RoutineStepContext};
use crate::manifest::{AgentManifest, ModelManifest, ProjectManifest};
use crate::memory::types::MemoryScope;
use crate::provider::{ErasedProvider, ProviderRuntime, ToolContext, ToolFactory};
use crate::tools::{Tool, ToolAutonomy, ToolSecurity};

/// Required parameters for constructing an [`AgentBuilder`].
pub(crate) struct AgentBuilderParams<P: ProviderRuntime = ErasedProvider> {
    pub agent: AgentManifest,
    pub model: ModelManifest,
    pub model_provider: Arc<P::Model<'static>>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub prompt_context: PromptContext,
    pub agent_config: AgentConfig,
    pub context_renderer: ContextRenderer,
}

/// Builder for constructing an [`AgentRunner`].
///
/// Pre-filled by [`Provider`](crate::provider::Provider) with
/// manifest data. Callers can override individual fields before building.
pub struct AgentBuilder<P: ProviderRuntime = ErasedProvider> {
    agent: Option<AgentManifest>,
    model: Option<ModelManifest>,
    model_provider: Option<Arc<P::Model<'static>>>,
    tools: Vec<Arc<dyn Tool>>,
    prompt_context: Option<PromptContext>,
    agent_config: AgentConfig,
    context_renderer: ContextRenderer,
    memory_vars: HashMap<String, String>,
    memory: Option<Arc<P::Memory<'static>>>,
    memory_scope_override: Option<MemoryScope>,
    pending_project_context: Option<ProjectManifest>,
    pending_routine_context: Option<RoutineContext>,
    pending_step_context: Option<RoutineStepContext>,
    // For DelegateToTool construction, set when a provider creates the builder.
    provider_runtime: Option<P>,
    child_delegation_ctx: Option<crate::types::DelegationContext>,
    /// When set, overrides SecurityPolicy.workspace_dir so all tools
    /// (shell, file_read, file_write, git) operate in this directory.
    work_dir: Option<PathBuf>,
}

impl<P: ProviderRuntime> AgentBuilder<P> {
    /// Create a new builder with required fields (called by Provider).
    pub(crate) fn new(params: AgentBuilderParams<P>) -> Self {
        Self {
            agent: Some(params.agent),
            model: Some(params.model),
            model_provider: Some(params.model_provider),
            tools: params.tools,
            prompt_context: Some(params.prompt_context),
            agent_config: params.agent_config,
            context_renderer: params.context_renderer,
            memory_vars: HashMap::new(),
            memory: None,
            memory_scope_override: None,
            pending_project_context: None,
            pending_routine_context: None,
            pending_step_context: None,
            provider_runtime: None,
            child_delegation_ctx: None,
            work_dir: None,
        }
    }

    /// Create a blank builder backed by a Provider.
    pub(crate) fn blank(
        provider: P,
        agent_config: AgentConfig,
        context_renderer: ContextRenderer,
    ) -> Self {
        Self {
            agent: None,
            model: None,
            model_provider: None,
            tools: Vec::new(),
            prompt_context: None,
            agent_config,
            context_renderer,
            memory_vars: HashMap::new(),
            memory: None,
            memory_scope_override: None,
            pending_project_context: None,
            pending_routine_context: None,
            pending_step_context: None,
            provider_runtime: Some(provider),
            child_delegation_ctx: None,
            work_dir: None,
        }
    }

    /// Set the agent manifest for this builder.
    pub fn with_agent_manifest(mut self, agent: AgentManifest) -> Self {
        self.prompt_context = self
            .provider_runtime
            .as_ref()
            .map(|provider| provider.build_prompt_context(&agent));
        self.agent = Some(agent);
        self
    }

    /// Set the model manifest for this builder.
    pub fn with_model_manifest(mut self, model: ModelManifest) -> Self {
        self.model = Some(model);
        self
    }

    /// Set both the model manifest and the concrete model provider to use.
    pub fn with_model_provider(
        mut self,
        model: ModelManifest,
        provider: Arc<P::Model<'static>>,
    ) -> Self {
        self.model = Some(model);
        self.model_provider = Some(provider);
        self
    }

    /// Set memory backend for this agent.
    ///
    /// When set, the runner will:
    /// 1. Load memories and artifacts and inject them into prompts
    /// 2. Include memory and artifact tools automatically
    ///
    /// The memory scope is derived from the agent name and project context
    /// at `build()` time, so call `with_project_context()` before `build()`
    /// to get project-scoped memories.
    pub fn with_memory(mut self, memory: Arc<P::Memory<'static>>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Override the resolved memory scope for this agent.
    ///
    /// Use this when the caller has already resolved the exact namespace
    /// mapping to apply, such as restoring a persisted session.
    pub fn with_memory_scope(mut self, scope: MemoryScope) -> Self {
        self.memory_scope_override = Some(scope);
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

    /// Override only the maximum number of LLM/tool-call turns.
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.agent_config.max_turns = max_turns;
        self
    }

    /// Inject project context so the agent's prompts can reference
    /// `{{ project.name }}`, `{{ project.description }}`, etc.
    ///
    /// Resolves git context from project settings if the repo is synced.
    /// `working_dir` is derived from `workspace_dir/slug` in `build_prompts()`.
    pub fn with_project_context(mut self, project: &ProjectManifest) -> Self {
        self.pending_project_context = Some(project.clone());
        self
    }

    /// Inject routine context so the agent's prompts can reference
    /// `{{ routine.name }}`, `{{ routine.id }}`, `{{ routine.execution_id }}`.
    pub fn with_routine_context(mut self, ctx: RoutineContext) -> Self {
        self.pending_routine_context = Some(ctx);
        self
    }

    /// Inject step context so the agent's prompts can reference
    /// `{{ routine.step.name }}`, `{{ routine.step.type }}`,
    /// `{{ routine.step.instructions }}`, and `{{ routine.step.metadata }}`.
    pub fn with_step_context(mut self, ctx: RoutineStepContext) -> Self {
        self.pending_step_context = Some(ctx);
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

    /// Set the Provider handle for delegation support.
    ///
    /// Attach the provider runtime so the runner can construct `DelegateToTool`
    /// with the ability to look up other agents.
    pub(crate) fn with_delegation_support(mut self, provider: P) -> Self {
        self.provider_runtime = Some(provider);
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
    pub async fn build(mut self) -> Result<AgentRunner<P>, super::error::AgentError> {
        let agent = self.agent.take().ok_or(AgentError::MissingAgentManifest)?;
        let model = self.model.take().ok_or(AgentError::MissingModelManifest)?;
        let model_provider = match self.model_provider.take() {
            Some(provider) => provider,
            None => {
                let provider = self
                    .provider_runtime
                    .as_ref()
                    .ok_or(AgentError::MissingModelProvider)?;
                provider.create_model_provider(&model).await?
            }
        };
        let mut prompt_context = match self.prompt_context.take() {
            Some(prompt_context) => prompt_context,
            None => {
                let provider = self
                    .provider_runtime
                    .as_ref()
                    .ok_or(AgentError::MissingAgentManifest)?;
                provider.build_prompt_context(&agent)
            }
        };
        if let Some(project) = self.pending_project_context.take() {
            let ctx = ProjectContext::from_manifest(&project);
            let extra = &mut prompt_context.render_ctx_extra;
            // Resolve git at the top level for prompt context defaults.
            if let Some(ref git) = ctx.git {
                extra.git = git.clone();
            }
            extra.project = ctx;
            prompt_context.current_project = project;
        }
        if let Some(ctx) = self.pending_routine_context.take() {
            prompt_context.render_ctx_extra.routine = ctx;
        }
        if let Some(ctx) = self.pending_step_context.take() {
            prompt_context.render_ctx_extra.routine.step = ctx;
        }

        let mut policy = match &self.provider_runtime {
            Some(provider) => {
                ToolSecurity::with_workspace_dir(provider.tool_factory().workspace_dir())
            }
            None => ToolSecurity::default(),
        };
        if let Some(dir) = &self.work_dir {
            policy.workspace_dir = dir.clone();
            // Agents running in a worktree are autonomous task executions —
            // allow all operations including git push and PR creation.
            policy.autonomy = ToolAutonomy::Full;
        }
        let security = Arc::new(policy);

        if let Some(ref provider) = self.provider_runtime {
            let project_slug = active_project_slug(&prompt_context);
            let mut provider_tools = provider
                .tool_factory()
                .create_tools_with_context(
                    &agent,
                    security.clone(),
                    ToolContext {
                        project_slug: project_slug.map(str::to_string),
                    },
                )
                .await;
            provider_tools.extend(provider.create_knowledge_tools());
            provider_tools.extend(self.tools);
            self.tools = provider_tools;
        }

        // Build memory scope and inject tools. This is the single place
        // where memory/artifact tools are added — scope is derived from the
        // agent name and whatever project context was set via with_project_context().
        let memory_scope = if let Some(ref mem) = self.memory {
            let scope = if let Some(scope) = self.memory_scope_override.clone() {
                scope
            } else {
                MemoryScope::new(&agent.name, active_project_slug(&prompt_context))
            };
            self.tools.extend(crate::memory::tools::memory_tools(
                mem.clone(),
                scope.clone(),
                &agent.name,
            ));
            Some(scope)
        } else {
            None
        };

        let provider_runtime = self.provider_runtime.clone();
        let delegation_support =
            self.provider_runtime
                .map(|provider| super::runner::DelegationSupport {
                    provider,
                    max_delegation_depth: self.agent_config.max_delegation_depth,
                    delegation_ctx: self.child_delegation_ctx,
                });

        let instance = AgentInstance {
            manifest: agent,
            model: AgentModel {
                model_name: model.model.clone(),
                id: model.id,
                temperature: model.temperature.unwrap_or(0.7),
                model_provider,
            },
            prompt: AgentPromptState {
                context: prompt_context,
                renderer: self.context_renderer,
                memory_vars: self.memory_vars,
                artifact_vars: HashMap::new(),
            },
            runtime: AgentRuntime {
                tools: self.tools,
                security,
                config: self.agent_config,
                provider_runtime,
            },
        };

        AgentRunner::new(instance, self.memory, memory_scope, delegation_support).await
    }
}

fn active_project_slug(prompt_context: &PromptContext) -> Option<&str> {
    let slug = if prompt_context.render_ctx_extra.project.slug.is_empty() {
        &prompt_context.current_project.slug
    } else {
        &prompt_context.render_ctx_extra.project.slug
    };

    if slug.is_empty() {
        None
    } else {
        Some(slug.as_str())
    }
}
