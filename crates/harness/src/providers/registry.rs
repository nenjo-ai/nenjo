//! Provider registry — implements `ModelProviderFactory` for the nenjo SDK.
//!
//! Maps provider name strings (e.g. "openai", "anthropic") to concrete
//! `ModelProvider` implementations, using API keys from the harness config.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
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
pub struct ProviderRegistry {
    api_keys: HashMap<String, String>,
    reliability: ReliabilityConfig,
}

impl ProviderRegistry {
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
        }
    }

    /// Look up the API key for a provider name.
    pub fn api_key(&self, provider_name: &str) -> Option<&str> {
        self.api_keys.get(provider_name).map(|s| s.as_str())
    }
}

impl ProviderRegistry {
    /// Create a bare (unwrapped) provider for a given name and API key.
    fn create_bare(provider_name: &str, api_key: &str) -> Box<dyn ModelProvider> {
        let key = Some(api_key);
        match provider_name {
            "anthropic" => Box::new(nenjo_models::AnthropicProvider::new(key)),
            "openai" => Box::new(nenjo_models::OpenAiProvider::new(key)),
            "openrouter" => Box::new(nenjo_models::OpenRouterProvider::new(key)),
            "google" | "gemini" => Box::new(nenjo_models::GeminiProvider::new(key)),
            "ollama" => Box::new(nenjo_models::OllamaProvider::new(None)),
            _ => {
                let base_url = format!("https://api.{provider_name}.com/v1");
                Box::new(nenjo_models::OpenAiCompatibleProvider::new(
                    provider_name,
                    &base_url,
                    key,
                    nenjo_models::AuthStyle::Bearer,
                ))
            }
        }
    }
}

impl ModelProviderFactory for ProviderRegistry {
    fn create(&self, provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        let api_key = self
            .api_keys
            .get(provider_name)
            .with_context(|| format!("no API key configured for provider '{provider_name}'"))?;

        // Build the primary provider + any configured fallback providers.
        let mut providers: Vec<(String, Box<dyn ModelProvider>)> = vec![(
            provider_name.to_string(),
            Self::create_bare(provider_name, api_key),
        )];

        for fallback_name in &self.reliability.fallback_providers {
            if fallback_name == provider_name {
                continue;
            }
            if let Some(fallback_key) = self.api_keys.get(fallback_name.as_str()) {
                providers.push((
                    fallback_name.clone(),
                    Self::create_bare(fallback_name, fallback_key),
                ));
            }
        }

        // Wrap with ReliableProvider for retries, backoff, and fallback.
        let reliable = ReliableProvider::new(
            providers,
            self.reliability.max_retries,
            self.reliability.backoff_ms,
        )
        .with_model_fallbacks(self.reliability.model_fallbacks.clone());

        Ok(Arc::new(reliable))
    }
}
