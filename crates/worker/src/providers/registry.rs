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

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tracing::debug;

use nenjo::ModelProviderFactory;
use nenjo_models::ModelProvider;
use nenjo_models::ReliableProvider;

use super::ModelProviders;
use crate::config::ReliabilityConfig;

/// Registry that creates LLM provider instances on demand.
///
/// Implements `ModelProviderFactory` so it can be passed to `Provider::from_manifest()`.
/// Each created provider is wrapped in [`ReliableProvider`] for automatic retries
/// with exponential backoff, rate-limit handling, and model fallback.
pub struct ModelProviderRegistry {
    api_keys: HashMap<String, String>,
    reliability: ReliabilityConfig,
    cache: Mutex<HashMap<ProviderCacheKey, Arc<dyn ModelProvider>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderCacheKey {
    provider_name: String,
    base_url: Option<String>,
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
    /// Create a new registry from the config's model provider API keys.
    pub fn new(keys: &HashMap<ModelProviders, String>, reliability: &ReliabilityConfig) -> Self {
        let api_keys: HashMap<String, String> = keys
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

        debug!(providers = api_keys.len(), "ProviderRegistry initialized");

        Self {
            api_keys,
            reliability: reliability.clone(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Look up the API key for a provider name.
    pub fn api_key(&self, provider_name: &str) -> Option<&str> {
        self.api_keys.get(provider_name).map(|s| s.as_str())
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
    ) -> Box<dyn ModelProvider> {
        let key = Some(api_key);
        match provider_name {
            "anthropic" => Box::new(nenjo_models::AnthropicProvider::new(key)),
            "openai" => Box::new(nenjo_models::OpenAiProvider::new(key)),
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
            Self::create_bare(provider_name, api_key, base_url),
        )];

        for fallback_name in &self.reliability.fallback_providers {
            if fallback_name == provider_name {
                continue;
            }
            if let Some(fallback_key) = self.api_keys.get(fallback_name.as_str()) {
                providers.push((
                    fallback_name.clone(),
                    Self::create_bare(fallback_name, fallback_key, None),
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

        if matches!(bare_name, "ollama" | "openai-compatible") {
            api_key = self.resolve_compatible_key(if bare_name == "ollama" { None } else { tag });
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use nenjo::ModelProviderFactory;

    use super::*;

    fn registry_with_openai_key() -> ModelProviderRegistry {
        let mut keys = HashMap::new();
        keys.insert(ModelProviders::OpenAI, "test-key".to_string());
        ModelProviderRegistry::new(&keys, &ReliabilityConfig::default())
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
    fn openai_compatible_tags_have_distinct_cache_entries() {
        let mut keys = HashMap::new();
        keys.insert(ModelProviders::OpenAiCompatible, "default-key".to_string());
        let registry = ModelProviderRegistry::new(&keys, &ReliabilityConfig::default());

        let first = registry
            .create_with_base_url("openai-compatible:first", Some("https://api.example/v1"))
            .unwrap();
        let second = registry
            .create_with_base_url("openai-compatible:second", Some("https://api.example/v1"))
            .unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
    }
}
