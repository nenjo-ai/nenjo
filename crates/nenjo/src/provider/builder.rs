//! Builder for [`Provider`].

use std::sync::Arc;

use anyhow::{Context, Result};

use super::{ModelProviderFactory, NoopToolFactory, Provider, ToolFactory};
use crate::config::AgentConfig;
use crate::manifest::{Manifest, ManifestLoader};
use crate::memory::Memory;
use crate::routines::LambdaRunner;

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
pub struct ProviderBuilder {
    manifest: Option<Manifest>,
    loaders: Vec<Arc<dyn ManifestLoader>>,
    model_factory: Option<Arc<dyn ModelProviderFactory>>,
    tool_factory: Option<Arc<dyn ToolFactory>>,
    memory: Option<Arc<dyn Memory>>,
    agent_config: AgentConfig,
    lambda_runner: Option<Arc<dyn LambdaRunner>>,
    template_source: Option<Arc<dyn crate::context::TemplateSource>>,
}

impl ProviderBuilder {
    pub fn new() -> Self {
        Self {
            manifest: None,
            loaders: Vec::new(),
            model_factory: None,
            tool_factory: None,
            memory: None,
            agent_config: AgentConfig::default(),
            lambda_runner: None,
            template_source: None,
        }
    }

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
    pub fn with_loader(mut self, loader: impl ManifestLoader + 'static) -> Self {
        self.loaders.push(Arc::new(loader));
        self
    }

    /// Set the LLM model factory (required).
    pub fn with_model_factory(mut self, factory: impl ModelProviderFactory + 'static) -> Self {
        self.model_factory = Some(Arc::new(factory));
        self
    }

    /// Set the tool factory.
    ///
    /// Defaults to [`NoopToolFactory`] if not set.
    pub fn with_tool_factory(mut self, factory: impl ToolFactory + 'static) -> Self {
        self.tool_factory = Some(Arc::new(factory));
        self
    }

    /// Set the memory backend.
    ///
    /// When set, agents automatically get memory tools (store, recall, forget)
    /// and memory summaries are injected into prompts.
    pub fn with_memory(mut self, memory: impl Memory + 'static) -> Self {
        self.memory = Some(Arc::new(memory));
        self
    }

    /// Set the agent configuration applied to all agents.
    ///
    /// Controls turn loop behavior: max iterations, parallel tools,
    /// context token budget, etc. Defaults to [`AgentConfig::default()`].
    pub fn with_agent_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = config;
        self
    }

    /// Set the lambda runner for executing deterministic script steps in routines.
    ///
    /// Without a lambda runner, lambda and cron-lambda steps will fail with a
    /// descriptive error.
    pub fn with_lambda_runner(mut self, runner: impl LambdaRunner + 'static) -> Self {
        self.lambda_runner = Some(Arc::new(runner));
        self
    }

    /// Set a lazy template source for context blocks.
    ///
    /// When set, the [`ContextRenderer`] loads templates on demand from this
    /// source instead of holding all templates in memory.
    pub fn with_template_source(mut self, source: Arc<dyn crate::context::TemplateSource>) -> Self {
        self.template_source = Some(source);
        self
    }

    /// Build the Provider by loading and merging all manifest sources.
    ///
    /// Requires at least one of [`with_manifest`](Self::with_manifest) or
    /// [`with_loader`](Self::with_loader) to be called.
    pub async fn build(self) -> Result<Provider> {
        anyhow::ensure!(
            self.manifest.is_some() || !self.loaders.is_empty(),
            "at least one manifest source is required — use .with_manifest() or .with_loader()"
        );

        let model_factory = self
            .model_factory
            .context("model_factory is required — use .with_model_factory(factory)")?;

        let tool_factory = self
            .tool_factory
            .unwrap_or_else(|| Arc::new(NoopToolFactory));

        // Start from the provided manifest (if any), then merge loaders on top.
        let mut manifest = self.manifest.unwrap_or_default();
        for loader in &self.loaders {
            let partial = loader.load().await?;
            manifest.merge(partial);
        }

        let manifest = Arc::new(manifest);

        Ok(Provider {
            manifest,
            model_factory,
            tool_factory,
            memory: self.memory,
            agent_config: self.agent_config,
            lambda_runner: self.lambda_runner,
            template_source: self.template_source,
        })
    }
}

impl Default for ProviderBuilder {
    fn default() -> Self {
        Self::new()
    }
}
