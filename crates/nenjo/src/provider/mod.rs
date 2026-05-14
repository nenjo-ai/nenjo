//! Provider — the root object for the Nenjo SDK.
//!
//! Holds the bootstrap manifest, LLM provider factory, tool factory, memory
//! backend, and provider-level knowledge packs. Build manifest-backed agents
//! via [`Provider::agent_by_id`] or [`Provider::agent_by_name`], or start a
//! blank agent builder with [`Provider::new_agent`].

pub mod builder;
pub mod error;
pub mod runtime;
pub mod tool_factory;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

pub use crate::routines::RoutineRunner;
pub use builder::ProviderBuilder;
pub use error::ProviderError;
pub use nenjo_models::{ModelProviderFactory, TypedModelProviderFactory};
pub use runtime::{ProviderMemory, ProviderRuntime};
pub use tool_factory::{NoopToolFactory, ToolContext, ToolFactory};

use crate::agents::builder::AgentBuilder;
use crate::agents::prompts::{self as prompts, PromptContext};
use crate::config::AgentConfig;
use crate::context::ContextRenderer;
use crate::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use crate::memory::Memory;
use crate::tools::Tool;
use crate::types::RenderContextVars;
use tracing::debug;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// The root object for the Nenjo SDK.
///
/// Created via [`ProviderBuilder`]. Holds the bootstrap manifest and runtime
/// factories. Use [`agent_by_id`](Self::agent_by_id) or
/// [`agent_by_name`](Self::agent_by_name) for manifest-backed agents, or
/// [`new_agent`](Self::new_agent) when the caller supplies an agent manifest
/// and model explicitly.
pub struct Provider<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> {
    inner: Arc<ProviderInner<ModelFactory, ToolFactoryImpl, Mem>>,
}

/// Compatibility provider with erased model factory, tool factory, and memory
/// backend types.
pub type ErasedProvider =
    Provider<dyn ModelProviderFactory + 'static, dyn ToolFactory + 'static, dyn Memory + 'static>;

impl<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> Clone
    for Provider<ModelFactory, ToolFactoryImpl, Mem>
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

