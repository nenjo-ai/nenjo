//! Local Ollama provider. No authentication required, configurable base URL
//! (defaults to `http://localhost:11434`).
//!
//! Supports native tool calling (Ollama ≥ 0.3.0).

use crate::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};
use async_trait::async_trait;
use nenjo_tools::ToolSpec;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OllamaProvider {
    base_url: String,
    client: Client,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    options: Options,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<NativeToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct Options {
    temperature: f64,
}

// ── Tool spec types (OpenAI-compatible format used by Ollama) ───

#[derive(Debug, Serialize)]
struct NativeToolSpec {
    #[serde(rename = "type")]
    kind: String,
    function: NativeToolFunctionSpec,
}

#[derive(Debug, Serialize)]
struct NativeToolFunctionSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolCall {
    function: NativeFunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeFunctionCall {
    name: String,
    arguments: serde_json::Value,
}

// ── Response types ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    message: ResponseMessage,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<NativeToolCall>>,
}

impl OllamaProvider {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url
                .unwrap_or("http://localhost:11434")
                .trim_end_matches('/')
                .to_string(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(300)) // Ollama runs locally, may be slow
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        tools.map(|items| {
            items
                .iter()
                .map(|tool| NativeToolSpec {
                    kind: "function".to_string(),
                    function: NativeToolFunctionSpec {
                        name: crate::sanitize_tool_name_lenient(&tool.name),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    },
                })
                .collect()
        })
    }

    fn convert_messages(messages: &[ChatMessage]) -> Vec<Message> {
        messages
            .iter()
            .map(|m| {
                // Reconstruct assistant tool-call messages for Ollama's format.
                if m.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ToolCall>>(tool_calls_value.clone())
                {
                    let tool_calls = parsed_calls
                        .into_iter()
                        .map(|tc| NativeToolCall {
                            function: NativeFunctionCall {
                                name: tc.name,
                                arguments: serde_json::from_str(&tc.arguments)
                                    .unwrap_or(serde_json::Value::Object(Default::default())),
                            },
                        })
                        .collect::<Vec<_>>();
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return Message {
                        role: "assistant".to_string(),
                        content,
                        tool_calls: Some(tool_calls),
                        tool_call_id: None,
                    };
                }

                // Reconstruct tool result messages.
                if m.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content)
                {
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return Message {
                        role: "tool".to_string(),
                        content,
                        tool_calls: None,
                        tool_call_id: None,
                    };
                }

                Message {
                    role: if m.role == "developer" {
                        "system".to_string()
                    } else {
                        m.role.clone()
                    },
                    content: Some(m.content.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                }
            })
            .collect()
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let ollama_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            stream: false,
            options: Options { temperature },
            tools: Self::convert_tools(request.tools),
        };

        let url = format!("{}/api/chat", self.base_url);

        let response = self
            .client
            .post(&url)
            .json(&ollama_request)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to connect to Ollama at {url}: {e}. \
                     Is Ollama running? (brew install ollama && ollama serve)"
                )
            })?;

        if !response.status().is_success() {
            let err = crate::api_error("Ollama", response).await;
            anyhow::bail!("{err}. Is Ollama running? (brew install ollama && ollama serve)");
        }

        let chat_response: ApiChatResponse = response.json().await?;

        let tool_calls = chat_response
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                ToolCall {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: tc.function.name,
                    // Ollama returns arguments as a JSON object; stringify for our ToolCall format.
                    arguments: tc.function.arguments.to_string(),
                }
            })
            .collect::<Vec<_>>();

        Ok(ChatResponse {
            text: chat_response.message.content,
            tool_calls,
            usage: TokenUsage {
                input_tokens: chat_response.prompt_eval_count.unwrap_or(0),
                output_tokens: chat_response.eval_count.unwrap_or(0),
            },
        })
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn context_window(&self, _model: &str) -> Option<usize> {
        // Ollama's effective context depends on VRAM and num_ctx setting.
        // Return None so the turn loop uses its conservative default.
        None
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        let url = format!("{}/api/tags", self.base_url);
        self.client.get(&url).send().await.map_err(|e| {
            anyhow::anyhow!(
                "Cannot reach Ollama at {}: {e}. \
                 Is Ollama running? (brew install ollama && ollama serve)",
                self.base_url
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url() {
        let p = OllamaProvider::new(None);
        assert_eq!(p.base_url, "http://localhost:11434");
    }

    #[test]
    fn custom_url_trailing_slash() {
        let p = OllamaProvider::new(Some("http://192.168.1.100:11434/"));
        assert_eq!(p.base_url, "http://192.168.1.100:11434");
    }

    #[test]
    fn custom_url_no_trailing_slash() {
        let p = OllamaProvider::new(Some("http://myserver:11434"));
        assert_eq!(p.base_url, "http://myserver:11434");
    }

    #[test]
    fn empty_url_uses_empty() {
        let p = OllamaProvider::new(Some(""));
        assert_eq!(p.base_url, "");
    }

    #[test]
    fn request_serializes_with_system() {
        let messages = vec![
            ChatMessage::system("You are Nenjo"),
            ChatMessage::user("hello"),
        ];
        let req = NativeChatRequest {
            model: "llama3".to_string(),
            messages: OllamaProvider::convert_messages(&messages),
            stream: false,
            options: Options { temperature: 0.7 },
            tools: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"stream\":false"));
        assert!(json.contains("llama3"));
        assert!(json.contains("system"));
        assert!(json.contains("\"temperature\":0.7"));
        assert!(!json.contains("\"tools\""));
    }

    #[test]
    fn request_serializes_without_system() {
        let messages = vec![ChatMessage::user("test")];
        let req = NativeChatRequest {
            model: "mistral".to_string(),
            messages: OllamaProvider::convert_messages(&messages),
            stream: false,
            options: Options { temperature: 0.0 },
            tools: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"role\":\"system\""));
        assert!(json.contains("mistral"));
    }

    #[test]
    fn developer_role_mapped_to_system() {
        let messages = vec![ChatMessage::developer("Be helpful")];
        let converted = OllamaProvider::convert_messages(&messages);
        assert_eq!(converted[0].role, "system");
        assert_eq!(converted[0].content.as_deref(), Some("Be helpful"));
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"message":{"role":"assistant","content":"Hello from Ollama!"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("Hello from Ollama!"));
    }

    #[test]
    fn response_with_empty_content() {
        let json = r#"{"message":{"role":"assistant","content":""}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some(""));
    }

    #[test]
    fn response_with_multiline() {
        let json = r#"{"message":{"role":"assistant","content":"line1\nline2\nline3"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.unwrap().contains("line1"));
    }

    #[test]
    fn response_with_tool_calls() {
        let json = r#"{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "function": {
                            "name": "get_weather",
                            "arguments": {"location": "Tokyo"}
                        }
                    }
                ]
            },
            "prompt_eval_count": 50,
            "eval_count": 20
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let tool_calls = resp.message.tool_calls.unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(
            tool_calls[0].function.arguments,
            serde_json::json!({"location": "Tokyo"})
        );
    }

    #[test]
    fn tool_spec_conversion() {
        let tools = vec![nenjo_tools::ToolSpec {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
            category: Default::default(),
        }];
        let converted = OllamaProvider::convert_tools(Some(&tools)).unwrap();
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].kind, "function");
        assert_eq!(converted[0].function.name, "read_file");
    }
}
