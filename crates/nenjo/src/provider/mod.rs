//! Provider — the root object for the Nenjo SDK.
//!
//! Holds the bootstrap manifest, LLM provider factory, tool factory, memory
//! backend, and provider-level knowledge packs. Build manifest-backed agents
//! via [`Provider::agent`], or start a
//! blank agent builder with [`Provider::new_agent`].

pub mod builder;
pub mod error;
pub mod runtime;
pub mod tool_factory;

use std::collections::HashMap;
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
use crate::manifest::{
    AbilityManifest, AgentManifest, DomainManifest, HasManifestSlug, Manifest, ModelManifest,
    ProjectManifest,
};
use crate::memory::Memory;
use crate::tools::Tool;
use crate::types::RenderContextVars;
use crate::{IntoSlug, Slug};
use tracing::debug;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// The root object for the Nenjo SDK.
///
/// Created via [`ProviderBuilder`]. Holds the bootstrap manifest and runtime
/// factories. Use [`agent`](Self::agent) for manifest-backed agents, or
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
    agents_by_slug: HashMap<Slug, usize>,
    abilities_by_name: HashMap<String, usize>,
    domains_by_slug: HashMap<Slug, usize>,
    domains_by_command: HashMap<String, usize>,
    models_by_slug: HashMap<Slug, usize>,
    routines_by_slug: HashMap<Slug, usize>,
    projects_by_slug: HashMap<Slug, usize>,
    councils_by_slug: HashMap<Slug, usize>,
}

impl ManifestIndex {
    fn new(manifest: Arc<Manifest>) -> Self {
        Self {
            agents_by_slug: index_by_manifest_slug(&manifest.agents),
            abilities_by_name: index_abilities_by_name(&manifest.abilities),
            domains_by_slug: index_domains_by_slug(&manifest.domains),
            domains_by_command: index_domains_by_command(&manifest.domains),
            models_by_slug: index_by_manifest_slug(&manifest.models),
            routines_by_slug: index_by_manifest_slug(&manifest.routines),
            projects_by_slug: index_by_manifest_slug(&manifest.projects),
            councils_by_slug: index_by_manifest_slug(&manifest.councils),
            manifest,
        }
    }

    fn agent(&self, slug: &Slug) -> Option<&AgentManifest> {
        self.agents_by_slug
            .get(slug)
            .map(|index| &self.manifest.agents[*index])
    }

    fn ability(&self, name: &str) -> Option<&AbilityManifest> {
        self.abilities_by_name
            .get(name)
            .map(|index| &self.manifest.abilities[*index])
    }

    fn domain(&self, selector: &str) -> Option<&DomainManifest> {
        self.domains_by_command
            .get(selector)
            .or_else(|| self.domains_by_slug.get(&Slug::derive(selector)))
            .map(|index| &self.manifest.domains[*index])
    }

    fn model(&self, slug: &Slug) -> Option<&ModelManifest> {
        self.models_by_slug
            .get(slug)
            .map(|index| &self.manifest.models[*index])
    }

    fn routine(&self, slug: &Slug) -> Option<&crate::manifest::RoutineManifest> {
        self.routines_by_slug
            .get(slug)
            .map(|index| &self.manifest.routines[*index])
    }

    fn project(&self, slug: &Slug) -> Option<&ProjectManifest> {
        self.projects_by_slug
            .get(slug)
            .map(|index| &self.manifest.projects[*index])
    }

    fn council(&self, slug: &Slug) -> Option<&crate::manifest::CouncilManifest> {
        self.councils_by_slug
            .get(slug)
            .map(|index| &self.manifest.councils[*index])
    }
}

fn index_by_manifest_slug<T: HasManifestSlug>(items: &[T]) -> HashMap<Slug, usize> {
    let mut index = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        index.entry(item.manifest_slug()).or_insert(position);
    }
    index
}

fn index_abilities_by_name(items: &[AbilityManifest]) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        index.entry(item.name.clone()).or_insert(position);
    }
    index
}

fn index_domains_by_slug(items: &[DomainManifest]) -> HashMap<Slug, usize> {
    let mut index = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        index.entry(item.manifest_slug()).or_insert(position);
        index.entry(Slug::derive(&item.name)).or_insert(position);
    }
    index
}

fn index_domains_by_command(items: &[DomainManifest]) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        index.entry(item.command.clone()).or_insert(position);
    }
    index
}

pub(crate) struct ProviderServices<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> {
    model_factory: Arc<ModelFactory>,
    tool_factory: Arc<ToolFactoryImpl>,
    memory: Option<Arc<Mem>>,
    agent_config: AgentConfig,
    render_ctx_extra: RenderContextVars,
    knowledge_registry: nenjo_knowledge::tools::CompositeKnowledgeRegistry,
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
        knowledge_registry: nenjo_knowledge::tools::CompositeKnowledgeRegistry,
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

