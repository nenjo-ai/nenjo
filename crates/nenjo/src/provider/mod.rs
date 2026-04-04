//! Provider — the root object for the Nenjo SDK.
//!
//! Holds the bootstrap manifest, LLM provider factory, and tool factory.
//! Builds agents via [`Provider::agent_by_id`] or [`Provider::agent_by_name`].

pub mod builder;
pub mod error;

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

pub use builder::ProviderBuilder;
pub use error::ProviderError;

use nenjo_models::ModelProvider;
use nenjo_tools::Tool;

use crate::agents::builder::AgentBuilder;
use crate::agents::prompts::{self as prompts, PromptContext};
use crate::config::AgentConfig;
use crate::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use crate::memory::Memory;
use crate::routines::{
    self, LambdaRunner, RoutineEvent, RoutineExecutionHandle, types::StepResult,
};
use crate::types::RenderContextVars;
use tokio::sync::mpsc;
use tracing::{debug, error};

// ---------------------------------------------------------------------------
// Factory traits
// ---------------------------------------------------------------------------

/// Maps a `model_provider` string (e.g. "openai", "anthropic") to an LLM provider.
///
/// Implementations are responsible for API key resolution.
pub trait ModelProviderFactory: Send + Sync {
    fn create(&self, provider_name: &str) -> Result<Arc<dyn ModelProvider>>;
}

/// Creates tools for an agent based on its bootstrap configuration.
///
/// Implementations use the agent's `platform_scopes`, `skills`, `abilities`,
/// and `mcp_server_ids` to decide which tools to provide.
#[async_trait::async_trait]
pub trait ToolFactory: Send + Sync {
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>>;
}

/// A no-op tool factory that returns an empty tool set.
pub struct NoopToolFactory;

#[async_trait::async_trait]
impl ToolFactory for NoopToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// The root object for the Nenjo SDK.
///
/// Created via [`ProviderBuilder`]. Holds the bootstrap manifest and
/// factory functions. Use [`agent_by_id`](Self::agent_by_id) or
/// [`agent_by_name`](Self::agent_by_name) to create agent runners.
#[derive(Clone)]
pub struct Provider {
    manifest: Arc<Manifest>,
    model_factory: Arc<dyn ModelProviderFactory>,
    tool_factory: Arc<dyn ToolFactory>,
    memory: Option<Arc<dyn Memory>>,
    agent_config: AgentConfig,
    lambda_runner: Option<Arc<dyn LambdaRunner>>,
    platform_resolver: Option<Arc<dyn crate::mcp::PlatformToolResolver>>,
}

impl Provider {
    /// Start building a Provider.
    pub fn builder() -> ProviderBuilder {
        ProviderBuilder::new()
    }

    /// Create a Provider from raw Arc fields (used internally by DelegateToTool).
    pub(crate) fn from_manifest_raw(
        manifest: Arc<Manifest>,
        model_factory: Arc<dyn ModelProviderFactory>,
        tool_factory: Arc<dyn ToolFactory>,
        memory: Option<Arc<dyn Memory>>,
        agent_config: AgentConfig,
        lambda_runner: Option<Arc<dyn LambdaRunner>>,
        platform_resolver: Option<Arc<dyn crate::mcp::PlatformToolResolver>>,
    ) -> Self {
        Self {
            manifest,
            model_factory,
            tool_factory,
            memory,
            agent_config,
            lambda_runner,
            platform_resolver,
        }
    }

    /// Get an agent builder by agent ID.
    pub async fn agent_by_id(&self, id: Uuid) -> Result<AgentBuilder, ProviderError> {
        let agent = self
            .manifest
            .agents
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| ProviderError::AgentNotFound(id.to_string()))?;

