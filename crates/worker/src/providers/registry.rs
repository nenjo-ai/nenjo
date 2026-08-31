//! Provider registry — implements `ModelProviderFactory` for the nenjo SDK.
//!
//! Maps provider name strings (e.g. "openai", "anthropic") to concrete
//! `ModelProvider` implementations, using API keys from the worker config.
//!
//! ## `openai-compatible:{tag}` convention
//!
//! For OpenAI-compatible providers, the `model_provider` field supports an
//! optional colon-delimited tag: `openai-compatible:sambanova`. The tag
//! drives API key resolution:
//!
//! 1. Config key lookup: `sambanova` in `[model_provider_api_keys]`
//! 2. Env var fallback: `SAMBANOVA_API_KEY`
//! 3. Generic fallback: `openai-compatible` config key / `OPENAI_COMPATIBLE_API_KEY`
//! 4. Empty (no auth — for local servers)
//!
//! vLLM is a separate first-class provider. It shares the compatible HTTP
//! transport but has its own content-part dialect and optional credentials.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::debug;

use nenjo::ModelProviderFactory;
use nenjo_models::{ArtifactInputTransport, MediaType, ModelProvider, ProviderMediaCapabilities};
use nenjo_models::{ReliableProvider, VllmStreaming};

use super::ModelProviders;
use crate::config::{Config as WorkerConfig, ReliabilityConfig};
use crate::media::{ArtifactTransportResolver, ArtifactTransportTarget, MediaCapabilitySource};

/// Complete configuration required to construct a model provider registry.
#[derive(Clone)]
pub struct ModelProviderRegistryConfig {
    api_keys: HashMap<String, String>,
    reliability: ReliabilityConfig,
    vllm_streaming: VllmStreaming,
    max_concurrent_requests: usize,
}

impl ModelProviderRegistryConfig {
    pub fn with_api_keys(mut self, keys: &HashMap<ModelProviders, String>) -> Self {
        self.api_keys = keys
            .iter()
            .map(|(provider, key)| (provider.to_string(), key.clone()))
            .collect();
        self
    }

    pub fn with_api_key(mut self, provider: ModelProviders, key: impl Into<String>) -> Self {
        self.api_keys.insert(provider.to_string(), key.into());
        self
    }

    pub fn with_reliability(mut self, reliability: ReliabilityConfig) -> Self {
        self.reliability = reliability;
        self
    }

    pub fn with_vllm_streaming(mut self, streaming: VllmStreaming) -> Self {
        self.vllm_streaming = streaming;
        self
    }

    pub fn with_max_concurrent_requests(mut self, max: usize) -> Self {
        self.max_concurrent_requests = max.max(1);
        self
    }
}

impl Default for ModelProviderRegistryConfig {
    fn default() -> Self {
        Self {
            api_keys: HashMap::new(),
            reliability: ReliabilityConfig::default(),
            vllm_streaming: VllmStreaming::Enabled,
            max_concurrent_requests: 3,
        }
    }
}

impl From<&WorkerConfig> for ModelProviderRegistryConfig {
    fn from(config: &WorkerConfig) -> Self {
        Self::default()
            .with_api_keys(&config.model_provider_api_keys)
            .with_reliability(config.reliability.clone())
            .with_vllm_streaming(config.vllm.streaming.into())
            .with_max_concurrent_requests(config.model_runtime.max_concurrent_requests)
    }
}