    /// Get an agent builder by agent slug.
    pub async fn agent(&self, slug: impl IntoSlug) -> Result<AgentBuilder<Self>, ProviderError> {
        let slug = slug.into_slug();
        let agent = self
            .inner
            .manifest
            .agent(&slug)
            .ok_or_else(|| ProviderError::AgentNotFound(slug.to_string()))?;

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

    pub(crate) fn find_agent_manifest(&self, slug: &Slug) -> Option<&AgentManifest> {
        self.inner.manifest.agent(slug)
    }

    pub(crate) fn find_ability(&self, name: &str) -> Option<&AbilityManifest> {
        self.inner.manifest.ability(name)
    }

    pub(crate) fn find_domain(&self, selector: &str) -> Option<&DomainManifest> {
        self.inner.manifest.domain(selector)
    }

    pub(crate) fn find_project(&self, slug: &Slug) -> Option<&ProjectManifest> {
        self.inner.manifest.project(slug)
    }

    /// Look up a project manifest by slug from the indexed bootstrap manifest.
    pub fn project(&self, slug: impl IntoSlug) -> Result<&ProjectManifest, ProviderError> {
        let slug = slug.into_slug();
        self.inner
            .manifest
            .project(&slug)
            .ok_or_else(|| ProviderError::ProjectNotFound(slug.to_string()))
    }

    /// Look up a model manifest by slug from the indexed bootstrap manifest.
    pub fn model(&self, slug: impl IntoSlug) -> Result<&ModelManifest, ProviderError> {
        let slug = slug.into_slug();
        self.inner
            .manifest
            .model(&slug)
            .ok_or_else(|| ProviderError::ModelNotFound(slug.to_string()))
    }

    /// Look up a council manifest by slug from the indexed bootstrap manifest.
    pub fn council(
        &self,
        slug: impl IntoSlug,
    ) -> Result<&crate::manifest::CouncilManifest, ProviderError> {
        let slug = slug.into_slug();
        self.inner
            .manifest
            .council(&slug)
            .ok_or_else(|| ProviderError::CouncilNotFound(slug.to_string()))
    }

    // -----------------------------------------------------------------------
    // Routine execution
    // -----------------------------------------------------------------------

    /// Look up a routine by slug and return a builder for configuring execution.
    ///
    /// ```ignore
    /// let task = nenjo::TaskInput::new("Fix auth", "Repair the login flow")
    ///     .with_project("demo_project")
    ///     .with_task_id(task_id);
    /// let result = provider.routine("triage")?
    ///     .run(task)
    ///     .await?;
    /// ```
    pub fn routine(&self, slug: impl IntoSlug) -> Result<RoutineRunner<Self>, ProviderError> {
        let slug = slug.into_slug();
        let routine = self
            .inner
            .manifest
            .routine(&slug)
            .ok_or_else(|| ProviderError::RoutineNotFound(slug.to_string()))?
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
        let model_manifest = self.resolve_model(agent)?;

        // Memory backend is passed to the builder; scope and tools are
        // constructed in build() based on the project context set at that point.

        let prompt_config = agent.prompt_config.clone();
        debug!(
            agent = %agent.name,
            system_prompt_len = prompt_config.system_prompt.len(),
            task_execution_len = prompt_config.templates.task_execution.len(),
            "Loaded typed prompt_config"
        );

        let agent_config = self.inner.services.agent_config.clone();
        let prompt_context = self.build_prompt_context(agent);

        let mut builder = AgentBuilder::new(super::agents::builder::AgentBuilderParams {
            agent_manifest: agent.clone(),
            model_manifest,
            tools: Vec::new(),
            prompt_context,
            agent_config,
            context_renderer: self.inner.context_renderer.clone(),
            provider_runtime: self.clone(),
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
        let model_slug = agent.model.as_ref().ok_or_else(|| {
            ProviderError::ModelNotFound(format!("agent '{}' has no model assigned", agent.name))
        })?;

        self.inner
            .manifest
            .model(model_slug)
            .cloned()
            .ok_or_else(|| {
                ProviderError::ModelNotFound(format!(
                    "model {model_slug} not found (agent '{}')",
                    agent.name
                ))
            })
    }

    fn build_prompt_context(&self, agent: &AgentManifest) -> PromptContext {
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
                slug: Slug::derive("project"),
                description: None,
                settings: serde_json::Value::Null,
            });

        PromptContext {
            agent_name: agent.name.clone(),
            agent_description: agent.description.clone().unwrap_or_default(),
            current_project,
            active_domain: None,
            append_active_domain_addon: true,
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

    fn with_manifest(&self, manifest: Manifest) -> Self {
        Provider::with_manifest(self, manifest)
    }

    fn tool_factory(&self) -> &Self::ToolFactory<'_> {
        self.tool_factory()
    }

    fn find_agent_manifest(&self, slug: &Slug) -> Option<&AgentManifest> {
        Provider::find_agent_manifest(self, slug)
    }

    fn find_ability(&self, name: &str) -> Option<&AbilityManifest> {
        Provider::find_ability(self, name)
    }

    fn find_domain(&self, selector: &str) -> Option<&DomainManifest> {
        Provider::find_domain(self, selector)
    }

    fn find_project(&self, slug: &Slug) -> Option<&ProjectManifest> {
        Provider::find_project(self, slug)
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

    async fn agent(&self, slug: &Slug) -> Result<AgentBuilder<Self>, ProviderError> {
        Provider::agent(self, slug.as_str()).await
    }

    fn routine(&self, slug: &Slug) -> Result<RoutineRunner<Self>, ProviderError> {
        Provider::routine(self, slug.as_str())
    }
}

#[cfg(test)]
mod tests;
