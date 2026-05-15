//! Builder for [`Provider`].

use std::sync::Arc;

use anyhow::{Result, bail};

use super::{
    ErasedProvider, ModelProviderFactory, NoopToolFactory, Provider, ProviderMemory, ToolFactory,
    TypedModelProviderFactory,
};
use crate::config::AgentConfig;
use crate::context::RenderContextVars;
use crate::manifest::{Manifest, ManifestLoader};
use crate::memory::Memory;
use nenjo_knowledge::tools::KnowledgePackEntry;

/// Builder for creating a [`Provider`].
///
/// # Quick start (with NenjoClient)
///
/// ```ignore
/// use nenjo_api_client::NenjoClient;
///
/// let client = NenjoClient::new("https://api.nenjo.dev", "nj_sk_...");
/// let provider = Provider::builder()
///     .with_loader(client)
///     .with_model_factory(my_factory)
///     .build()
///     .await?;
/// ```
///
/// # With local context blocks merged on top
///
/// ```ignore
/// let provider = Provider::builder()
///     .with_loader(client)
///     .with_loader(LocalManifestLoader::new("."))
///     .with_model_factory(factory)
///     .build()
///     .await?;
/// ```
pub struct ProviderBuilder<
    Loaders = (),
    ModelFactory = MissingModelProviderFactory,
    ToolFactoryImpl = NoopToolFactory,
    Mem = NoMemory,
> {
    manifest: Option<Manifest>,
    loaders: Loaders,
    model_factory: ModelFactory,
    tool_factory: ToolFactoryImpl,
    memory: Option<Mem>,
    agent_config: AgentConfig,
    render_ctx_extra: RenderContextVars,
    knowledge_registry: nenjo_knowledge::tools::StaticKnowledgeRegistry,
}

/// Marker used until `.with_model_factory(...)` is called.
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct MissingModelProviderFactory;

impl ModelProviderFactory for MissingModelProviderFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn nenjo_models::ModelProvider>> {
        bail!("model_factory is required — use .with_model_factory(factory)")
    }
}

/// Uninhabited marker used until `.with_memory(...)` is called.
#[doc(hidden)]
pub enum NoMemory {}

#[async_trait::async_trait]
#[doc(hidden)]
pub trait ManifestLoaders {
    async fn load_into(&self, manifest: &mut Manifest) -> Result<()>;
}

#[async_trait::async_trait]
impl ManifestLoaders for () {
    async fn load_into(&self, _manifest: &mut Manifest) -> Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl<Previous, Loader> ManifestLoaders for (Previous, Loader)
where
    Previous: ManifestLoaders + Send + Sync,
    Loader: ManifestLoader + Send + Sync,
{
    async fn load_into(&self, manifest: &mut Manifest) -> Result<()> {
        self.0.load_into(manifest).await?;
        manifest.merge(self.1.load().await?);
        Ok(())
    }
}

impl ProviderBuilder<(), MissingModelProviderFactory, NoopToolFactory, NoMemory> {
    /// Create an empty provider builder.
    pub fn new() -> Self {
        Self {
            manifest: None,
            loaders: (),
            model_factory: MissingModelProviderFactory,
            tool_factory: NoopToolFactory,
            memory: None,
            agent_config: AgentConfig::default(),
            render_ctx_extra: RenderContextVars::default(),
            knowledge_registry: nenjo_knowledge::tools::StaticKnowledgeRegistry::new(),
        }
    }
}

impl<Loaders, ModelFactory, ToolFactoryImpl, Mem>
    ProviderBuilder<Loaders, ModelFactory, ToolFactoryImpl, Mem>
{
    /// Provide a pre-built manifest directly.
    ///
    /// Loaders added via [`with_loader`](Self::with_loader) are merged on top,
    /// so you can use both: a base manifest plus loaders for local overrides.
    pub fn with_manifest(mut self, manifest: Manifest) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Add a manifest loader.
    ///
    /// Loaders are called in order during [`build()`](Self::build). Each
    /// returns a partial manifest that is merged into the result. Later
    /// loaders override earlier ones on name collision (for context blocks).
    ///
    /// `NenjoClient` from `nenjo-api-client` implements `ManifestLoader`,
    /// so you can pass it directly:
    ///
    /// ```ignore
    /// builder.with_loader(NenjoClient::new(url, api_key))
    /// ```
    pub fn with_loader<Loader>(
        self,
        loader: Loader,
    ) -> ProviderBuilder<(Loaders, Loader), ModelFactory, ToolFactoryImpl, Mem>
    where
        Loader: ManifestLoader + 'static,
    {
        ProviderBuilder {
            manifest: self.manifest,
            loaders: (self.loaders, loader),
            model_factory: self.model_factory,
            tool_factory: self.tool_factory,
            memory: self.memory,
            agent_config: self.agent_config,
            render_ctx_extra: self.render_ctx_extra,
            knowledge_registry: self.knowledge_registry,
        }
    }

    /// Set the LLM model factory.
    ///
    /// Manifest-backed agent resolution requires a model factory. Blank
    /// agents created with [`Provider::new_agent`](crate::provider::Provider::new_agent)
    /// may instead provide a concrete model provider through
    /// [`AgentBuilder::with_model_provider`](crate::agents::AgentBuilder::with_model_provider).
    pub fn with_model_factory<Factory>(
        self,
        factory: Factory,
    ) -> ProviderBuilder<Loaders, Factory, ToolFactoryImpl, Mem>
    where
        Factory: TypedModelProviderFactory + 'static,
    {
        ProviderBuilder {
            manifest: self.manifest,
            loaders: self.loaders,
            model_factory: factory,
            tool_factory: self.tool_factory,
            memory: self.memory,
            agent_config: self.agent_config,
            render_ctx_extra: self.render_ctx_extra,
            knowledge_registry: self.knowledge_registry,
        }
    }

    /// Set the tool factory.
    ///
    /// Defaults to [`NoopToolFactory`] if not set.
    pub fn with_tool_factory<Factory>(
        self,
        factory: Factory,
    ) -> ProviderBuilder<Loaders, ModelFactory, Factory, Mem>
    where
        Factory: ToolFactory + 'static,
    {
        ProviderBuilder {
            manifest: self.manifest,
            loaders: self.loaders,
            model_factory: self.model_factory,
            tool_factory: factory,
            memory: self.memory,
            agent_config: self.agent_config,
            render_ctx_extra: self.render_ctx_extra,
            knowledge_registry: self.knowledge_registry,
        }
    }

    /// Set the memory backend.
    ///
    /// When set, agents automatically get memory tools (store, recall, forget)
    /// and memory summaries are injected into prompts.
    pub fn with_memory<MemoryImpl>(
        self,
        memory: MemoryImpl,
    ) -> ProviderBuilder<Loaders, ModelFactory, ToolFactoryImpl, MemoryImpl>
    where
        MemoryImpl: Memory + 'static,
    {
        ProviderBuilder {
            manifest: self.manifest,
            loaders: self.loaders,
            model_factory: self.model_factory,
            tool_factory: self.tool_factory,
            memory: Some(memory),
            agent_config: self.agent_config,
            render_ctx_extra: self.render_ctx_extra,
            knowledge_registry: self.knowledge_registry,
        }
    }

    /// Set the agent configuration applied to all agents.
    ///
    /// Controls turn loop behavior: max iterations, parallel tools,
    /// context token budget, etc. Defaults to [`AgentConfig::default()`].
    pub fn with_agent_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = config;
        self
    }

