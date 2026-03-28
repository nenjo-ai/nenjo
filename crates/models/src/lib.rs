//! # nenjo-providers
//!
//! LLM provider trait, types, and implementations for the Nenjo agent platform.
//!
//! This crate provides:
//! - The [`ModelProvider`] trait for LLM integrations
//! - Message types: [`ChatMessage`], [`ChatRequest`], [`ChatResponse`], [`ToolCall`]
//! - Provider implementations: Anthropic, OpenAI, Gemini, Ollama, OpenRouter, and
//!   OpenAI-compatible providers
//! - Reliability wrappers: [`ReliableProvider`] (retry/fallback), [`RouterProvider`] (model routing)

pub mod anthropic;
pub mod compatible;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod openrouter;
pub mod reliable;
pub mod router;
pub mod traits;

// Re-export core types at crate root.
pub use traits::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, ModelProvider, TokenUsage,
    ToolCall, ToolResultMessage, one_shot,
};

// Re-export provider implementations.
pub use anthropic::AnthropicProvider;
pub use compatible::{AuthStyle, OpenAiCompatibleProvider};
pub use gemini::GeminiProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;
pub use openrouter::OpenRouterProvider;
pub use reliable::ReliableProvider;
pub use router::RouterProvider;

// ── Error helpers ───────────────────────────────────────────────

const MAX_API_ERROR_CHARS: usize = 200;

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')
}

fn token_end(input: &str, from: usize) -> usize {
    let mut end = from;
    for (i, c) in input[from..].char_indices() {
        if is_secret_char(c) {
            end = from + i + c.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Scrub known secret-like token prefixes from provider error strings.
pub fn scrub_secret_patterns(input: &str) -> String {
    const PREFIXES: [&str; 3] = ["sk-", "xoxb-", "xoxp-"];
    let mut scrubbed = input.to_string();
    for prefix in PREFIXES {
        let mut search_from = 0;
        loop {
            let Some(rel) = scrubbed[search_from..].find(prefix) else {
                break;
            };
            let start = search_from + rel;
            let content_start = start + prefix.len();
            let end = token_end(&scrubbed, content_start);
            if end == content_start {
                search_from = content_start;
                continue;
            }
            scrubbed.replace_range(start..end, "[REDACTED]");
            search_from = start + "[REDACTED]".len();
        }
    }
    scrubbed
}

/// Sanitize API error text by scrubbing secrets and truncating length.
pub fn sanitize_api_error(input: &str) -> String {
    let scrubbed = scrub_secret_patterns(input);
    if scrubbed.chars().count() <= MAX_API_ERROR_CHARS {
        return scrubbed;
    }
    let mut end = MAX_API_ERROR_CHARS;
    while end > 0 && !scrubbed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &scrubbed[..end])
}

/// Build a sanitized provider error from a failed HTTP response.
pub async fn api_error(provider: &str, response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<failed to read provider error body>".to_string());
    let sanitized = sanitize_api_error(&body);
    anyhow::anyhow!("{provider} API error ({status}): {sanitized}")
}
