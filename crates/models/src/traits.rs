use async_trait::async_trait;
use serde::{Deserialize, Serialize};
pub use tool_api::{ToolCall, ToolCategory, ToolResultMessage, ToolSpec};

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

/// An LLM response that may contain text, tool calls, or both.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Text content of the response (may be empty if only tool calls).
    pub text: Option<String>,
    /// Tool calls requested by the LLM.
    pub tool_calls: Vec<ToolCall>,
    /// Token usage reported by the provider (zeros when not available).
    pub usage: TokenUsage,
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
    /// When true, the turn loop sends system and developer prompts as separate
    /// messages. When false, they are combined into a single system message.
    fn supports_developer_role(&self, _model: &str) -> bool {
        false
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
        messages.push(ChatMessage::system(sys));
    }
    messages.push(ChatMessage::user(message));
    let request = ChatRequest {
        messages: &messages,
        tools: None,
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
            usage: TokenUsage::default(),
        };
        assert!(with_tools.has_tool_calls());
        assert_eq!(with_tools.text_or_empty(), "Let me check");
    }

    #[test]
    fn tool_call_serialization() {
        let tc = ToolCall {
            id: "call_123".into(),
            name: "file_read".into(),
            arguments: r#"{"path":"test.txt"}"#.into(),
        };
        let json = serde_json::to_string(&tc).unwrap();
        assert!(json.contains("call_123"));
        assert!(json.contains("file_read"));
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