/// Registry that creates LLM provider instances on demand.
///
/// Implements `ModelProviderFactory` so it can be passed to `Provider::from_manifest()`.
/// Each created provider is wrapped in [`ReliableProvider`] for automatic retries
/// with exponential backoff, rate-limit handling, and model fallback.
pub struct ModelProviderRegistry {
    api_keys: HashMap<String, String>,
    reliability: ReliabilityConfig,
    vllm_streaming: VllmStreaming,
    model_admission: Arc<Semaphore>,
    max_concurrent_requests: usize,
    cache: Mutex<HashMap<ProviderCacheKey, Arc<dyn ModelProvider>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderCacheKey {
    provider_name: String,
    base_url: Option<String>,
}

/// Admission control sits inside the reliability wrapper, so every physical
/// retry/fallback attempt obtains a permit and releases it before backoff.
struct AdmissionControlledProvider {
    inner: Box<dyn ModelProvider>,
    permits: Arc<Semaphore>,
    limit: usize,
}

impl AdmissionControlledProvider {
    fn new(inner: Box<dyn ModelProvider>, permits: Arc<Semaphore>, limit: usize) -> Self {
        Self {
            inner,
            permits,
            limit,
        }
    }

    async fn acquire(&self) -> anyhow::Result<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Ok(permit);
        }
        let queued_at = std::time::Instant::now();
        debug!(
            max_concurrent_requests = self.limit,
            "Model request waiting for worker admission capacity"
        );
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .context("model admission controller closed")?;
        debug!(
            max_concurrent_requests = self.limit,
            queued_ms = queued_at.elapsed().as_millis(),
            "Model request admitted after capacity wait"
        );
        Ok(permit)
    }

    async fn acquire_for_stream(
        &self,
        events: &tokio::sync::mpsc::Sender<nenjo_models::ProviderStreamEvent>,
    ) -> anyhow::Result<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Ok(permit);
        }
        events
            .send(nenjo_models::ProviderStreamEvent::CapacityWaiting { limit: self.limit })
            .await
            .context("provider stream consumer closed while waiting for capacity")?;
        let permit = self.acquire().await?;
        events
            .send(nenjo_models::ProviderStreamEvent::CapacityAcquired)
            .await
            .context("provider stream consumer closed after capacity was acquired")?;
        Ok(permit)
    }
}

#[async_trait::async_trait]
impl ModelProvider for AdmissionControlledProvider {
    async fn chat(
        &self,
        request: nenjo_models::ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<nenjo_models::ChatResponse> {
        let _permit = self.acquire().await?;
        self.inner.chat(request, model, temperature).await
    }

    async fn chat_stream(
        &self,
        request: nenjo_models::ChatRequest<'_>,
        model: &str,
        temperature: f64,
        events: tokio::sync::mpsc::Sender<nenjo_models::ProviderStreamEvent>,
    ) -> anyhow::Result<nenjo_models::ChatResponse> {
        let _permit = self.acquire_for_stream(&events).await?;
        self.inner
            .chat_stream(request, model, temperature, events)
            .await
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        self.inner.context_window(model)
    }

    fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        self.inner.supports_developer_role(model)
    }

    fn artifact_input_transport(
        &self,
        model: &str,
        capability: nenjo_models::ModelCapabilityId,
        media_type: &MediaType,
    ) -> ArtifactInputTransport {
        self.inner
            .artifact_input_transport(model, capability, media_type)
    }

    fn media_capabilities(&self) -> Option<ProviderMediaCapabilities> {
        self.inner.media_capabilities()
    }

    async fn submit_media(
        &self,
        request: nenjo_models::NativeMediaRequest,
    ) -> anyhow::Result<nenjo_models::NativeMediaResponse> {
        let _permit = self.acquire().await?;
        self.inner.submit_media(request).await
    }

    async fn poll_media_job(
        &self,
        job: &nenjo_models::NativeMediaJob,
    ) -> anyhow::Result<nenjo_models::NativeMediaResponse> {
        let _permit = self.acquire().await?;
        self.inner.poll_media_job(job).await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        self.inner.warmup().await
    }
}

impl ProviderCacheKey {
    fn new(provider_name: &str, base_url: Option<&str>) -> Self {
        Self {
            provider_name: provider_name.to_string(),
            base_url: base_url.map(str::to_string),
        }
    }
}

