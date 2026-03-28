//! Multi-model router that dispatches requests to different provider+model
//! combinations based on hint-prefixed model names.

use crate::ModelProvider;
use crate::traits::{ChatRequest, ChatResponse};
use async_trait::async_trait;
use std::collections::HashMap;

/// A single route: maps a task hint to a provider + model combo.
#[derive(Debug, Clone)]
pub struct Route {
    pub provider_name: String,
    pub model: String,
}

/// Multi-model router — routes requests to different provider+model combos
/// based on a task hint encoded in the model parameter.
///
/// The model parameter can be:
/// - A regular model name (e.g. "anthropic/claude-sonnet-4") → uses default provider
/// - A hint-prefixed string (e.g. "hint:reasoning") → resolves via route table
///
/// This wraps multiple pre-created providers and selects the right one per request.
pub struct RouterProvider {
    routes: HashMap<String, (usize, String)>, // hint → (provider_index, model)
    providers: Vec<(String, Box<dyn ModelProvider>)>,
    default_index: usize,
}

impl RouterProvider {
    /// Create a new router with a default provider and optional routes.
    ///
    /// `providers` is a list of (name, provider) pairs. The first one is the default.
    /// `routes` maps hint names to Route structs containing provider_name and model.
    pub fn new(
        providers: Vec<(String, Box<dyn ModelProvider>)>,
        routes: Vec<(String, Route)>,
        _default_model: String,
    ) -> Self {
        // Build provider name → index lookup
        let name_to_index: HashMap<&str, usize> = providers
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (name.as_str(), i))
            .collect();

        // Resolve routes to provider indices
        let resolved_routes: HashMap<String, (usize, String)> = routes
            .into_iter()
            .filter_map(|(hint, route)| {
                let index = name_to_index.get(route.provider_name.as_str()).copied();
                match index {
                    Some(i) => Some((hint, (i, route.model))),
                    None => {
                        tracing::warn!(
                            hint = hint,
                            provider = route.provider_name,
                            "Route references unknown provider, skipping"
                        );
                        None
                    }
                }
            })
            .collect();

        Self {
            routes: resolved_routes,
            providers,
            default_index: 0,
        }
    }

    /// Resolve a model parameter to a (provider, actual_model) pair.
    ///
    /// If the model starts with "hint:", look up the hint in the route table.
    /// Otherwise, use the default provider with the given model name.
    /// Resolve a model parameter to a (provider_index, actual_model) pair.
    fn resolve(&self, model: &str) -> (usize, String) {
        if let Some(hint) = model.strip_prefix("hint:") {
            if let Some((idx, resolved_model)) = self.routes.get(hint) {
                return (*idx, resolved_model.clone());
            }
            tracing::warn!(
                hint = hint,
                "Unknown route hint, falling back to default provider"
            );
        }

        // Not a hint or hint not found — use default provider with the model as-is
        (self.default_index, model.to_string())
    }
}

