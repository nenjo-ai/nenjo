//! Provider — the root object for the Nenjo SDK.
//!
//! Holds the bootstrap manifest, LLM provider factory, and tool factory.
//! Builds agents via [`Provider::agent_by_id`] or [`Provider::agent_by_name`].

pub mod builder;
pub mod error;

use std::collections::{HashMap, HashSet};
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
use crate::context::ContextRenderer;
use crate::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use crate::memory::Memory;
use crate::routines::{self, RoutineEvent, RoutineExecutionHandle, types::StepResult};
use crate::types::RenderContextVars;
use tokio::sync::mpsc;
use tracing::{debug, trace};

// ---------------------------------------------------------------------------
// Factory traits
// ---------------------------------------------------------------------------

/// Maps a `model_provider` string (e.g. "openai", "anthropic") to an LLM provider.
///
/// Implementations are responsible for API key resolution.
pub trait ModelProviderFactory: Send + Sync {
    fn create(&self, provider_name: &str) -> Result<Arc<dyn ModelProvider>>;

    /// Create a provider with an optional base URL override.
    ///
    /// Used for self-hosted providers like Ollama where the user configures
    /// a custom endpoint. The default implementation ignores the URL.
    fn create_with_base_url(
        &self,
        provider_name: &str,
        base_url: Option<&str>,
    ) -> Result<Arc<dyn ModelProvider>> {
        let _ = base_url;
        self.create(provider_name)
    }
}

/// Creates tools for an agent based on its bootstrap configuration.
///
/// Implementations use the agent's `platform_scopes`, `abilities`,
/// and `mcp_server_ids` to decide which tools to provide.
#[async_trait::async_trait]
pub trait ToolFactory: Send + Sync {
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>>;

    /// Create tools with a custom security policy (e.g. scoped to a worktree).
    /// Default implementation delegates to `create_tools` (ignores the override).
    async fn create_tools_with_security(
        &self,
        agent: &AgentManifest,
        _security: Arc<nenjo_tools::security::SecurityPolicy>,
    ) -> Vec<Arc<dyn Tool>> {
        self.create_tools(agent).await
    }

    /// Create tools with execution context such as the active project.
    ///
    /// Default implementation delegates to `create_tools_with_security`.
    async fn create_tools_with_context(
        &self,
        agent: &AgentManifest,
        security: Arc<nenjo_tools::security::SecurityPolicy>,
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
        nenjo_tools::security::SecurityPolicy::default().workspace_dir
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
    inner: Arc<ProviderInner>,
}

pub(crate) struct ProviderInner {
    manifest: ManifestIndex,
    context_renderer: ContextRenderer,
    services: Arc<ProviderServices>,
}

pub(crate) struct ManifestIndex {
    manifest: Arc<Manifest>,
    agents_by_id: HashMap<Uuid, usize>,
    agents_by_name: HashMap<String, usize>,
    models_by_id: HashMap<Uuid, usize>,
    routines_by_id: HashMap<Uuid, usize>,
    projects_by_id: HashMap<Uuid, usize>,
    projects_by_slug: HashMap<String, usize>,
    abilities_by_id: HashMap<Uuid, usize>,
    domains_by_id: HashMap<Uuid, usize>,
    mcp_servers_by_id: HashMap<Uuid, usize>,
}

impl ManifestIndex {
    fn new(manifest: Arc<Manifest>) -> Self {
        Self {
            agents_by_id: index_by_id(manifest.agents.iter().map(|agent| agent.id)),
            agents_by_name: index_by_name(manifest.agents.iter().map(|agent| agent.name.as_str())),
            models_by_id: index_by_id(manifest.models.iter().map(|model| model.id)),
            routines_by_id: index_by_id(manifest.routines.iter().map(|routine| routine.id)),
            projects_by_id: index_by_id(manifest.projects.iter().map(|project| project.id)),
            projects_by_slug: index_by_name(
                manifest
                    .projects
                    .iter()
                    .map(|project| project.slug.as_str()),
            ),
            abilities_by_id: index_by_id(manifest.abilities.iter().map(|ability| ability.id)),
            domains_by_id: index_by_id(manifest.domains.iter().map(|domain| domain.id)),
            mcp_servers_by_id: index_by_id(manifest.mcp_servers.iter().map(|server| server.id)),
            manifest,
        }
    }

    fn agent_by_id(&self, id: Uuid) -> Option<&AgentManifest> {
        self.agents_by_id
            .get(&id)
            .map(|index| &self.manifest.agents[*index])
    }