impl ModelProviderRegistry {
    /// Create a registry from one complete configuration value.
    pub fn new(config: ModelProviderRegistryConfig) -> Self {
        debug!(
            providers = config.api_keys.len(),
            "ProviderRegistry initialized"
        );

        Self {
            api_keys: config.api_keys,
            reliability: config.reliability,
            vllm_streaming: config.vllm_streaming,
            model_admission: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            max_concurrent_requests: config.max_concurrent_requests,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Look up the API key for a provider name.
    pub fn api_key(&self, provider_name: &str) -> Option<&str> {
        self.api_keys.get(provider_name).map(|s| s.as_str())
    }

    /// Return a configured provider instance for worker-owned runtime tooling.
    pub fn provider(&self, provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        <Self as nenjo::ModelProviderFactory>::create(self, provider_name)
    }

    /// Return a configured provider instance with a model-specific base URL.
    pub fn provider_with_base_url(
        &self,
        provider_name: &str,
        base_url: Option<&str>,
    ) -> Result<Arc<dyn ModelProvider>> {
        <Self as nenjo::ModelProviderFactory>::create_with_base_url(self, provider_name, base_url)
    }

    /// Return provider media capability metadata without requiring runtime
    /// credentials. Capability discovery is static provider metadata; actual
    /// calls still go through authenticated provider instances.
    pub fn media_capabilities(&self, provider_name: &str) -> Option<ProviderMediaCapabilities> {
        let bare_name = provider_name
            .strip_prefix("openai-compatible:")
            .map_or(provider_name, |_| "openai-compatible");
        Self::create_bare(bare_name, "", None, VllmStreaming::Enabled).media_capabilities()
    }

    /// Candidate env var names for a provider, used as a runtime fallback when
    /// the provider isn't in the config map. Providers with non-obvious env var
    /// names get explicit entries; everything else uses `{NAME}_API_KEY`.
    fn env_var_candidates(provider_name: &str) -> Vec<String> {
        match provider_name {
            "google" | "gemini" => vec![
                "GOOGLE_AI_API_KEY".into(),
                "GEMINI_API_KEY".into(),
                "GOOGLE_API_KEY".into(),
            ],
            "anthropic" => vec!["ANTHROPIC_API_KEY".into()],
            _ => vec![format!(
                "{}_API_KEY",
                provider_name.to_uppercase().replace('-', "_"),
            )],
        }
    }

    /// Create a bare (unwrapped) provider for a given name, API key, and optional base URL.
    fn create_bare(
        provider_name: &str,
        api_key: &str,
        base_url: Option<&str>,
        vllm_streaming: VllmStreaming,
    ) -> Box<dyn ModelProvider> {
        let key = Some(api_key);
        match provider_name {
            "anthropic" => Box::new(nenjo_models::AnthropicProvider::new(key)),
            "openai" => Box::new(nenjo_models::OpenAiProvider::new(key)),
            "xai" => {
                let url = base_url.unwrap_or(nenjo_models::XAI_DEFAULT_BASE_URL);
                Box::new(nenjo_models::XAiProvider::with_base_url(key, url))
            }
            "openrouter" => Box::new(nenjo_models::OpenRouterProvider::new(key)),
            "google" | "gemini" => Box::new(nenjo_models::GeminiProvider::new(key)),
            "minimax" => {
                let url = base_url.unwrap_or("https://api.minimax.io/v1");
                Box::new(nenjo_models::OpenAiCompatibleProvider::new(
                    "minimax",
                    url,
                    key,
                    nenjo_models::AuthStyle::Bearer,
                ))
            }
            "ollama" => Box::new(nenjo_models::OllamaProvider::new(base_url)),
            "vllm" => Box::new(nenjo_models::VllmProvider::with_streaming(
                base_url,
                key,
                vllm_streaming,
            )),
            "openai-compatible" => {
                let url = base_url.unwrap_or("http://localhost:8000/v1");
                Box::new(nenjo_models::OpenAiCompatibleProvider::new(
                    "openai-compatible",
                    url,
                    key,
                    nenjo_models::AuthStyle::Bearer,
                ))
            }
            _ => {
                let url = base_url
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| format!("https://api.{provider_name}.com/v1"));
                Box::new(nenjo_models::OpenAiCompatibleProvider::new(
                    provider_name,
                    &url,
                    key,
                    nenjo_models::AuthStyle::Bearer,
                ))
            }
        }
    }

    /// Wrap a primary provider (+ configured fallbacks) in [`ReliableProvider`].
    fn build_reliable(
        &self,
        provider_name: &str,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Result<Arc<dyn ModelProvider>> {
        let mut providers: Vec<(String, Box<dyn ModelProvider>)> = vec![(
            provider_name.to_string(),
            Box::new(AdmissionControlledProvider::new(
                Self::create_bare(provider_name, api_key, base_url, self.vllm_streaming),
                Arc::clone(&self.model_admission),
                self.max_concurrent_requests,
            )),
        )];

        for fallback_name in &self.reliability.fallback_providers {
            if fallback_name == provider_name {
                continue;
            }
            if let Some(fallback_key) = self.api_keys.get(fallback_name.as_str()) {
                providers.push((
                    fallback_name.clone(),
                    Box::new(AdmissionControlledProvider::new(
                        Self::create_bare(fallback_name, fallback_key, None, self.vllm_streaming),
                        Arc::clone(&self.model_admission),
                        self.max_concurrent_requests,
                    )),
                ));
            }
        }

        let reliable = ReliableProvider::new(
            providers,
            self.reliability.max_retries,
            self.reliability.backoff_ms,
        )
        .with_model_fallbacks(self.reliability.model_fallbacks.clone());

        Ok(Arc::new(reliable))
    }

    /// Resolve the API key for an `openai-compatible:{tag}` provider.
    ///
    /// Lookup order:
    /// 1. Config key matching the tag (e.g. `sambanova` in `[model_provider_api_keys]`)
    /// 2. Env var `{TAG}_API_KEY` (e.g. `SAMBANOVA_API_KEY`)
    /// 3. Generic `openai-compatible` config key
    /// 4. Empty string (no auth)
    fn resolve_compatible_key(&self, tag: Option<&str>) -> String {
        let no_key = String::new();

        if let Some(tag) = tag {
            // 1. Config key for the tag
            if let Some(key) = self.api_keys.get(tag) {
                return key.clone();
            }
            // 2. Env var derived from tag
            let env_var = format!("{}_API_KEY", tag.to_uppercase().replace('-', "_"));
            if let Ok(val) = std::env::var(&env_var) {
                debug!(
                    env_var,
                    tag, "Resolved API key from env for compatible provider"
                );
                return val;
            }
        }

        // 3. Generic openai-compatible key, 4. empty
        self.api_keys
            .get("openai-compatible")
            .unwrap_or(&no_key)
            .clone()
    }

    /// Resolve optional vLLM authentication without requiring it for local endpoints.
    fn resolve_vllm_key(&self) -> String {
        self.api_keys
            .get("vllm")
            .cloned()
            .or_else(|| {
                std::env::var("VLLM_API_KEY")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_default()
    }
}

impl MediaCapabilitySource for ModelProviderRegistry {
    fn media_capabilities(&self, provider_name: &str) -> Option<ProviderMediaCapabilities> {
        ModelProviderRegistry::media_capabilities(self, provider_name)
    }
}

impl ArtifactTransportResolver for ModelProviderRegistry {
    fn resolve_transport(
        &self,
        target: ArtifactTransportTarget<'_>,
        media_type: &MediaType,
    ) -> ArtifactInputTransport {
        let bare_name = target
            .provider
            .strip_prefix("openai-compatible:")
            .map_or(target.provider, |_| "openai-compatible");
        Self::create_bare(bare_name, "", target.base_url, self.vllm_streaming)
            .artifact_input_transport(target.model, target.capability, media_type)
    }
}

impl ModelProviderFactory for ModelProviderRegistry {
    fn create(&self, provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        self.create_with_base_url(provider_name, None)
    }

    fn create_with_base_url(
        &self,
        provider_name: &str,
        base_url: Option<&str>,
    ) -> Result<Arc<dyn ModelProvider>> {
        let cache_key = ProviderCacheKey::new(provider_name, base_url);
        if let Some(provider) = self.cache.lock().get(&cache_key).cloned() {
            return Ok(provider);
        }

        // Parse "openai-compatible:{tag}" — the tag drives API key lookup.
        let (bare_name, tag) = if let Some(tag) = provider_name.strip_prefix("openai-compatible:") {
            ("openai-compatible", Some(tag))
        } else {
            (provider_name, None)
        };

        let api_key: String;

        if matches!(bare_name, "ollama" | "openai-compatible" | "vllm") {
            api_key = match bare_name {
                "vllm" => self.resolve_vllm_key(),
                "ollama" => self.resolve_compatible_key(None),
                "openai-compatible" => self.resolve_compatible_key(tag),
                _ => unreachable!("matched local or compatible provider"),
            };
        } else if let Some(key) = self.api_keys.get(bare_name) {
            api_key = key.clone();
        } else {
            // Fall back to env vars at runtime (covers providers that aren't
            // in config.toml but have the env var set).
            let env_candidates = Self::env_var_candidates(bare_name);
            api_key = env_candidates
                .iter()
                .find_map(|var| std::env::var(var).ok().filter(|v| !v.trim().is_empty()))
                .with_context(|| {
                    format!(
                        "no API key configured for provider '{bare_name}'. \
                         Set {} or add it to [model_provider_api_keys] in config.toml",
                        env_candidates.join(" or ")
                    )
                })?;
        }

        let provider = self.build_reliable(bare_name, &api_key, base_url)?;
        self.cache.lock().insert(cache_key, provider.clone());
        Ok(provider)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use nenjo::ModelProviderFactory;
    use nenjo_models::{ChatRequest, ChatResponse, FinishReason, TokenUsage};

    use super::*;

    struct ConcurrencyProbe {
        active: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for ConcurrencyProbe {
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_observed.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: Some("ok".into()),
                tool_calls: Vec::new(),
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
            })
        }
    }

    fn registry_with_openai_key() -> ModelProviderRegistry {
        ModelProviderRegistry::new(
            ModelProviderRegistryConfig::default().with_api_key(ModelProviders::OpenAI, "test-key"),
        )
    }

    #[test]
    fn registry_config_loads_worker_settings_at_the_composition_boundary() {
        let mut worker = WorkerConfig::default();
        worker
            .model_provider_api_keys
            .insert(ModelProviders::OpenAI, "configured-key".to_string());
        worker.reliability.max_retries = 7;
        worker.vllm.streaming = false;
        worker.model_runtime.max_concurrent_requests = 3;

        let config = ModelProviderRegistryConfig::from(&worker);

        assert_eq!(config.api_keys["openai"], "configured-key");
        assert_eq!(config.reliability.max_retries, 7);
        assert_eq!(config.vllm_streaming, VllmStreaming::Disabled);
        assert_eq!(config.max_concurrent_requests, 3);
    }

    #[tokio::test]
    async fn admission_controller_caps_parallel_physical_provider_calls() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(AdmissionControlledProvider::new(
            Box::new(ConcurrencyProbe {
                active: Arc::clone(&active),
                max_observed: Arc::clone(&max_observed),
            }),
            Arc::new(Semaphore::new(1)),
            1,
        ));
        let messages = Vec::new();
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };

        let (first, second) = tokio::join!(
            provider.chat(request, "test", 0.0),
            provider.chat(request, "test", 0.0),
        );

        first.unwrap();
        second.unwrap();
        assert_eq!(max_observed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn caches_provider_for_same_name_and_base_url() {
        let registry = registry_with_openai_key();

        let first = registry.create("openai").unwrap();
        let second = registry.create("openai").unwrap();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn base_url_is_part_of_cache_key() {
        let registry = registry_with_openai_key();

        let first = registry
            .create_with_base_url("openai", Some("https://api.one.example/v1"))
            .unwrap();
        let second = registry
            .create_with_base_url("openai", Some("https://api.two.example/v1"))
            .unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn artifact_transport_discovery_does_not_require_provider_credentials() {
        let registry = ModelProviderRegistry::new(ModelProviderRegistryConfig::default());

        assert!(matches!(
            registry.resolve_transport(
                ArtifactTransportTarget {
                    provider: "openai",
                    model: "gpt-4.1",
                    base_url: None,
                    capability: nenjo_models::ModelCapabilityId::Chat,
                },
                &MediaType::parse("image/png").unwrap(),
            ),
            ArtifactInputTransport::Inline { .. }
        ));
        assert!(matches!(
            registry.resolve_transport(
                ArtifactTransportTarget {
                    provider: "openrouter",
                    model: "google/gemini-3.7-flash",
                    base_url: None,
                    capability: nenjo_models::ModelCapabilityId::Chat,
                },
                &MediaType::parse("text/markdown").unwrap(),
            ),
            ArtifactInputTransport::InlineText { .. }
        ));
        assert!(matches!(
            registry.resolve_transport(
                ArtifactTransportTarget {
                    provider: "vllm",
                    model: "vision-model",
                    base_url: Some("http://localhost:8000/v1"),
                    capability: nenjo_models::ModelCapabilityId::AnalyzeImage,
                },
                &MediaType::parse("image/png").unwrap(),
            ),
            ArtifactInputTransport::Inline { .. }
        ));
        assert!(matches!(
            registry.resolve_transport(
                ArtifactTransportTarget {
                    provider: "openai-compatible:local-stt",
                    model: "whisper",
                    base_url: Some("http://localhost:8001/v1"),
                    capability: nenjo_models::ModelCapabilityId::TranscribeAudio,
                },
                &MediaType::parse("audio/wav").unwrap(),
            ),
            ArtifactInputTransport::Inline { .. }
        ));
        assert!(matches!(
            registry.resolve_transport(
                ArtifactTransportTarget {
                    provider: "vllm",
                    model: "video-model",
                    base_url: Some("http://localhost:8000/v1"),
                    capability: nenjo_models::ModelCapabilityId::AnalyzeVideo,
                },
                &MediaType::parse("video/mp4").unwrap(),
            ),
            ArtifactInputTransport::Inline { .. }
        ));
        assert_eq!(
            registry.resolve_transport(
                ArtifactTransportTarget {
                    provider: "vllm",
                    model: "text-model",
                    base_url: Some("http://localhost:8000/v1"),
                    capability: nenjo_models::ModelCapabilityId::AnalyzeDocument,
                },
                &MediaType::parse("application/pdf").unwrap(),
            ),
            ArtifactInputTransport::Unsupported
        );
    }

    #[test]
    fn vllm_provider_does_not_require_an_api_key() {
        let registry = ModelProviderRegistry::new(ModelProviderRegistryConfig::default());

        assert!(
            registry
                .create_with_base_url("vllm", Some("http://localhost:8000/v1"))
                .is_ok()
        );
    }

    #[test]
    fn openai_compatible_tags_have_distinct_cache_entries() {
        let registry = ModelProviderRegistry::new(
            ModelProviderRegistryConfig::default()
                .with_api_key(ModelProviders::OpenAiCompatible, "default-key"),
        );

        let first = registry
            .create_with_base_url("openai-compatible:first", Some("https://api.example/v1"))
            .unwrap();
        let second = registry
            .create_with_base_url("openai-compatible:second", Some("https://api.example/v1"))
            .unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn xai_provider_exposes_media_capabilities_through_registry() {
        let registry = ModelProviderRegistry::new(
            ModelProviderRegistryConfig::default().with_api_key(ModelProviders::XAI, "test-key"),
        );

        let provider = registry.create("xai").unwrap();
        let capabilities = provider
            .media_capabilities()
            .expect("xai media capabilities");

        assert_eq!(capabilities.provider, "xai");
    }
}