pub(crate) struct ProviderInner<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> {
    manifest: ManifestIndex,
    context_renderer: ContextRenderer,
    services: ProviderServices<ModelFactory, ToolFactoryImpl, Mem>,
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

pub(crate) struct ProviderServices<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> {
    model_factory: Arc<ModelFactory>,
    tool_factory: Arc<ToolFactoryImpl>,
    memory: Option<Arc<Mem>>,
    agent_config: AgentConfig,
    render_ctx_extra: RenderContextVars,
    knowledge_registry: nenjo_knowledge::tools::StaticKnowledgeRegistry,
}

impl<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> Clone
    for ProviderServices<ModelFactory, ToolFactoryImpl, Mem>
{
    fn clone(&self) -> Self {
        Self {
            model_factory: self.model_factory.clone(),
            tool_factory: self.tool_factory.clone(),
            memory: self.memory.clone(),
            agent_config: self.agent_config.clone(),
            render_ctx_extra: self.render_ctx_extra.clone(),
            knowledge_registry: self.knowledge_registry.clone(),
        }
    }
}

impl ErasedProvider {
    /// Start building a Provider.
    pub fn builder() -> ProviderBuilder {
        ProviderBuilder::new()
    }
}

impl<ModelFactory, ToolFactoryImpl, Mem> Provider<ModelFactory, ToolFactoryImpl, Mem>
where
    ModelFactory: TypedModelProviderFactory + ?Sized + 'static,
    ToolFactoryImpl: ToolFactory + ?Sized + 'static,
    Mem: ProviderMemory + ?Sized + 'static,
{
    pub(crate) fn new_inner(
        manifest: Arc<Manifest>,
        model_factory: Arc<ModelFactory>,
        tool_factory: Arc<ToolFactoryImpl>,
        memory: Option<Arc<Mem>>,
        agent_config: AgentConfig,
        render_ctx_extra: RenderContextVars,
        knowledge_registry: nenjo_knowledge::tools::StaticKnowledgeRegistry,
    ) -> Self {
        let services = ProviderServices {
            model_factory,
            tool_factory,
            memory,
            agent_config,
            render_ctx_extra,
            knowledge_registry,
        };
        Self::from_services(manifest, services)
    }

    fn from_services(
        manifest: Arc<Manifest>,
        services: ProviderServices<ModelFactory, ToolFactoryImpl, Mem>,
    ) -> Self {
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
    pub async fn agent_by_id(&self, id: Uuid) -> Result<AgentBuilder<Self>, ProviderError> {
        let agent = self
            .inner
            .manifest
            .agent_by_id(id)
            .ok_or_else(|| ProviderError::AgentNotFound(id.to_string()))?;

        self.build_agent(agent).await
    }

    /// Get an agent builder by agent name.
    pub async fn agent_by_name(&self, name: &str) -> Result<AgentBuilder<Self>, ProviderError> {
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
    pub fn manifest_snapshot(&self) -> Arc<Manifest> {
        self.inner.manifest.manifest.clone()
    }

    /// Create a new Provider with the given manifest but same factories/memory/config.
    ///
    /// Used by the harness to hot-swap bootstrap data without rebuilding factories.
    pub fn with_manifest(&self, manifest: Manifest) -> Self {
        Self::from_services(Arc::new(manifest), self.inner.services.clone())
    }

    /// Access the memory backend, if configured.
    pub fn memory(&self) -> Option<&Arc<Mem>> {
        self.inner.services.memory.as_ref()
    }

    /// Access the agent config.
    pub fn agent_config(&self) -> &AgentConfig {
        &self.inner.services.agent_config
    }

    /// Access the tool factory.
    pub fn tool_factory(&self) -> &ToolFactoryImpl {
        self.inner.services.tool_factory.as_ref()
    }

    pub(crate) fn find_agent_manifest(&self, name: &str) -> Option<&AgentManifest> {
        self.inner.manifest.agent_by_name(name)
    }

    pub(crate) fn find_project(&self, id: Uuid) -> Option<&ProjectManifest> {
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
    /// let task = nenjo::TaskInput::new(project_id, task_id, "Fix auth", "Repair the login flow");
    /// let result = provider.routine_by_id(id)?
    ///     .run(task)
    ///     .await?;
    /// ```
    pub fn routine_by_id(&self, routine_id: Uuid) -> Result<RoutineRunner<Self>, ProviderError> {
        let routine = self
            .inner
            .manifest
            .routine_by_id(routine_id)
            .ok_or_else(|| ProviderError::RoutineNotFound(routine_id.to_string()))?
            .clone();

        Ok(RoutineRunner::new(self.clone(), routine))
    }

    /// Start configuring an agent that does not need to exist in the provider manifest.
    pub fn new_agent(&self) -> AgentBuilder<Self> {
        AgentBuilder::blank(
            self.clone(),
            self.inner.services.agent_config.clone(),
            self.inner.context_renderer.clone(),
        )
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    async fn build_agent(
        &self,
        agent: &AgentManifest,
    ) -> Result<AgentBuilder<Self>, ProviderError> {
        let model = self.resolve_model(agent)?;

        let provider = self
            .inner
            .services
            .model_factory
            .create_typed_with_base_url(&model.model_provider, model.base_url.as_deref())
            .map_err(|e| {
                ProviderError::FactoryFailed(e.context(format!(
                    "failed to create LLM provider '{}' for agent '{}'",
                    model.model_provider, agent.name
                )))
            })?;

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
            model_provider: provider,
            tools: Vec::new(),
            prompt_context,
            agent_config,
            context_renderer: self.inner.context_renderer.clone(),
        });

        if let Some(memory) = Mem::clone_runtime(self.inner.services.memory.as_ref()) {
            builder = builder.with_memory(memory);
        }

        // Enable delegation support so the runner can inject DelegateToTool.
        builder = builder.with_delegation_support(self.clone());

        Ok(builder)
    }

    pub(crate) fn create_knowledge_tools(&self) -> Vec<Arc<dyn Tool>> {
        if self.inner.services.knowledge_registry.is_empty() {
            Vec::new()
        } else {
            nenjo_knowledge::tools::knowledge_toolbelt(Arc::new(
                self.inner.services.knowledge_registry.clone(),
            ))
        }
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
            render_ctx_extra: self.inner.services.render_ctx_extra.clone(),
        }
    }

    async fn create_model_provider(
        &self,
        model: &ModelManifest,
    ) -> Result<Arc<ModelFactory::Provider<'static>>, ProviderError> {
        self.inner
            .services
            .model_factory
            .create_typed_with_base_url(&model.model_provider, model.base_url.as_deref())
            .map_err(|e| {
                ProviderError::FactoryFailed(e.context(format!(
                    "failed to create LLM provider '{}'",
                    model.model_provider
                )))
            })
    }
}

#[async_trait::async_trait]
impl<ModelFactory, ToolFactoryImpl, Mem> ProviderRuntime
    for Provider<ModelFactory, ToolFactoryImpl, Mem>
where
    ModelFactory: TypedModelProviderFactory + ?Sized + 'static,
    ToolFactoryImpl: ToolFactory + ?Sized + 'static,
    Mem: ProviderMemory + ?Sized + 'static,
{
    type Model<'a>
        = ModelFactory::Provider<'static>
    where
        Self: 'a;
    type ToolFactory<'a>
        = ToolFactoryImpl
    where
        Self: 'a;
    type Memory<'a>
        = Mem::Runtime<'static>
    where
        Self: 'a;

    fn manifest_snapshot(&self) -> Arc<Manifest> {
        Provider::manifest_snapshot(self)
    }

    fn tool_factory(&self) -> &Self::ToolFactory<'_> {
        self.tool_factory()
    }

    fn find_agent_manifest(&self, name: &str) -> Option<&AgentManifest> {
        Provider::find_agent_manifest(self, name)
    }

    fn find_project(&self, id: Uuid) -> Option<&ProjectManifest> {
        Provider::find_project(self, id)
    }

    fn create_knowledge_tools(&self) -> Vec<Arc<dyn Tool>> {
        Provider::create_knowledge_tools(self)
    }

    fn build_prompt_context(&self, agent: &AgentManifest) -> PromptContext {
        Provider::build_prompt_context(self, agent)
    }

    async fn create_model_provider(
        &self,
        model: &ModelManifest,
    ) -> Result<Arc<Self::Model<'static>>, ProviderError> {
        Provider::create_model_provider(self, model).await
    }

    fn new_agent(&self) -> AgentBuilder<Self> {
        Provider::new_agent(self)
    }

    async fn build_agent_by_id(&self, id: Uuid) -> Result<AgentBuilder<Self>, ProviderError> {
        Provider::agent_by_id(self, id).await
    }

    async fn build_agent_by_name(&self, name: &str) -> Result<AgentBuilder<Self>, ProviderError> {
        Provider::agent_by_name(self, name).await
    }
}

#[cfg(test)]
mod tests;