#[async_trait]
impl ModelProvider for RouterProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let (provider_idx, resolved_model) = self.resolve(model);
        let (_, provider) = &self.providers[provider_idx];
        provider.chat(request, &resolved_model, temperature).await
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        self.providers
            .get(self.default_index)
            .and_then(|(_, p)| p.context_window(model))
    }

    fn supports_native_tools(&self) -> bool {
        self.providers
            .get(self.default_index)
            .map(|(_, p)| p.supports_native_tools())
            .unwrap_or(false)
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        self.providers
            .get(self.default_index)
            .map(|(_, p)| p.supports_developer_role(model))
            .unwrap_or(false)
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        for (name, provider) in &self.providers {
            tracing::info!(provider = name, "Warming up routed provider");
            if let Err(e) = provider.warmup().await {
                tracing::warn!(provider = name, "Warmup failed (non-fatal): {e}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{ChatMessage, ChatRequest, ChatResponse, TokenUsage, one_shot};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        response: &'static str,
        last_model: std::sync::Mutex<String>,
    }

    impl MockProvider {
        fn new(response: &'static str) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                response,
                last_model: std::sync::Mutex::new(String::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn last_model(&self) -> String {
            self.last_model.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_model.lock().unwrap() = model.to_string();
            Ok(ChatResponse {
                text: Some(self.response.to_string()),
                tool_calls: vec![],
                usage: TokenUsage::default(),
            })
        }
    }

    fn make_router(
        providers: Vec<(&'static str, &'static str)>,
        routes: Vec<(&str, &str, &str)>,
    ) -> (RouterProvider, Vec<Arc<MockProvider>>) {
        let mocks: Vec<Arc<MockProvider>> = providers
            .iter()
            .map(|(_, response)| Arc::new(MockProvider::new(response)))
            .collect();

        let provider_list: Vec<(String, Box<dyn ModelProvider>)> = providers
            .iter()
            .zip(mocks.iter())
            .map(|((name, _), mock)| {
                (
                    name.to_string(),
                    Box::new(Arc::clone(mock)) as Box<dyn ModelProvider>,
                )
            })
            .collect();

        let route_list: Vec<(String, Route)> = routes
            .iter()
            .map(|(hint, provider_name, model)| {
                (
                    hint.to_string(),
                    Route {
                        provider_name: provider_name.to_string(),
                        model: model.to_string(),
                    },
                )
            })
            .collect();

        let router = RouterProvider::new(provider_list, route_list, "default-model".to_string());

        (router, mocks)
    }

    // Arc<MockProvider> should also be a Provider
    #[async_trait]
    impl ModelProvider for Arc<MockProvider> {
        async fn chat(
            &self,
            request: ChatRequest<'_>,
            model: &str,
            temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.as_ref().chat(request, model, temperature).await
        }
    }

    #[tokio::test]
    async fn routes_hint_to_correct_provider() {
        let (router, mocks) = make_router(
            vec![("fast", "fast-response"), ("smart", "smart-response")],
            vec![
                ("fast", "fast", "llama-3-70b"),
                ("reasoning", "smart", "claude-opus"),
            ],
        );

        let result = one_shot(&router, None, "hello", "hint:reasoning", 0.5)
            .await
            .unwrap();
        assert_eq!(result, "smart-response");
        assert_eq!(mocks[1].call_count(), 1);
        assert_eq!(mocks[1].last_model(), "claude-opus");
        assert_eq!(mocks[0].call_count(), 0);
    }

    #[tokio::test]
    async fn routes_fast_hint() {
        let (router, mocks) = make_router(
            vec![("fast", "fast-response"), ("smart", "smart-response")],
            vec![("fast", "fast", "llama-3-70b")],
        );

        let result = one_shot(&router, None, "hello", "hint:fast", 0.5)
            .await
            .unwrap();
        assert_eq!(result, "fast-response");
        assert_eq!(mocks[0].call_count(), 1);
        assert_eq!(mocks[0].last_model(), "llama-3-70b");
    }

    #[tokio::test]
    async fn unknown_hint_falls_back_to_default() {
        let (router, mocks) = make_router(
            vec![("default", "default-response"), ("other", "other-response")],
            vec![],
        );

        let result = one_shot(&router, None, "hello", "hint:nonexistent", 0.5)
            .await
            .unwrap();
        assert_eq!(result, "default-response");
        assert_eq!(mocks[0].call_count(), 1);
        // Falls back to default with the hint as model name
        assert_eq!(mocks[0].last_model(), "hint:nonexistent");
    }

    #[tokio::test]
    async fn non_hint_model_uses_default_provider() {
        let (router, mocks) = make_router(
            vec![
                ("primary", "primary-response"),
                ("secondary", "secondary-response"),
            ],
            vec![("code", "secondary", "codellama")],
        );

        let result = one_shot(
            &router,
            None,
            "hello",
            "anthropic/claude-sonnet-4-20250514",
            0.5,
        )
        .await
        .unwrap();
        assert_eq!(result, "primary-response");
        assert_eq!(mocks[0].call_count(), 1);
        assert_eq!(mocks[0].last_model(), "anthropic/claude-sonnet-4-20250514");
    }

    #[test]
    fn resolve_preserves_model_for_non_hints() {
        let (router, _) = make_router(vec![("default", "ok")], vec![]);

        let (idx, model) = router.resolve("gpt-4o");
        assert_eq!(idx, 0);
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn resolve_strips_hint_prefix() {
        let (router, _) = make_router(
            vec![("fast", "ok"), ("smart", "ok")],
            vec![("reasoning", "smart", "claude-opus")],
        );

        let (idx, model) = router.resolve("hint:reasoning");
        assert_eq!(idx, 1);
        assert_eq!(model, "claude-opus");
    }

    #[test]
    fn skips_routes_with_unknown_provider() {
        let (router, _) = make_router(
            vec![("default", "ok")],
            vec![("broken", "nonexistent", "model")],
        );

        // Route should not exist
        assert!(!router.routes.contains_key("broken"));
    }

    #[tokio::test]
    async fn warmup_calls_all_providers() {
        let (router, _) = make_router(vec![("a", "ok"), ("b", "ok")], vec![]);

        // Warmup should not error
        assert!(router.warmup().await.is_ok());
    }

    #[tokio::test]
    async fn chat_dispatches_to_correct_provider() {
        let mock = Arc::new(MockProvider::new("response"));
        let router = RouterProvider::new(
            vec![(
                "default".into(),
                Box::new(Arc::clone(&mock)) as Box<dyn ModelProvider>,
            )],
            vec![],
            "model".into(),
        );

        let result = one_shot(&router, Some("system"), "hello", "model", 0.5)
            .await
            .unwrap();
        assert_eq!(result, "response");
        assert_eq!(mock.call_count(), 1);
    }
}