    fn agent_by_name(&self, name: &str) -> Option<&AgentManifest> {
        self.agents_by_name
            .get(name)
            .map(|index| &self.manifest.agents[*index])
    }

    fn model_by_id(&self, id: Uuid) -> Option<&ModelManifest> {
        self.models_by_id
            .get(&id)
            .map(|index| &self.manifest.models[*index])
    }

    fn routine_by_id(&self, id: Uuid) -> Option<&crate::manifest::RoutineManifest> {
        self.routines_by_id
            .get(&id)
            .map(|index| &self.manifest.routines[*index])
    }

    fn project_by_id(&self, id: Uuid) -> Option<&ProjectManifest> {
        self.projects_by_id
            .get(&id)
            .map(|index| &self.manifest.projects[*index])
    }

    fn project_by_slug(&self, slug: &str) -> Option<&ProjectManifest> {
        self.projects_by_slug
            .get(slug)
            .map(|index| &self.manifest.projects[*index])
    }

    fn abilities_by_ids(&self, ids: &[Uuid]) -> Vec<crate::manifest::AbilityManifest> {
        let mut seen = HashSet::with_capacity(ids.len());
        ids.iter()
            .filter_map(|id| {
                if !seen.insert(*id) {
                    return None;
                }
                self.abilities_by_id
                    .get(id)
                    .map(|index| self.manifest.abilities[*index].clone())
            })
            .collect()
    }

    fn domains_by_ids(&self, ids: &[Uuid]) -> Vec<crate::manifest::DomainManifest> {
        let mut seen = HashSet::with_capacity(ids.len());
        ids.iter()
            .filter_map(|id| {
                if !seen.insert(*id) {
                    return None;
                }
                self.domains_by_id
                    .get(id)
                    .map(|index| self.manifest.domains[*index].clone())
            })
            .collect()
    }

    fn mcp_server_info_by_ids(&self, ids: &[Uuid]) -> Vec<(String, String)> {
        let mut seen = HashSet::with_capacity(ids.len());
        ids.iter()
            .filter_map(|id| {
                if !seen.insert(*id) {
                    return None;
                }
                self.mcp_servers_by_id.get(id).map(|index| {
                    let server = &self.manifest.mcp_servers[*index];
                    (
                        server.display_name.clone(),
                        server.description.clone().unwrap_or_default(),
                    )
                })
            })
            .collect()
    }
}

fn index_by_id(ids: impl Iterator<Item = Uuid>) -> HashMap<Uuid, usize> {
    let mut index = HashMap::new();
    for (position, id) in ids.enumerate() {
        index.entry(id).or_insert(position);
    }
    index
}

fn index_by_name<'a>(names: impl Iterator<Item = &'a str>) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    for (position, name) in names.enumerate() {
        index.entry(name.to_string()).or_insert(position);
    }
    index
}

pub(crate) struct ProviderServices {
    model_factory: Box<dyn ModelProviderFactory>,
    tool_factory: Box<dyn ToolFactory>,
    memory: Option<Arc<dyn Memory>>,
    agent_config: AgentConfig,
}

impl Provider {
    /// Start building a Provider.
    pub fn builder() -> ProviderBuilder {
        ProviderBuilder::new()
    }

    pub(crate) fn new_inner(
        manifest: Arc<Manifest>,
        model_factory: Box<dyn ModelProviderFactory>,
        tool_factory: Box<dyn ToolFactory>,
        memory: Option<Arc<dyn Memory>>,
        agent_config: AgentConfig,
    ) -> Self {
        let services = Arc::new(ProviderServices {
            model_factory,
            tool_factory,
            memory,
            agent_config,
        });
        Self::from_services(manifest, services)
    }

    fn from_services(manifest: Arc<Manifest>, services: Arc<ProviderServices>) -> Self {
        let render_blocks: Vec<_> = manifest
            .context_blocks
            .iter()
            .map(prompts::render_context_block)
            .collect();
        let context_renderer = ContextRenderer::from_blocks(&render_blocks);

        Self {
            inner: Arc::new(ProviderInner {
                manifest: ManifestIndex::new(manifest),
                context_renderer,
                services,
            }),
        }
    }

    /// Get an agent builder by agent ID.
    pub async fn agent_by_id(&self, id: Uuid) -> Result<AgentBuilder, ProviderError> {
        let agent = self
            .inner
            .manifest
            .agent_by_id(id)
            .ok_or_else(|| ProviderError::AgentNotFound(id.to_string()))?;

        self.build_agent(agent).await
    }

