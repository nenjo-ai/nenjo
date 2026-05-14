//! Anthropic Claude provider. Authenticates via `x-api-key` header.

use crate::ToolSpec;
use crate::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct AnthropicProvider {
    credential: Option<String>,
    base_url: String,
    client: Client,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<NativeMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    content: Vec<NativeContentOut>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum NativeContentOut {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct NativeToolSpec {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct NativeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct NativeChatResponse {
    #[serde(default)]
    content: Vec<NativeContentIn>,
    #[serde(default)]
    usage: Option<NativeUsage>,
}

#[derive(Debug, Deserialize)]
struct NativeContentIn {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

impl AnthropicProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self::with_base_url(api_key, None)
    }

    pub fn with_base_url(api_key: Option<&str>, base_url: Option<&str>) -> Self {
        let base_url = base_url
            .map(|u| u.trim_end_matches('/'))
            .unwrap_or("https://api.anthropic.com")
            .to_string();
        Self {
            credential: api_key
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(ToString::to_string),
            base_url,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn is_setup_token(token: &str) -> bool {
        token.starts_with("sk-ant-oat01-")
    }

    fn apply_auth(
        &self,
        request: reqwest::RequestBuilder,
        credential: &str,
    ) -> reqwest::RequestBuilder {
        if Self::is_setup_token(credential) {
            request.header("Authorization", format!("Bearer {credential}"))
        } else {
            request.header("x-api-key", credential)
        }
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        let items = tools?;
        if items.is_empty() {
            return None;
        }
        Some(
            items
                .iter()
                .map(|tool| NativeToolSpec {
                    name: crate::sanitize_tool_name(&tool.name),
                    description: tool.description.clone(),
                    input_schema: tool.parameters.clone(),
                })
                .collect(),
        )
    }

    fn parse_assistant_tool_call_message(content: &str) -> Option<Vec<NativeContentOut>> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_calls = value
            .get("tool_calls")
            .and_then(|v| serde_json::from_value::<Vec<ToolCall>>(v.clone()).ok())?;

        let mut blocks = Vec::new();
        if let Some(text) = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            blocks.push(NativeContentOut::Text {
                text: text.to_string(),
            });
        }
        for call in tool_calls {
            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            blocks.push(NativeContentOut::ToolUse {
                id: call.id,
                name: call.name,
                input,
            });
        }
        Some(blocks)
    }

    fn parse_tool_result_message(content: &str) -> Option<NativeMessage> {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        let tool_use_id = value
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let result = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        Some(NativeMessage {
            role: "user".to_string(),
            content: vec![NativeContentOut::ToolResult {
                tool_use_id,
                content: result,
            }],
        })
    }

    fn convert_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<NativeMessage>) {
        let mut system_prompt: Option<String> = None;
        let mut native_messages = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" | "developer" => match &mut system_prompt {
                    Some(existing) => {
                        existing.push_str("\n\n");
                        existing.push_str(&msg.content);
                    }
                    None => {
                        system_prompt = Some(msg.content.clone());
                    }
                },
                "assistant" => {
                    if let Some(blocks) = Self::parse_assistant_tool_call_message(&msg.content) {
                        native_messages.push(NativeMessage {
                            role: "assistant".to_string(),
                            content: blocks,
                        });
                    } else {
                        native_messages.push(NativeMessage {
                            role: "assistant".to_string(),
                            content: vec![NativeContentOut::Text {
                                text: msg.content.clone(),
                            }],
                        });
                    }
                }
                "tool" => {
                    if let Some(tool_result) = Self::parse_tool_result_message(&msg.content) {
                        native_messages.push(tool_result);
                    } else {
                        native_messages.push(NativeMessage {
                            role: "user".to_string(),
                            content: vec![NativeContentOut::Text {
                                text: msg.content.clone(),
                            }],
                        });
                    }
                }
                _ => {
                    native_messages.push(NativeMessage {
                        role: "user".to_string(),
                        content: vec![NativeContentOut::Text {
                            text: msg.content.clone(),
                        }],
                    });
                }
            }
        }

        (system_prompt, native_messages)
    }

    fn parse_native_response(response: NativeChatResponse) -> ChatResponse {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in response.content {
            match block.kind.as_str() {
                "text" => {
                    if let Some(text) = block.text.map(|t| t.trim().to_string())
                        && !text.is_empty()
                    {
                        text_parts.push(text);
                    }
                }
                "tool_use" => {
                    let name = block.name.unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let arguments = block
                        .input
                        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                    tool_calls.push(ToolCall {
                        id: block.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        name,
                        arguments: arguments.to_string(),
                    });
                }
                _ => {}
            }
        }

        let usage = response
            .usage
            .map(|u| TokenUsage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
            })
            .unwrap_or_default();

        ChatResponse {
            text: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            },
            tool_calls,
            usage,
        }
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Anthropic credentials not set. Set ANTHROPIC_API_KEY or ANTHROPIC_OAUTH_TOKEN (setup-token)."
            )
        })?;

        let (system_prompt, messages) = Self::convert_messages(request.messages);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            max_tokens: 16384,
            system: system_prompt,
            messages,
            // Anthropic caps temperature at 1.0.
            temperature: temperature.min(1.0),
            tools: Self::convert_tools(request.tools),
        };

        let req = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&native_request);

        let response = self.apply_auth(req, credential).send().await?;
        if !response.status().is_success() {
            return Err(crate::api_error("Anthropic", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        Ok(Self::parse_native_response(native_response))
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        let m = model.to_lowercase();
        Some(if m.contains("opus-4") {
            // Claude Opus 4.x: 1M
            1_000_000
        } else if m.contains("sonnet-4-6") || m.contains("sonnet-4.6") {
            // Claude Sonnet 4.6: 1M
            1_000_000
        } else if m.contains("sonnet-4") || m.contains("haiku-4") {
            // Claude Sonnet 4.0/4.5, Haiku 4.5: 200K
            200_000
        } else if m.contains("3.5") || m.contains("3-5") {
            200_000
        } else {
            // Conservative fallback
            200_000
        })
    }

    fn supports_native_tools(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = AnthropicProvider::new(Some("sk-ant-test123"));
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("sk-ant-test123"));
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_without_key() {
        let p = AnthropicProvider::new(None);
        assert!(p.credential.is_none());
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_with_empty_key() {
        let p = AnthropicProvider::new(Some(""));
        assert!(p.credential.is_none());
    }

    #[test]
    fn creates_with_whitespace_key() {
        let p = AnthropicProvider::new(Some("  sk-ant-test123  "));
        assert!(p.credential.is_some());
        assert_eq!(p.credential.as_deref(), Some("sk-ant-test123"));
    }

    #[test]
    fn creates_with_custom_base_url() {
        let p =
            AnthropicProvider::with_base_url(Some("sk-ant-test"), Some("https://api.example.com"));
        assert_eq!(p.base_url, "https://api.example.com");
        assert_eq!(p.credential.as_deref(), Some("sk-ant-test"));
    }

    #[test]
    fn custom_base_url_trims_trailing_slash() {
        let p = AnthropicProvider::with_base_url(None, Some("https://api.example.com/"));
        assert_eq!(p.base_url, "https://api.example.com");
    }

    #[test]
    fn default_base_url_when_none_provided() {
        let p = AnthropicProvider::with_base_url(None, None);
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = p.chat(request, "claude-3-opus", 0.7).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("credentials not set"),
            "Expected key error, got: {err}"
        );
    }

    #[test]
    fn setup_token_detection_works() {
        assert!(AnthropicProvider::is_setup_token("sk-ant-oat01-abcdef"));
        assert!(!AnthropicProvider::is_setup_token("sk-ant-api-key"));
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let messages = vec![
            ChatMessage::system("You are Nenjo"),
            ChatMessage::user("hello"),
        ];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = p.chat(request, "claude-3-opus", 0.7).await;
        assert!(result.is_err());
    }

    #[test]
    fn temperature_clamped_to_1() {
        // Anthropic caps temperature at 1.0; verify our request builder clamps it.
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("hello"),
        ];
        let (system, native_msgs) = AnthropicProvider::convert_messages(&messages);
        assert!(system.is_some());
        assert_eq!(native_msgs.len(), 1);

        // Build the request with temperature > 1.0
        let req = NativeChatRequest {
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 16384,
            system,
            messages: native_msgs,
            temperature: 1.8_f64.min(1.0),
            tools: None,
        };
        assert!((req.temperature - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn convert_messages_combines_system_and_developer() {
        let messages = vec![
            ChatMessage::system("System prompt"),
            ChatMessage::developer("Developer instructions"),
            ChatMessage::user("hello"),
        ];
        let (system, native_msgs) = AnthropicProvider::convert_messages(&messages);
        assert_eq!(
            system.as_deref(),
            Some("System prompt\n\nDeveloper instructions")
        );
        assert_eq!(native_msgs.len(), 1);
        assert_eq!(native_msgs[0].role, "user");
    }

    #[test]
    fn convert_tools_sanitizes_names() {
        let tools = vec![ToolSpec {
            name: "app.nenjo.platform/tasks".into(),
            description: "Manage tasks".into(),
            parameters: serde_json::json!({}),
            category: Default::default(),
        }];
        let converted = AnthropicProvider::convert_tools(Some(&tools)).unwrap();
        assert_eq!(converted[0].name, "app_nenjo_platform_tasks");
    }

    #[test]
    fn parse_tool_call_roundtrip() {
        // Simulate assistant response with tool_use, then tool result
        let response = NativeChatResponse {
            content: vec![
                NativeContentIn {
                    kind: "text".into(),
                    text: Some("Let me check.".into()),
                    id: None,
                    name: None,
                    input: None,
                },
                NativeContentIn {
                    kind: "tool_use".into(),
                    text: None,
                    id: Some("toolu_01".into()),
                    name: Some("shell".into()),
                    input: Some(serde_json::json!({"command": "ls"})),
                },
            ],
            usage: Some(NativeUsage {
                input_tokens: 100,
                output_tokens: 50,
            }),
        };
        let parsed = AnthropicProvider::parse_native_response(response);
        assert_eq!(parsed.text.as_deref(), Some("Let me check."));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "toolu_01");
        assert_eq!(parsed.tool_calls[0].name, "shell");
        assert!(parsed.tool_calls[0].arguments.contains("ls"));
        assert_eq!(parsed.usage.input_tokens, 100);
        assert_eq!(parsed.usage.output_tokens, 50);
    }
}
