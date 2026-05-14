//! LLM provider re-exports and worker-specific provider infrastructure.
//!
//! Re-exports types from `nenjo-models` and defines `ModelProviders` (the
//! enum used as a HashMap key for API keys in the worker config) and
//! `ProviderRegistry` (implements `ModelProviderFactory` for the nenjo SDK).

pub mod registry;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// Re-export core model types from nenjo-models.
pub use nenjo_models::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, ModelProvider, TokenUsage,
    ToolCall, ToolResultMessage, one_shot,
};

pub use nenjo_models::{
    AnthropicProvider, GeminiProvider, OllamaProvider, OpenAiCompatibleProvider, OpenAiProvider,
    OpenRouterProvider, ReliableProvider, RouterProvider,
};

pub use nenjo_models::{sanitize_api_error, scrub_secret_patterns};

/// Backward-compatible alias: old code used `Provider`, new code uses `ModelProvider`.
pub use nenjo_models::ModelProvider as Provider;

// Re-export registry.
pub use registry::ModelProviderRegistry;

// ---------------------------------------------------------------------------
// ModelProviders enum — config-level concern for API key management
// ---------------------------------------------------------------------------

/// Known LLM provider names. Used as HashMap keys in config.toml for
/// mapping provider names to API keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelProviders {
    OpenAI,
    Anthropic,
    OpenRouter,
    Ollama,
    Groq,
    Venice,
    Mistral,
    Deepseek,
    XAI,
    Together,
    Fireworks,
    Perplexity,
    Cohere,
    Moonshot,
    GLM,
    Google,
    Minimax,
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

impl std::fmt::Display for ModelProviders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
            Self::Ollama => "ollama",
            Self::Groq => "groq",
            Self::Venice => "venice",
            Self::Mistral => "mistral",
            Self::Deepseek => "deepseek",
            Self::XAI => "xai",
            Self::Together => "together",
            Self::Fireworks => "fireworks",
            Self::Perplexity => "perplexity",
            Self::Cohere => "cohere",
            Self::Moonshot => "moonshot",
            Self::GLM => "glm",
            Self::Google => "google",
            Self::Minimax => "minimax",
            Self::OpenAiCompatible => "openai-compatible",
        };
        write!(f, "{name}")
    }
}

/// Returns a map of provider → candidate environment variable names.
///
/// The first non-empty env var wins during config loading.
pub fn provider_env_vars() -> HashMap<ModelProviders, Vec<String>> {
    let mut m = HashMap::new();
    m.insert(ModelProviders::OpenAI, vec!["OPENAI_API_KEY".into()]);
    m.insert(ModelProviders::Anthropic, vec!["ANTHROPIC_API_KEY".into()]);
    m.insert(
        ModelProviders::OpenRouter,
        vec!["OPENROUTER_API_KEY".into()],
    );
    m.insert(ModelProviders::Ollama, vec!["OLLAMA_API_KEY".into()]);
    m.insert(ModelProviders::Groq, vec!["GROQ_API_KEY".into()]);
    m.insert(ModelProviders::Venice, vec!["VENICE_API_KEY".into()]);
    m.insert(ModelProviders::Mistral, vec!["MISTRAL_API_KEY".into()]);
    m.insert(ModelProviders::Deepseek, vec!["DEEPSEEK_API_KEY".into()]);
    m.insert(ModelProviders::XAI, vec!["XAI_API_KEY".into()]);
    m.insert(ModelProviders::Together, vec!["TOGETHER_API_KEY".into()]);
    m.insert(ModelProviders::Fireworks, vec!["FIREWORKS_API_KEY".into()]);
    m.insert(
        ModelProviders::Perplexity,
        vec!["PERPLEXITY_API_KEY".into()],
    );
    m.insert(ModelProviders::Cohere, vec!["COHERE_API_KEY".into()]);
    m.insert(ModelProviders::Moonshot, vec!["MOONSHOT_API_KEY".into()]);
    m.insert(ModelProviders::GLM, vec!["GLM_API_KEY".into()]);
    m.insert(ModelProviders::Minimax, vec!["MINIMAX_API_KEY".into()]);
    m.insert(
        ModelProviders::Google,
        vec!["GOOGLE_AI_API_KEY".into(), "GEMINI_API_KEY".into()],
    );
    m.insert(
        ModelProviders::OpenAiCompatible,
        vec!["OPENAI_COMPATIBLE_API_KEY".into()],
    );
    m
}