    /// Get an agent builder by agent name.
    pub async fn agent_by_name(&self, name: &str) -> Result<AgentBuilder, ProviderError> {
        let agent = self
            .inner
            .manifest
            .agent_by_name(name)
            .ok_or_else(|| ProviderError::AgentNotFound(name.to_string()))?;

        self.build_agent(agent).await
    }

    /// Access the bootstrap manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest.manifest
    }

    /// Get a clone of the manifest Arc (for mutation + rebuild).
    pub fn manifest_arc(&self) -> Arc<Manifest> {
        self.inner.manifest.manifest.clone()
    }

    /// Create a new Provider with the given manifest but same factories/memory/config.
    ///
    /// Used by the harness to hot-swap bootstrap data without rebuilding factories.
    pub fn with_manifest(&self, manifest: Manifest) -> Self {
        Self::from_services(Arc::new(manifest), self.inner.services.clone())
    }

    /// Access the memory backend, if configured.
    pub fn memory(&self) -> Option<&Arc<dyn Memory>> {
        self.inner.services.memory.as_ref()
    }

    /// Access the agent config.
    pub fn agent_config(&self) -> &AgentConfig {
        &self.inner.services.agent_config
    }

    /// Access the tool factory.
    pub fn tool_factory(&self) -> &dyn ToolFactory {
        &*self.inner.services.tool_factory
    }

    pub(crate) fn agent_manifest_by_name(&self, name: &str) -> Option<&AgentManifest> {
        self.inner.manifest.agent_by_name(name)
    }

    pub(crate) fn project_by_id(&self, id: Uuid) -> Option<&ProjectManifest> {
        self.inner.manifest.project_by_id(id)
    }

    /// Look up a project manifest by slug from the indexed bootstrap manifest.
    pub fn project_by_slug(&self, slug: &str) -> Option<&ProjectManifest> {
        self.inner.manifest.project_by_slug(slug)
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
            .inner
            .manifest
            .routine_by_id(routine_id)
            .ok_or_else(|| ProviderError::RoutineNotFound(routine_id.to_string()))?
            .clone();

        Ok(RoutineRunner {
            provider: self.clone(),
            routine,
            session_binding: None,
        })
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    async fn build_agent(&self, agent: &AgentManifest) -> Result<AgentBuilder, ProviderError> {
        let model = self.resolve_model(agent)?;

        let provider = self
            .inner
            .services
            .model_factory
            .create_with_base_url(&model.model_provider, model.base_url.as_deref())
            .map_err(|e| {
                ProviderError::FactoryFailed(e.context(format!(
                    "failed to create LLM provider '{}' for agent '{}'",
                    model.model_provider, agent.name
                )))
            })?;

        let tools = self.inner.services.tool_factory.create_tools(agent).await;
        if tracing::enabled!(tracing::Level::TRACE) {
            let tool_names = tools
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>()
                .join("\n- ");
            trace!(
                agent = %agent.name,
                tool_count = tools.len(),
                "\nTool belt for {}:\n- {}",
                agent.name,
                tool_names,
            );
        }

        // Memory backend is passed to the builder; scope and tools are
        // constructed in build() based on the project context set at that point.

        let prompt_config = agent.prompt_config.clone();
        debug!(
            agent = %agent.name,
            system_prompt_len = prompt_config.system_prompt.len(),
            cron_task_len = prompt_config.templates.cron_task.len(),
            task_execution_len = prompt_config.templates.task_execution.len(),
            "Loaded typed prompt_config"
        );

        let agent_config = self.inner.services.agent_config.clone();
        let prompt_context = self.build_prompt_context(agent);

        let mut builder = AgentBuilder::new(super::agents::builder::AgentBuilderParams {
            agent: agent.clone(),
            model,
            provider,
            tools,
            prompt_config,
            prompt_context,
            agent_config,
            context_renderer: self.inner.context_renderer.clone(),
        });

        if let Some(ref mem) = self.inner.services.memory {
            builder = builder.with_memory(mem.clone());
        }

        // Enable delegation support so the runner can inject DelegateToTool.
        builder = builder.with_delegation_support(self.clone());

        Ok(builder)
    }

    fn resolve_model(&self, agent: &AgentManifest) -> Result<ModelManifest, ProviderError> {
        let model_id = agent.model_id.ok_or_else(|| {
            ProviderError::ModelNotFound(format!("agent '{}' has no model assigned", agent.name))
        })?;

        self.inner
            .manifest
            .model_by_id(model_id)
            .cloned()
            .ok_or_else(|| {
                ProviderError::ModelNotFound(format!(
                    "model {model_id} not found (agent '{}')",
                    agent.name
                ))
            })
    }

    fn build_prompt_context(&self, agent: &AgentManifest) -> PromptContext {
        let abilities: Vec<_> = self.inner.manifest.abilities_by_ids(&agent.ability_ids);

        let domains: Vec<_> = self.inner.manifest.domains_by_ids(&agent.domain_ids);

        let mcp_server_info: Vec<(String, String)> = self
            .inner
            .manifest
            .mcp_server_info_by_ids(&agent.mcp_server_ids);

        let current_project = self
            .inner
            .manifest
            .manifest
            .projects
            .first()
            .cloned()
            .unwrap_or_else(|| ProjectManifest {
                id: Uuid::nil(),
                name: String::new(),
                slug: String::new(),
                description: None,
                settings: serde_json::Value::Null,
            });

        PromptContext {
            agent_name: agent.name.clone(),
            agent_description: agent.description.clone().unwrap_or_default(),
            available_agents: self.inner.manifest.manifest.agents.clone(),
            available_routines: self.inner.manifest.manifest.routines.clone(),
            current_project,
            available_abilities: abilities,
            available_domains: domains,
            mcp_server_info,
            platform_scopes: agent.platform_scopes.clone(),
            active_domain: None,
            append_active_domain_addon: true,
            docs_base_dir: Some(self.inner.services.tool_factory.workspace_dir()),
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
    session_binding: Option<crate::routines::SessionBinding>,
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

    pub fn with_session_binding(mut self, binding: crate::routines::SessionBinding) -> Self {
        self.session_binding = Some(binding);
        self
    }

    /// Run the routine to completion and return the final result.
    pub async fn run(&self, task: crate::types::TaskType) -> Result<StepResult> {
        self.run_stream(task).await?.output().await
    }

    /// Run the routine with streaming events.
    pub async fn run_stream(&self, task: crate::types::TaskType) -> Result<RoutineExecutionHandle> {
        self.provider
            .run_routine_inner(&self.routine, task, self.session_binding.clone())
            .await
    }
}

impl Provider {
    /// Internal: run a routine from an already-resolved manifest entry.
    async fn run_routine_inner(
        &self,
        routine: &RoutineManifest,
        task: crate::types::TaskType,
        session_binding: Option<crate::routines::SessionBinding>,
    ) -> Result<RoutineExecutionHandle> {
        let routine = routine.clone();
        let routine_name = routine.name.clone();
        let routine_id = routine.id;
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_inner = cancel.clone();

        let cron_schedule = match &task {
            crate::types::TaskType::Cron {
                schedule,
                start_at,
                timeout,
                ..
            } => Some((schedule.clone(), *start_at, *timeout)),
            _ => None,
        };

        let (events_tx, events_rx) = mpsc::unbounded_channel::<RoutineEvent>();

        let mut input = routines::types::routine_input_from_task(&task);
        if let Some(binding) = session_binding {
            input = input.with_session_binding(binding);
        }
        tracing::debug!(
            is_cron = input.is_cron_trigger,
            "RoutineInput built from task"
        );

        let provider = self.clone();

        let join = tokio::spawn(async move {
            let mut state = routines::types::RoutineState::new(routine_id, input);
            state.routine_name = Some(routine_name);

            if let Some((schedule, start_at, timeout)) = cron_schedule {
                routines::cron::executor::execute_routine_cron(
                    &provider,
                    &routine,
                    &mut state,
                    routines::cron::executor::CronExecutionConfig {
                        events_tx: &events_tx,
                        cancel: &cancel_inner,
                        schedule: &schedule,
                        start_at,
                        timeout,
                    },
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
    use crate::manifest::PromptConfig;
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

    struct WorkspaceToolFactory(std::path::PathBuf);

    #[async_trait::async_trait]
    impl ToolFactory for WorkspaceToolFactory {
        async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
            Vec::new()
        }

        fn workspace_dir(&self) -> std::path::PathBuf {
            self.0.clone()
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
            base_url: None,
        };
        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "agent".into(),
            description: Some("test".into()),
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: Some(model.id),
            domain_ids: vec![],
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };
        Manifest {
            agents: vec![agent],
            models: vec![model],
            projects: vec![ProjectManifest {
                id: Uuid::new_v4(),
                name: "p".into(),
                slug: "p".into(),
                description: None,
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
    async fn project_context_renders_synced_project_knowledge_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project = test_manifest().projects[0].clone();
        let project_dir = workspace_dir.join(&project.slug);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("knowledge_manifest.json"),
            format!(
                r#"{{
                  "pack_id": "project-{project_id}",
                  "pack_version": "1",
                  "schema_version": 1,
                  "root_uri": "project://{project_id}/",
                  "synced_at": "2026-01-01T00:00:00Z",
                  "docs": [
                    {{
                      "id": "overview",
                      "virtual_path": "project://{project_id}/domain/overview.md",
                      "source_path": "docs/domain/overview.md",
                      "title": "Overview",
                      "summary": "Project overview metadata",
                      "description": null,
                      "kind": "domain",
                      "authority": "canonical",
                      "status": "stable",
                      "tags": ["domain:project"],
                      "aliases": ["overview"],
                      "keywords": ["project"],
                      "related": []
                    }}
                  ]
                }}"#,
                project_id = project.id
            ),
        )
        .unwrap();

        let provider = Provider::builder()
            .with_manifest(test_manifest())
            .with_model_factory(MockFactory)
            .with_tool_factory(WorkspaceToolFactory(workspace_dir))
            .build()
            .await
            .unwrap();

        let runner = provider
            .agent_by_name("agent")
            .await
            .unwrap()
            .with_project_context(&project)
            .build()
            .await
            .unwrap();

        let documents_xml = &runner.instance().documents_xml;
        assert!(documents_xml.contains("<project_documents"));
        assert!(documents_xml.contains("name=\"overview.md\""));
        assert!(documents_xml.contains("path=\"domain\""));
        assert!(documents_xml.contains("Project overview metadata"));
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

    #[tokio::test]
    async fn routine_runner_keeps_manifest_snapshot_after_provider_update() {
        let model = ModelManifest {
            id: Uuid::new_v4(),
            name: "m".into(),
            description: None,
            model: "mock".into(),
            model_provider: "mock".into(),
            temperature: Some(0.5),
            base_url: None,
        };
        let original_agent_id = Uuid::new_v4();
        let updated_agent_id = Uuid::new_v4();
        let routine_id = Uuid::new_v4();
        let step_id = Uuid::new_v4();

        let original_agent = AgentManifest {
            id: original_agent_id,
            name: "agent-old".into(),
            description: Some("old".into()),
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: Some(model.id),
            domain_ids: vec![],
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };
        let updated_agent = AgentManifest {
            id: updated_agent_id,
            name: "agent-new".into(),
            description: Some("new".into()),
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: Some(model.id),
            domain_ids: vec![],
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };
        let routine = RoutineManifest {
            id: routine_id,
            name: "routine".into(),
            description: None,
            trigger: crate::manifest::RoutineTrigger::Task,
            metadata: crate::manifest::RoutineMetadata::default(),
            steps: vec![crate::manifest::RoutineStepManifest {
                id: step_id,
                routine_id,
                name: "step".into(),
                step_type: crate::manifest::RoutineStepType::Agent,
                council_id: None,
                agent_id: Some(original_agent_id),
                config: serde_json::json!({}),
                order_index: 0,
            }],
            edges: vec![],
        };

        let original_manifest = Manifest {
            agents: vec![original_agent.clone()],
            models: vec![model.clone()],
            routines: vec![routine.clone()],
            projects: vec![ProjectManifest {
                id: Uuid::new_v4(),
                name: "p".into(),
                slug: "p".into(),
                description: None,
                settings: serde_json::Value::Null,
            }],
            ..Default::default()
        };

        let provider = Provider::builder()
            .with_manifest(original_manifest)
            .with_model_factory(MockFactory)
            .with_tool_factory(NoopToolFactory)
            .build()
            .await
            .unwrap();

        let original_runner = provider.routine_by_id(routine_id).unwrap();

        let mut updated_manifest = provider.manifest().clone();
        updated_manifest.agents = vec![updated_agent.clone()];
        updated_manifest.routines[0].steps[0].agent_id = Some(updated_agent_id);

        let updated_provider = provider.with_manifest(updated_manifest);
        let updated_runner = updated_provider.routine_by_id(routine_id).unwrap();

        assert_eq!(
            original_runner.routine.steps[0].agent_id,
            Some(original_agent_id)
        );
        assert_eq!(
            updated_runner.routine.steps[0].agent_id,
            Some(updated_agent_id)
        );
        assert_eq!(
            original_runner.provider.manifest().agents[0].name,
            "agent-old"
        );
        assert_eq!(
            updated_runner.provider.manifest().agents[0].name,
            "agent-new"
        );
    }
}
