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
    ToolCall, ToolCategory, ToolResultMessage, ToolSpec, one_shot,
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

// ── Thinking/reasoning helpers ───────────────────────────────────

/// Strip `<think>…</think>` blocks from model output.
///
/// Reasoning models (DeepSeek-reasoner, MiniMax, etc.) emit chain-of-thought
/// wrapped in `<think>` tags. This content is large, not useful for downstream
/// consumers, and wastes bandwidth on the event bus. Call this on
/// `ChatResponse.text` before the text enters the message history or event
/// stream.
pub fn strip_thinking(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find("<think>") {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start..].find("</think>") {
            remaining = &remaining[start + end + "</think>".len()..];
        } else {
            // Unclosed <think> tag — drop everything after it
            return result.trim().to_string();
        }
    }
    result.push_str(remaining);
    result.trim().to_string()
}

// ── Tool name helpers ───────────────────────────────────────────

/// Sanitize a tool function name to match the strict OpenAI pattern `^[a-zA-Z0-9_-]+$`.
///
/// Used by OpenAI, DeepSeek, and other strict providers. Replaces dots, slashes,
/// and any other disallowed characters with `_`.
pub fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Light sanitization for lenient providers (Ollama) — only replaces characters
/// that break JSON-RPC or URL parsing (slashes, spaces, etc.) while preserving
/// dots used in MCP namespaced tool names.
pub fn sanitize_tool_name_lenient(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_thinking_removes_think_block() {
        let input = "<think>\nLet me reason about this...\n</think>\nHello!";
        assert_eq!(strip_thinking(input), "Hello!");
    }

    #[test]
    fn strip_thinking_multiple_blocks() {
        let input = "<think>first</think>A<think>second</think>B";
        assert_eq!(strip_thinking(input), "AB");
    }

    #[test]
    fn strip_thinking_no_tags() {
        assert_eq!(strip_thinking("Just regular text"), "Just regular text");
    }

    #[test]
    fn strip_thinking_empty_think_block() {
        assert_eq!(strip_thinking("<think></think>Hello"), "Hello");
    }

    #[test]
    fn strip_thinking_unclosed_tag() {
        let input = "Before<think>reasoning that never ends...";
        assert_eq!(strip_thinking(input), "Before");
    }

    #[test]
    fn strip_thinking_only_thinking() {
        let input = "<think>All reasoning, no output</think>";
        assert_eq!(strip_thinking(input), "");
    }

    #[test]
    fn sanitize_tool_name_replaces_dots_and_slashes() {
        assert_eq!(
            sanitize_tool_name("app.nenjo.platform/tasks"),
            "app_nenjo_platform_tasks"
        );
    }

    #[test]
    fn sanitize_tool_name_preserves_valid_chars() {
        assert_eq!(sanitize_tool_name("my-tool_v2"), "my-tool_v2");
    }

    #[test]
    fn sanitize_tool_name_lenient_preserves_dots() {
        assert_eq!(
            sanitize_tool_name_lenient("app.nenjo.platform/tasks"),
            "app.nenjo.platform_tasks"
        );
    }
}
