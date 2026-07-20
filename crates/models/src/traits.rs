use async_trait::async_trait;
pub use nenjo_tool_api::{ToolCall, ToolCategory, ToolResultMessage, ToolSpec};
use serde::{Deserialize, Serialize};

use crate::native::{
    NativeMediaJob, NativeMediaRequest, NativeMediaResponse, NativeModelToolId,
    ProviderMediaCapabilities,
};

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
        }
    }

    pub fn developer(content: impl Into<String>) -> Self {
        Self {
            role: "developer".into(),
            content: content.into(),
        }
    }
}

/// Token usage reported by the LLM provider.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A provider-executed tool call observed inside a model response.
///
/// These traces are informational only. They must not be fed to the local tool
/// executor because the provider has already executed the tool server-side.
#[derive(Debug, Clone)]
pub struct ProviderToolTrace {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub citations: Vec<serde_json::Value>,
}

/// An LLM response that may contain text, tool calls, or both.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Text content of the response (may be empty if only tool calls).
    pub text: Option<String>,
    /// Tool calls requested by the LLM for the local runtime to execute.
    pub tool_calls: Vec<ToolCall>,
    /// Provider-executed tool calls observed in the model response.
    pub provider_tool_calls: Vec<ProviderToolTrace>,
    /// Token usage reported by the provider (zeros when not available).
    pub usage: TokenUsage,
}

/// Incremental events emitted while a provider-native model request is running.
///
/// These events are provider-agnostic and intentionally lossy: they carry the
/// information the worker needs to update live activity without baking a single
/// vendor's raw streaming schema into the turn loop.
#[derive(Debug, Clone)]
pub enum ProviderStreamEvent {
    TextDelta(String),
    ProviderToolStarted(ProviderToolTrace),
    ProviderToolCompleted(ProviderToolTrace),
}

impl ChatResponse {
    /// True when the LLM wants to invoke at least one tool.
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// Convenience: return text content or empty string.
    pub fn text_or_empty(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

/// Request payload for provider chat calls.
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
    pub native_tools: Option<&'a [NativeModelToolId]>,
}

/// A message in a multi-turn conversation, including tool interactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ConversationMessage {
    /// Regular chat message (system, user, assistant).
    Chat(ChatMessage),
    /// Tool calls from the assistant (stored for history fidelity).
    AssistantToolCalls {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    /// Results of tool executions, fed back to the LLM.
    ToolResults(Vec<ToolResultMessage>),
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Structured chat API — the single required method.
    ///
    /// Accepts a full conversation (system + user + assistant + tool messages)
    /// plus optional tool definitions. Returns text and/or tool calls.
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse>;

    /// Optional streaming chat API.
    ///
    /// Providers that can surface incremental model or provider-native tool
    /// progress should override this. The default implementation preserves the
    /// existing non-streaming behavior.
    async fn chat_stream(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        events: tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
    ) -> anyhow::Result<ChatResponse> {
        let _ = events;
        self.chat(request, model, temperature).await
    }

    /// Context window size in tokens for the given model.
    ///
    /// Providers return the raw advertised context window. The turn loop
    /// applies its own safety margin. Returns `None` if the model is
    /// unknown; the turn loop falls back to a conservative default.
    fn context_window(&self, _model: &str) -> Option<usize> {
        None
    }

    /// Whether provider supports native tool calls over API.
    fn supports_native_tools(&self) -> bool {
        false
    }

    /// Whether the given model supports the `developer` message role (OpenAI-spec).
    /// When true, app-owned instructions are sent as a developer message.
    /// When false, they are folded into the provider's system-equivalent role.
    fn supports_developer_role(&self, _model: &str) -> bool {
        false
    }

    /// Provider media capabilities outside the chat/tool turn loop.
    ///
    /// Examples include direct image generation, async video rendering,
    /// text-to-speech, and speech-to-text endpoints.
    fn media_capabilities(&self) -> Option<ProviderMediaCapabilities> {
        None
    }

    /// Submit a provider media operation.
    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        anyhow::bail!(
            "provider does not support media operation {:?}",
            request.operation()
        )
    }

    /// Poll an async provider media job.
    async fn poll_media_job(&self, job: &NativeMediaJob) -> anyhow::Result<NativeMediaResponse> {
        let _ = job;
        anyhow::bail!("provider does not support polling media jobs")
    }

    /// Warm up the HTTP connection pool (TLS handshake, DNS, HTTP/2 setup).
    /// Default implementation is a no-op; providers with HTTP clients should override.
    async fn warmup(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// One-shot helper: builds a ChatRequest from system + user message, calls chat(),
/// and returns just the text. Used by memory manager and tests.
pub async fn one_shot(
    provider: &dyn ModelProvider,
    system: Option<&str>,
    message: &str,
    model: &str,
    temperature: f64,
) -> anyhow::Result<String> {
    let mut messages = Vec::new();
    if let Some(sys) = system {
        if provider.supports_developer_role(model) {
            messages.push(ChatMessage::developer(sys));
        } else {
            messages.push(ChatMessage::system(sys));
        }
    }
    messages.push(ChatMessage::user(message));
    let request = ChatRequest {
        messages: &messages,
        tools: None,
        native_tools: None,
    };
    let response = provider.chat(request, model, temperature).await?;
    Ok(response.text.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_constructors() {
        let sys = ChatMessage::system("Be helpful");
        assert_eq!(sys.role, "system");
        assert_eq!(sys.content, "Be helpful");

        let user = ChatMessage::user("Hello");
        assert_eq!(user.role, "user");

        let asst = ChatMessage::assistant("Hi there");
        assert_eq!(asst.role, "assistant");

        let tool = ChatMessage::tool("{}");
        assert_eq!(tool.role, "tool");

        let dev = ChatMessage::developer("Follow these instructions");
        assert_eq!(dev.role, "developer");
        assert_eq!(dev.content, "Follow these instructions");
    }

    #[test]
    fn chat_response_helpers() {
        let empty = ChatResponse {
            text: None,
            tool_calls: vec![],
            provider_tool_calls: vec![],
            usage: TokenUsage::default(),
        };
        assert!(!empty.has_tool_calls());
        assert_eq!(empty.text_or_empty(), "");

        let with_tools = ChatResponse {
            text: Some("Let me check".into()),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
            }],
            provider_tool_calls: vec![],
            usage: TokenUsage::default(),
        };
        assert!(with_tools.has_tool_calls());
        assert_eq!(with_tools.text_or_empty(), "Let me check");
    }

    #[test]
    fn tool_call_serialization() {
        let tc = ToolCall {
            id: "call_123".into(),
            name: "read".into(),
            arguments: r#"{"path":"test.txt"}"#.into(),
        };
        let json = serde_json::to_string(&tc).unwrap();
        assert!(json.contains("call_123"));
        assert!(json.contains("read"));
    }

    #[test]
    fn conversation_message_variants() {
        let chat = ConversationMessage::Chat(ChatMessage::user("hi"));
        let json = serde_json::to_string(&chat).unwrap();
        assert!(json.contains("\"type\":\"Chat\""));

        let tool_result = ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "1".into(),
            content: "done".into(),
        }]);
        let json = serde_json::to_string(&tool_result).unwrap();
        assert!(json.contains("\"type\":\"ToolResults\""));
    }
}