        self.build_agent(agent).await
    }

    /// Get an agent builder by agent name.
    pub async fn agent_by_name(&self, name: &str) -> Result<AgentBuilder, ProviderError> {
        let agent = self
            .manifest
            .agents
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| ProviderError::AgentNotFound(name.to_string()))?;

        self.build_agent(agent).await
    }

    /// Access the bootstrap manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Get a clone of the manifest Arc (for mutation + rebuild).
    pub fn manifest_arc(&self) -> Arc<Manifest> {
        self.manifest.clone()
    }

    /// Create a new Provider with the given manifest but same factories/memory/config.
    ///
    /// Used by the harness to hot-swap bootstrap data without rebuilding factories.
    pub fn with_manifest(&self, manifest: Manifest) -> Self {
        Self {
            manifest: Arc::new(manifest),
            model_factory: self.model_factory.clone(),
            tool_factory: self.tool_factory.clone(),
            memory: self.memory.clone(),
            agent_config: self.agent_config.clone(),
            lambda_runner: self.lambda_runner.clone(),
            platform_resolver: self.platform_resolver.clone(),
        }
    }

    /// Access the memory backend, if configured.
    pub fn memory(&self) -> Option<&Arc<dyn Memory>> {
        self.memory.as_ref()
    }

    /// Access the agent config.
    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent_config
    }

    /// Access the tool factory.
    pub fn tool_factory(&self) -> &Arc<dyn ToolFactory> {
        &self.tool_factory
    }

    /// Access the lambda runner, if configured.
    pub fn lambda_runner(&self) -> Option<&Arc<dyn LambdaRunner>> {
        self.lambda_runner.as_ref()
    }

    /// Access the platform tool resolver, if configured.
    pub fn platform_resolver(&self) -> Option<&Arc<dyn crate::mcp::PlatformToolResolver>> {
        self.platform_resolver.as_ref()
    }

    // -----------------------------------------------------------------------
    // Routine execution
    // -----------------------------------------------------------------------

    /// Look up a routine by ID and return a builder for configuring execution.
    ///
    /// ```ignore
    /// let result = provider.routine_by_id(id)?
    ///     .run(TaskType::Task(task))
    ///     .await?;
    /// ```
    pub fn routine_by_id(&self, routine_id: Uuid) -> Result<RoutineRunner, ProviderError> {
        let routine = self
            .manifest
            .routines
            .iter()
            .find(|r| r.id == routine_id)
            .ok_or_else(|| ProviderError::RoutineNotFound(routine_id.to_string()))?
            .clone();

        Ok(RoutineRunner {
            provider: self.clone(),
            routine,
        })
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    async fn build_agent(&self, agent: &AgentManifest) -> Result<AgentBuilder, ProviderError> {
        let model = self.resolve_model(agent)?;

        let provider = self
            .model_factory
            .create(&model.model_provider)
            .map_err(|e| {
                ProviderError::FactoryFailed(e.context(format!(
                    "failed to create LLM provider '{}' for agent '{}'",
                    model.model_provider, agent.name
                )))
            })?;

        let tools = self.tool_factory.create_tools(agent).await;

        // Memory backend is passed to the builder; scope and tools are
        // constructed in build() based on the project context set at that point.

        let prompt_config: crate::agents::prompts::PromptConfig =
            match serde_json::from_value::<crate::agents::prompts::PromptConfig>(
                agent.prompt_config.clone(),
            ) {
                Ok(config) => {
                    debug!(
                        agent = %agent.name,
                        system_prompt_len = config.system_prompt.len(),
                        cron_task_len = config.templates.cron_task.len(),
                        task_execution_len = config.templates.task_execution.len(),
                        "Parsed prompt_config"
                    );
                    config
                }
                Err(e) => {
                    error!(
                        agent = %agent.name,
                        error = %e,
                        raw = %agent.prompt_config,
                        "Failed to parse prompt_config, using defaults"
                    );
                    Default::default()
                }
            };

        let agent_config = self.agent_config.clone();
        let prompt_context = self.build_prompt_context(agent);

        let context_renderer = {
            let render_blocks: Vec<_> = self
                .manifest
                .context_blocks
                .iter()
                .map(prompts::render_context_block)
                .collect();
            crate::context::ContextRenderer::from_blocks(&render_blocks)
        };

        let mut builder = AgentBuilder::new(super::agents::builder::AgentBuilderParams {
            agent: agent.clone(),
            model,
            provider,
            tools,
            prompt_config,
            prompt_context,
            agent_config,
            context_renderer,
        });

        if let Some(ref mem) = self.memory {
            builder = builder.with_memory(mem.clone());
        }

        // Enable delegation support so the runner can inject DelegateToTool.
        builder = builder.with_delegation_support(
            self.manifest.clone(),
            self.model_factory.clone(),
            self.tool_factory.clone(),
            self.lambda_runner.clone(),
            self.platform_resolver.clone(),
        );

        Ok(builder)
    }

    fn resolve_model(&self, agent: &AgentManifest) -> Result<ModelManifest, ProviderError> {
        let model_id = agent.model_id.ok_or_else(|| {
            ProviderError::ModelNotFound(format!("agent '{}' has no model assigned", agent.name))
        })?;

        self.manifest
            .models
            .iter()
            .find(|m| m.id == model_id)
            .cloned()
            .ok_or_else(|| {
                ProviderError::ModelNotFound(format!(
                    "model {model_id} not found (agent '{}')",
                    agent.name
                ))
            })
    }

    fn build_prompt_context(&self, agent: &AgentManifest) -> PromptContext {
        let skills: Vec<_> = self
            .manifest
            .skills
            .iter()
            .filter(|s| agent.skills.contains(&s.id))
            .cloned()
            .collect();

        let abilities: Vec<_> = self
            .manifest
            .abilities
            .iter()
            .filter(|a| agent.abilities.contains(&a.id))
            .cloned()
            .collect();

        let domains: Vec<_> = self
            .manifest
            .domains
            .iter()
            .filter(|d| agent.domains.contains(&d.id))
            .cloned()
            .collect();

        let mcp_server_info: Vec<(String, String)> = self
            .manifest
            .mcp_servers
            .iter()
            .filter(|s| agent.mcp_server_ids.contains(&s.id))
            .map(|s| {
                (
                    s.display_name.clone(),
                    s.description.clone().unwrap_or_default(),
                )
            })
            .collect();

        let current_project =
            self.manifest
                .projects
                .first()
                .cloned()
                .unwrap_or_else(|| ProjectManifest {
                    id: Uuid::nil(),
                    name: String::new(),
                    slug: String::new(),
                    description: None,
                    is_system: false,
                    settings: serde_json::Value::Null,
                });

        PromptContext {
            agent_name: agent.name.clone(),
            agent_description: agent.description.clone().unwrap_or_default(),
            available_agents: self.manifest.agents.clone(),
            available_routines: self.manifest.routines.clone(),
            current_project,
            skills,
            available_abilities: abilities,
            available_domains: domains,
            mcp_server_info,
            platform_scopes: agent.platform_scopes.clone(),
            active_domain: None,
            docs_base_dir: None,
            render_ctx_extra: RenderContextVars::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// RoutineRunner — symmetric API for routine execution
// ---------------------------------------------------------------------------

use crate::manifest::RoutineManifest;

/// A routine resolved from the manifest, ready to execute.
///
/// Created via [`Provider::routine_by_id`]. Provides the same simple/streaming
/// split as [`AgentRunner`](crate::AgentRunner).
///
/// ```ignore
/// let result = provider.routine_by_id(id)?
///     .run(TaskType::Task(task))
///     .await?;
/// ```
pub struct RoutineRunner {
    provider: Provider,
    routine: RoutineManifest,
}

impl std::fmt::Debug for RoutineRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutineRunner")
            .field("routine_id", &self.routine.id)
            .field("routine_name", &self.routine.name)
            .finish()
    }
}

impl RoutineRunner {
    /// The routine's name.
    pub fn name(&self) -> &str {
        &self.routine.name
    }

    /// The routine's ID.
    pub fn id(&self) -> Uuid {
        self.routine.id
    }

    /// Run the routine to completion and return the final result.
    pub async fn run(&self, task: crate::types::TaskType) -> Result<StepResult> {
        self.run_stream(task).await?.output().await
    }

    /// Run the routine with streaming events.
    pub async fn run_stream(&self, task: crate::types::TaskType) -> Result<RoutineExecutionHandle> {
        self.provider.run_routine_inner(&self.routine, task).await
    }
}

impl Provider {
    /// Internal: run a routine from an already-resolved manifest entry.
    async fn run_routine_inner(
        &self,
        routine: &RoutineManifest,
        task: crate::types::TaskType,
    ) -> Result<RoutineExecutionHandle> {
        let routine = routine.clone();
        let max_retries = routine.max_retries;
        let routine_name = routine.name.clone();
        let routine_id = routine.id;
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_inner = cancel.clone();

        let cron_schedule = match &task {
            crate::types::TaskType::Cron {
                interval, timeout, ..
            } => Some((*interval, *timeout)),
            _ => None,
        };

        let (events_tx, events_rx) = mpsc::unbounded_channel::<RoutineEvent>();

        let input = routines::types::routine_input_from_task(&task);
        tracing::debug!(
            is_cron = input.is_cron_trigger,
            "RoutineInput built from task"
        );

        let provider = self.clone();

        let join = tokio::spawn(async move {
            let mut state = routines::types::RoutineState::new(routine_id, input, max_retries);
            state.routine_name = Some(routine_name);

            if let Some((interval, timeout)) = cron_schedule {
                routines::cron::executor::execute_routine_cron(
                    &provider,
                    &routine,
                    &mut state,
                    &events_tx,
                    &cancel_inner,
                    interval,
                    timeout,
                )
                .await
            } else {
                routines::executor::execute_routine(
                    &provider,
                    &routine,
                    &mut state,
                    &events_tx,
                    &cancel_inner,
                )
                .await
            }
        });

        Ok(RoutineExecutionHandle::new(events_rx, join, cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ContextBlockManifest, ManifestLoader};

    struct MockProvider;

    #[async_trait::async_trait]
    impl nenjo_models::ModelProvider for MockProvider {
        async fn chat(
            &self,
            _request: nenjo_models::ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> Result<nenjo_models::ChatResponse> {
            Ok(nenjo_models::ChatResponse {
                text: Some("mock".into()),
                tool_calls: vec![],
                usage: nenjo_models::TokenUsage::default(),
            })
        }
    }

    struct MockFactory;

    impl ModelProviderFactory for MockFactory {
        fn create(&self, _name: &str) -> Result<Arc<dyn nenjo_models::ModelProvider>> {
            Ok(Arc::new(MockProvider))
        }
    }

    struct StaticLoader(Manifest);

    #[async_trait::async_trait]
    impl ManifestLoader for StaticLoader {
        async fn load(&self) -> Result<Manifest> {
            Ok(self.0.clone())
        }
    }

    fn test_manifest() -> Manifest {
        let model = ModelManifest {
            id: Uuid::new_v4(),
            name: "m".into(),
            description: None,
            model: "mock".into(),
            model_provider: "mock".into(),
            temperature: Some(0.5),
            tags: vec![],
        };
        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "agent".into(),
            description: Some("test".into()),
            is_system: false,
            prompt_config: serde_json::json!({}),
            color: None,
            model_id: Some(model.id),
            model_name: None,
            skills: vec![],
            domains: vec![],
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            abilities: vec![],
        };
        Manifest {
            agents: vec![agent],
            models: vec![model],
            projects: vec![ProjectManifest {
                id: Uuid::new_v4(),
                name: "p".into(),
                slug: "p".into(),
                description: None,
                is_system: false,
                settings: serde_json::Value::Null,
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn from_manifest_and_agent_lookup() {
        let manifest = test_manifest();
        let name = manifest.agents[0].name.clone();
        let id = manifest.agents[0].id;

        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(MockFactory)
            .with_tool_factory(NoopToolFactory)
            .build()
            .await
            .unwrap();

        assert!(provider.agent_by_name(&name).await.is_ok());
        assert!(provider.agent_by_id(id).await.is_ok());
        assert!(provider.agent_by_name("missing").await.is_err());
    }

    #[tokio::test]
    async fn builder_via_loader() {
        let manifest = test_manifest();
        let name = manifest.agents[0].name.clone();

        let provider = Provider::builder()
            .with_loader(StaticLoader(manifest))
            .with_model_factory(MockFactory)
            .build()
            .await
            .unwrap();

        assert!(provider.agent_by_name(&name).await.is_ok());
    }

    #[tokio::test]
    async fn multiple_loaders_merge() {
        let manifest = test_manifest();

        let local = Manifest {
            context_blocks: vec![ContextBlockManifest {
                id: Uuid::new_v4(),
                name: "local_block".into(),
                path: "local".into(),
                display_name: None,
                description: None,
                template: "local content".into(),
                is_system: false,
            }],
            ..Default::default()
        };

        let provider = Provider::builder()
            .with_loader(StaticLoader(manifest))
            .with_loader(StaticLoader(local))
            .with_model_factory(MockFactory)
            .build()
            .await
            .unwrap();

        assert_eq!(provider.manifest().agents.len(), 1);
        assert!(
            provider
                .manifest()
                .context_blocks
                .iter()
                .any(|b| b.name == "local_block")
        );
    }

    #[tokio::test]
    async fn builder_fails_without_loader() {
        let result = Provider::builder()
            .with_model_factory(MockFactory)
            .build()
            .await;

        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("loader"));
    }

    #[tokio::test]
    async fn builder_fails_without_model_factory() {
        let result = Provider::builder()
            .with_loader(StaticLoader(test_manifest()))
            .build()
            .await;

        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("model_factory"));
    }

    #[tokio::test]
    async fn agent_without_model_fails() {
        let mut manifest = test_manifest();
        manifest.agents[0].model_id = None;

        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(MockFactory)
            .with_tool_factory(NoopToolFactory)
            .build()
            .await
            .unwrap();
        assert!(provider.agent_by_name("agent").await.is_err());
    }
}
