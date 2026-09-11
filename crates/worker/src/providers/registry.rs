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
use std::time::Duration;

use anyhow::{Context, Result};
use nenjo::concurrency::AdmissionPool;
use parking_lot::Mutex;
use tracing::debug;

use nenjo::ModelProviderFactory;
use nenjo_models::{ArtifactInputTransport, MediaType, ModelProvider, ProviderMediaCapabilities};
use nenjo_models::{ReliableProvider, VllmStreaming};

use super::ModelProviders;
use super::admission::AdmissionControlledProvider;
use crate::config::{
    Config as WorkerConfig, ModelRuntimeConfig, ProviderPoolConfig, ReliabilityConfig,
};
use crate::media::{ArtifactTransportResolver, ArtifactTransportTarget, MediaCapabilitySource};

/// Complete configuration required to construct a model provider registry.
#[derive(Clone)]
pub struct ModelProviderRegistryConfig {
    api_keys: HashMap<String, String>,
    reliability: ReliabilityConfig,
    vllm_streaming: VllmStreaming,
    runtime: ModelRuntimeConfig,
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

    /// Configure named provider pools and bounded admission queues.
    pub fn with_runtime(mut self, runtime: ModelRuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn with_max_concurrent_requests(mut self, max: usize) -> Self {
        self.runtime.max_concurrent_requests = max.max(1);
        self
    }
}

impl Default for ModelProviderRegistryConfig {
    fn default() -> Self {
        Self {
            api_keys: HashMap::new(),
            reliability: ReliabilityConfig::default(),
            vllm_streaming: VllmStreaming::Enabled,
            runtime: ModelRuntimeConfig::default(),
        }
    }
}

impl From<&WorkerConfig> for ModelProviderRegistryConfig {
    fn from(config: &WorkerConfig) -> Self {
        Self::default()
            .with_api_keys(&config.model_provider_api_keys)
            .with_reliability(config.reliability.clone())
            .with_vllm_streaming(config.vllm.streaming.into())
            .with_runtime(config.model_runtime.clone())
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
    model_admission: Mutex<HashMap<AdmissionKey, AdmissionPool>>,
    runtime: ModelRuntimeConfig,
    cache: Mutex<HashMap<ProviderCacheKey, Arc<dyn ModelProvider>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderCacheKey {
    provider_name: String,
    base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AdmissionKey {
    Named(String),
    Provider(ProviderCacheKey),
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
            model_admission: Mutex::new(HashMap::new()),
            runtime: config.runtime,
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
        Self::create_bare_with_client(provider_name, api_key, base_url, vllm_streaming, None)
    }

    fn create_bare_with_client(
        provider_name: &str,
        api_key: &str,
        base_url: Option<&str>,
        vllm_streaming: VllmStreaming,
        client: Option<reqwest::Client>,
    ) -> Box<dyn ModelProvider> {
        macro_rules! configured {
            ($provider:expr) => {{
                let provider = $provider;
                Box::new(match client {
                    Some(client) => provider.with_http_client(client),
                    None => provider,
                })
            }};
        }
        let key = Some(api_key);
        match provider_name {
            "anthropic" => configured!(nenjo_models::AnthropicProvider::new(key)),
            "openai" => configured!(nenjo_models::OpenAiProvider::new(key)),
            "xai" => {
                let url = base_url.unwrap_or(nenjo_models::XAI_DEFAULT_BASE_URL);
                configured!(nenjo_models::XAiProvider::with_base_url(key, url))
            }
            "openrouter" => configured!(nenjo_models::OpenRouterProvider::new(key)),
            "google" | "gemini" => configured!(nenjo_models::GeminiProvider::new(key)),
            "minimax" => {
                let url = base_url.unwrap_or("https://api.minimax.io/v1");
                configured!(nenjo_models::OpenAiCompatibleProvider::new(
                    "minimax",
                    url,
                    key,
                    nenjo_models::AuthStyle::Bearer,
                ))
            }
            "ollama" => configured!(nenjo_models::OllamaProvider::new(base_url)),
            "vllm" => configured!(nenjo_models::VllmProvider::with_streaming(
                base_url,
                key,
                vllm_streaming,
            )),
            "openai-compatible" => {
                let url = base_url.unwrap_or("http://localhost:8000/v1");
                configured!(nenjo_models::OpenAiCompatibleProvider::new(
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
                configured!(nenjo_models::OpenAiCompatibleProvider::new(
                    provider_name,
                    &url,
                    key,
                    nenjo_models::AuthStyle::Bearer,
                ))
            }
        }
    }

    /// Apply HTTP deadlines beneath admission so queued requests do not time out.
    fn http_client(
        &self,
        provider_name: &str,
        base_url: Option<&str>,
    ) -> Result<Option<reqwest::Client>> {
        let (_, pool) = self.pool_config(provider_name, base_url);
        let config = ReliabilityConfig {
            request_timeout_secs: pool
                .request_timeout_secs
                .or(self.reliability.request_timeout_secs),
            read_timeout_secs: pool
                .read_timeout_secs
                .or(self.reliability.read_timeout_secs),
            connect_timeout_secs: pool
                .connect_timeout_secs
                .or(self.reliability.connect_timeout_secs),
            ..self.reliability.clone()
        };
        if config.request_timeout_secs.is_none()
            && config.read_timeout_secs.is_none()
            && config.connect_timeout_secs.is_none()
        {
            return Ok(None);
        }
        let (default_request, default_read) = match provider_name {
            "ollama" => (300, 0),
            "openai" | "anthropic" | "xai" | "openrouter" | "google" | "gemini" => (120, 0),
            _ => (0, 300),
        };
        let mut builder = reqwest::Client::builder();
        let request = config.request_timeout_secs.unwrap_or(default_request);
        let read = config.read_timeout_secs.unwrap_or(default_read);
        let connect = config.connect_timeout_secs.unwrap_or(10);
        if request > 0 {
            builder = builder.timeout(Duration::from_secs(request));
        }
        if read > 0 {
            builder = builder.read_timeout(Duration::from_secs(read));
        }
        if connect > 0 {
            builder = builder.connect_timeout(Duration::from_secs(connect));
        }
        Ok(Some(
            builder
                .build()
                .context("Failed to configure model HTTP timeouts")?,
        ))
    }

    fn pool_config(
        &self,
        provider_name: &str,
        base_url: Option<&str>,
    ) -> (AdmissionKey, ProviderPoolConfig) {
        let binding = self
            .runtime
            .bindings
            .iter()
            .filter(|binding| binding.provider == provider_name)
            .find(|binding| binding.base_url.is_some() && binding.base_url.as_deref() == base_url)
            .or_else(|| {
                self.runtime
                    .bindings
                    .iter()
                    .find(|binding| binding.provider == provider_name && binding.base_url.is_none())
            });
        if let Some(binding) = binding {
            return (
                AdmissionKey::Named(binding.pool.clone()),
                self.runtime.pools[&binding.pool].clone(),
            );
        }
        (
            AdmissionKey::Provider(ProviderCacheKey::new(provider_name, base_url)),
            ProviderPoolConfig {
                max_concurrent_requests: self.runtime.max_concurrent_requests,
                max_queued_requests: self.runtime.max_queued_requests,
                queue_timeout_secs: self.runtime.queue_timeout_secs,
                ..ProviderPoolConfig::default()
            },
        )
    }

    /// Direct calls, retries, aliases, and fallbacks share their destination resource pool.
    fn admission_for(&self, provider_name: &str, base_url: Option<&str>) -> AdmissionPool {
        let (key, config) = self.pool_config(provider_name, base_url);
        self.model_admission
            .lock()
            .entry(key.clone())
            .or_insert_with(|| {
                let name = match key {
                    AdmissionKey::Named(name) => name,
                    AdmissionKey::Provider(provider) => format!(
                        "{} at {}",
                        provider.provider_name,
                        provider.base_url.as_deref().unwrap_or("default endpoint")
                    ),
                };
                AdmissionPool::new(
                    name,
                    config.max_concurrent_requests,
                    config.max_queued_requests,
                    Duration::from_secs(config.queue_timeout_secs),
                )
            })
            .clone()
    }

    /// Wrap a primary provider (+ configured fallbacks) in [`ReliableProvider`].
    fn build_reliable(
        &self,
        provider_name: &str,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Result<Arc<dyn ModelProvider>> {
        let bare_name = provider_name
            .strip_prefix("openai-compatible:")
            .map_or(provider_name, |_| "openai-compatible");
        let mut providers: Vec<(String, Box<dyn ModelProvider>)> = vec![(
            provider_name.to_string(),
            Box::new(AdmissionControlledProvider::new(
                Self::create_bare_with_client(
                    bare_name,
                    api_key,
                    base_url,
                    self.vllm_streaming,
                    self.http_client(provider_name, base_url)?,
                ),
                self.admission_for(provider_name, base_url),
                self.admission_for(provider_name, base_url).limit(),
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
                        Self::create_bare_with_client(
                            fallback_name,
                            fallback_key,
                            None,
                            self.vllm_streaming,
                            self.http_client(fallback_name, None)?,
                        ),
                        self.admission_for(fallback_name, None),
                        self.admission_for(fallback_name, None).limit(),
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
        self.runtime.validate()?;
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

        let provider = self.build_reliable(provider_name, &api_key, base_url)?;
        self.cache.lock().insert(cache_key, provider.clone());
        Ok(provider)
    }
}

#[cfg(test)]
mod tests {
    use nenjo::ModelProviderFactory;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn named_pool_bindings_share_alias_capacity_and_allow_independent_limits() {
        let runtime: ModelRuntimeConfig = toml::from_str(
            r#"
[pools.local]
max_concurrent_requests = 1
max_queued_requests = 0
read_timeout_secs = 600
[pools.remote]
max_concurrent_requests = 10
[[bindings]]
provider = "vllm"
pool = "local"
[[bindings]]
provider = "openai-compatible:alias"
pool = "local"
[[bindings]]
provider = "openai"
pool = "remote"
[[bindings]]
provider = "vllm"
base_url = "http://another-server/v1"
pool = "remote"
"#,
        )
        .unwrap();
        runtime.validate().unwrap();
        let registry = ModelProviderRegistry::new(
            ModelProviderRegistryConfig::default().with_runtime(runtime),
        );
        let local = registry.admission_for("vllm", None);
        let _held = local.acquire().await.unwrap();
        assert!(
            registry
                .admission_for("openai-compatible:alias", Some("http://localhost/v1"))
                .acquire()
                .await
                .is_err()
        );
        assert_eq!(registry.admission_for("openai", None).limit(), 10);
        assert_eq!(
            registry
                .admission_for("vllm", Some("http://another-server/v1"))
                .limit(),
            10
        );
        registry
            .admission_for("openai", None)
            .acquire()
            .await
            .unwrap();
        assert_eq!(
            registry.pool_config("vllm", None).1.read_timeout_secs,
            Some(600)
        );
    }

    #[tokio::test]
    async fn admission_is_shared_by_configuration_and_independent_across_providers() {
        let registry = ModelProviderRegistry::new(
            ModelProviderRegistryConfig::default().with_max_concurrent_requests(1),
        );
        let local = registry.admission_for("vllm", Some("http://localhost:8000"));
        let held = local.acquire().await.unwrap();
        let same = registry.admission_for("vllm", Some("http://localhost:8000"));
        let mut waiting = Box::pin(same.acquire());
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
        for (name, url) in [
            ("openai", None),
            ("vllm", Some("http://localhost:9000")),
            ("openai-compatible:a", None),
        ] {
            registry.admission_for(name, url).acquire().await.unwrap();
        }
        drop(held);
        waiting.await.unwrap();
    }

    #[tokio::test]
    async fn tagged_provider_calls_wait_on_their_configuration_pool() {
        let registry = ModelProviderRegistry::new(
            ModelProviderRegistryConfig::default().with_max_concurrent_requests(1),
        );
        let name = "openai-compatible:local";
        let url = Some("http://localhost:8000/v1");
        let gate = registry.admission_for(name, url);
        let _held = gate.acquire().await.unwrap();
        let provider = registry.provider_with_base_url(name, url).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let request = nenjo_models::ChatRequest {
            messages: &[],
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };
        let mut call = Box::pin(provider.chat_stream(request, "local", 0.0, tx));
        assert!(futures_util::poll!(call.as_mut()).is_pending());
        assert!(matches!(
            rx.try_recv().unwrap(),
            nenjo_models::ProviderStreamEvent::CapacityWaiting { limit: 1 }
        ));
        assert_eq!(registry.model_admission.lock().len(), 1);
    }

    #[test]
    fn direct_and_fallback_instances_share_destination_admission() {
        let registry = ModelProviderRegistry::new(
            ModelProviderRegistryConfig::default()
                .with_api_key(ModelProviders::OpenAI, "test")
                .with_api_key(ModelProviders::Anthropic, "test")
                .with_reliability(ReliabilityConfig {
                    fallback_providers: vec!["anthropic".into()],
                    ..ReliabilityConfig::default()
                }),
        );
        registry.provider("openai").unwrap();
        let fallback = registry.admission_for("anthropic", None);
        registry.provider("anthropic").unwrap();
        assert_eq!(
            fallback.limit(),
            registry.admission_for("anthropic", None).limit()
        );
        assert_eq!(registry.model_admission.lock().len(), 2);
    }

    #[test]
    fn reliability_timeouts_deserialize_and_preserve_defaults() {
        let defaults: ReliabilityConfig = toml::from_str("").unwrap();
        assert_eq!(defaults.request_timeout_secs, None);
        assert_eq!(defaults.read_timeout_secs, None);
        assert_eq!(defaults.connect_timeout_secs, None);
        let worker = WorkerConfig {
            reliability: toml::from_str(
                "request_timeout_secs = 0\nread_timeout_secs = 600\nconnect_timeout_secs = 20",
            )
            .unwrap(),
            ..WorkerConfig::default()
        };
        let config = ModelProviderRegistryConfig::from(&worker);
        assert_eq!(config.reliability.request_timeout_secs, Some(0));
        assert_eq!(config.reliability.read_timeout_secs, Some(600));
        assert_eq!(config.reliability.connect_timeout_secs, Some(20));
        assert!(toml::from_str::<ReliabilityConfig>("read_timeout_secs = -1").is_err());
    }

    #[tokio::test]
    async fn configured_timeouts_reach_local_providers_through_reliable_wrapper() {
        for name in ["ollama", "vllm", "openai-compatible"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                tokio::time::sleep(Duration::from_secs(10)).await;
                drop(socket);
            });
            let registry = ModelProviderRegistry::new(
                ModelProviderRegistryConfig::default().with_reliability(ReliabilityConfig {
                    request_timeout_secs: Some(1),
                    max_retries: 0,
                    ..ReliabilityConfig::default()
                }),
            );
            let provider = registry.provider_with_base_url(name, Some(&url)).unwrap();
            let request = nenjo_models::ChatRequest {
                messages: &[],
                tools: None,
                native_tools: None,
                prepared_artifacts: None,
            };
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                provider.chat(request, "local-model", 0.0),
            )
            .await;
            server.abort();
            let error = result
                .expect("configured deadline must beat the default")
                .unwrap_err();
            assert!(
                error.to_string().contains("All providers/models failed"),
                "{name}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn idle_timeout_allows_active_reads_beyond_total_deadline_and_rejects_stalls() {
        for (request_timeout, delay_ms, succeeds) in
            [(0, 600, true), (1, 600, false), (0, 1500, false)]
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
                for _ in 0..3 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    if socket.write_all(b"x").await.is_err() {
                        break;
                    }
                }
            });
            let registry = ModelProviderRegistry::new(
                ModelProviderRegistryConfig::default().with_reliability(ReliabilityConfig {
                    request_timeout_secs: Some(request_timeout),
                    read_timeout_secs: Some(1),
                    ..ReliabilityConfig::default()
                }),
            );
            let client = registry.http_client("ollama", None).unwrap().unwrap();
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                client.get(&url).send().await?.text().await
            })
            .await
            .expect("HTTP timeout must terminate stalled reads");
            server.abort();
            if succeeds {
                assert_eq!(result.unwrap(), "xxx");
            } else {
                assert!(result.unwrap_err().is_timeout());
            }
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
        assert_eq!(config.runtime.max_concurrent_requests, 3);
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