    /// Set provider-level prompt context vars injected into every agent.
    pub fn with_render_context_vars(mut self, vars: RenderContextVars) -> Self {
        self.render_ctx_extra = vars;
        self
    }

    /// Register multiple knowledge packs with this provider.
    ///
    /// Registered packs automatically contribute reusable knowledge tools and
    /// prompt metadata variables for all agents built by the provider. Use
    /// [`KnowledgePackEntry`] to preserve selector metadata and support
    /// collections with different concrete pack types.
    pub fn with_knowledge_packs<I, E>(mut self, packs: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<KnowledgePackEntry>,
    {
        for pack in packs {
            self.add_knowledge_pack(pack.into());
        }
        self
    }

    fn add_knowledge_pack(&mut self, entry: KnowledgePackEntry) {
        self.render_ctx_extra.knowledge_vars.extend(
            nenjo_knowledge::tools::knowledge_pack_prompt_vars(
                entry.selector(),
                entry.pack().as_ref(),
            ),
        );
        self.knowledge_registry = self.knowledge_registry.clone().with_entry(entry);
    }
}

impl<Loaders, ModelFactory, ToolFactoryImpl, Mem>
    ProviderBuilder<Loaders, ModelFactory, ToolFactoryImpl, Mem>
where
    Loaders: ManifestLoaders + Send + Sync,
    ModelFactory: TypedModelProviderFactory + 'static,
    ToolFactoryImpl: ToolFactory + 'static,
    Mem: ProviderMemory + 'static,
{
    /// Build the Provider by loading and merging all manifest sources.
    ///
    /// If no manifest source is configured, the Provider is built with an
    /// empty manifest. If no model factory is configured, manifest-backed agent
    /// resolution will fail unless the agent builder receives an explicit model
    /// provider.
    pub async fn build(self) -> Result<Provider<ModelFactory, ToolFactoryImpl, Mem>> {
        let model_factory = self.model_factory;

        // Start from the provided manifest (if any), then merge loaders on top.
        let mut manifest = self.manifest.unwrap_or_default();
        self.loaders.load_into(&mut manifest).await?;

        let manifest = Arc::new(manifest);

        Ok(Provider::new_inner(
            manifest,
            Arc::new(model_factory),
            Arc::new(self.tool_factory),
            self.memory.map(Arc::new),
            self.agent_config,
            self.render_ctx_extra,
            self.knowledge_registry,
        ))
    }
}

impl<Loaders, ModelFactory, ToolFactoryImpl, Mem>
    ProviderBuilder<Loaders, ModelFactory, ToolFactoryImpl, Mem>
where
    Loaders: ManifestLoaders + Send + Sync,
    ModelFactory: ModelProviderFactory + 'static,
    ToolFactoryImpl: ToolFactory + 'static,
    Mem: Memory + 'static,
{
    /// Build a Provider using the compatibility-erased model factory path.
    ///
    /// Use [`build`](Self::build) to preserve the concrete model
    /// factory type through [`Provider`].
    pub async fn build_erased(self) -> Result<ErasedProvider> {
        let model_factory = self.model_factory;

        let mut manifest = self.manifest.unwrap_or_default();
        self.loaders.load_into(&mut manifest).await?;

        Ok(Provider::new_inner(
            Arc::new(manifest),
            Arc::new(model_factory) as Arc<dyn ModelProviderFactory>,
            Arc::new(self.tool_factory) as Arc<dyn ToolFactory>,
            self.memory
                .map(|memory| Arc::new(memory) as Arc<dyn Memory>),
            self.agent_config,
            self.render_ctx_extra,
            self.knowledge_registry,
        ))
    }
}

impl Default for ProviderBuilder<(), MissingModelProviderFactory, NoopToolFactory, NoMemory> {
    fn default() -> Self {
        Self::new()
    }
}
