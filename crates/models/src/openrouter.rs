//! OpenRouter aggregator provider. Authenticates via Bearer token, routes to
//! multiple upstream models with provider-order pinning.

use crate::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};
use async_trait::async_trait;
use nenjo_tools::ToolSpec;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenRouterProvider {
    api_key: Option<String>,
    client: Client,
    /// Track the last upstream provider that served a successful response
    /// so we can pin future requests to it and avoid broken fallbacks.
    last_good_provider: std::sync::Mutex<Option<String>>,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<NativeMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<NativeProviderRouting>,
}

#[derive(Debug, Serialize)]
struct NativeProviderRouting {
    order: Vec<String>,
    allow_fallbacks: bool,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<NativeToolCall>>,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    function: NativeFunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct NativeUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct NativeChatResponse {
    choices: Vec<NativeChoice>,
    /// The upstream provider that served this response (e.g. "SambaNova").
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    usage: Option<NativeUsage>,
}

#[derive(Debug, Deserialize)]
struct NativeChoice {
    message: NativeResponseMessage,
}

#[derive(Debug, Deserialize)]
struct NativeResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<NativeToolCall>>,
}

impl OpenRouterProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self {
            api_key: api_key.map(ToString::to_string),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
            last_good_provider: std::sync::Mutex::new(None),
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
                    kind: "function".to_string(),
                    function: NativeToolFunctionSpec {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    },
                })
                .collect(),
        )
    }

    fn convert_messages(messages: &[ChatMessage]) -> Vec<NativeMessage> {
        messages
            .iter()
            .map(|m| {
                if m.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ToolCall>>(tool_calls_value.clone())
                {
                    let tool_calls = parsed_calls
                        .into_iter()
                        .map(|tc| NativeToolCall {
                            id: Some(tc.id),
                            kind: Some("function".to_string()),
                            function: NativeFunctionCall {
                                name: tc.name,
                                arguments: tc.arguments,
                            },
                        })
                        .collect::<Vec<_>>();
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return NativeMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_call_id: None,
                        tool_calls: Some(tool_calls),
                    };
                }

                if m.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content)
                {
                    let tool_call_id = value
                        .get("tool_call_id")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return NativeMessage {
                        role: "tool".to_string(),
                        content,
                        tool_call_id,
                        tool_calls: None,
                    };
                }

                NativeMessage {
                    role: m.role.clone(),
                    content: Some(m.content.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect()
    }

    fn parse_native_response(message: NativeResponseMessage) -> ChatResponse {
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect::<Vec<_>>();

        ChatResponse {
            text: message.content,
            tool_calls,
            usage: TokenUsage::default(),
        }
    }
}

#[async_trait]
impl ModelProvider for OpenRouterProvider {
    async fn warmup(&self) -> anyhow::Result<()> {
        // Hit a lightweight endpoint to establish TLS + HTTP/2 connection pool.
        // This prevents the first real chat request from timing out on cold start.
        if let Some(api_key) = self.api_key.as_ref() {
            self.client
                .get("https://openrouter.ai/api/v1/auth/key")
                .header("Authorization", format!("Bearer {api_key}"))
                .send()
                .await?
                .error_for_status()?;
        }
        Ok(())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenRouter API key not set. Set OPENROUTER_API_KEY env var.")
        })?;

        let tools = Self::convert_tools(request.tools);

        // Pin to the last successful upstream provider to avoid broken
        // fallbacks (e.g. Clarifai failing for minimax models).
        let provider_routing = self
            .last_good_provider
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .map(|p| NativeProviderRouting {
                order: vec![p],
                allow_fallbacks: true,
            });

        let messages = Self::convert_messages(request.messages);

        // Log estimated request size so context-too-large issues are visible
        let estimated_chars: usize = messages
            .iter()
            .map(|m| m.content.as_deref().unwrap_or("").len())
            .sum();
        let estimated_tokens = estimated_chars / 4;
        tracing::info!(
            model = model,
            messages = messages.len(),
            estimated_tokens = estimated_tokens,
            "OpenRouter request"
        );

        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
            provider: provider_routing,
        };

        let response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("HTTP-Referer", "https://github.com/nenjo-ai/nenjo")
            .header("X-Title", "Nenjo")
            .json(&native_request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(crate::api_error("OpenRouter", response).await);
        }

        let body_text = response.text().await.map_err(|e| {
            anyhow::anyhow!(
                "OpenRouter: failed to read response body (status {status}, \
                 ~{estimated_tokens} input tokens, {messages_count} messages): {e}",
                messages_count = native_request.messages.len(),
            )
        })?;
        // OpenRouter can return HTTP 200 with an error payload when a
        // downstream provider (e.g. Clarifai) fails.  Detect this before
        // trying to parse as a normal chat completion.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_text)
            && let Some(err) = value.get("error")
        {
            let msg = err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            return Err(anyhow::anyhow!(
                "OpenRouter returned an error in a 200 response: {msg}"
            ));
        }

        let native_response: NativeChatResponse =
            serde_json::from_str(&body_text).map_err(|e| {
                anyhow::anyhow!(
                    "OpenRouter response decode error: {e}\nBody: {}",
                    &body_text[..body_text.len().min(500)]
                )
            })?;

        // Track the upstream provider that served this response so we
        // can pin future requests to it.
        if let Some(ref provider_name) = native_response.provider
            && let Ok(mut guard) = self.last_good_provider.lock()
        {
            *guard = Some(provider_name.clone());
        }

        let usage = native_response
            .usage
            .map(|u| TokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            })
            .unwrap_or_default();

        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenRouter"))?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        // OpenRouter routes to many models. Match on the model slug.
        let m = model.to_lowercase();
        if m.contains("claude-opus-4")
            || m.contains("claude-sonnet-4.6")
            || m.contains("claude-sonnet-4-6")
        {
            Some(1_000_000)
        } else if m.contains("claude-sonnet-4")
            || m.contains("claude-haiku-4")
            || m.contains("claude-3.5")
            || m.contains("claude-3-")
            || m.contains("claude-3.7")
        {
            Some(200_000)
        } else if m.contains("gpt-5") {
            Some(1_000_000)
        } else if m.contains("gpt-4o") {
            Some(128_000)
        } else if m.contains("o1") || m.contains("o3") || m.contains("o4") {
            Some(200_000)
        } else if m.contains("gemini") {
            Some(1_000_000)
        } else if m.contains("deepseek") {
            Some(128_000)
        } else if m.contains("llama-4") || m.contains("llama4") {
            Some(1_000_000)
        } else if m.contains("llama-3") || m.contains("llama3") {
            Some(128_000)
        } else if m.contains("mistral-large") || m.contains("qwen") {
            Some(256_000)
        } else if m.contains("grok-4") && m.contains("fast") {
            Some(2_000_000)
        } else if m.contains("grok-4") {
            Some(256_000)
        } else if m.contains("grok-3") {
            Some(1_000_000)
        } else if m.contains("kimi") {
            Some(256_000)
        } else if m.contains("minimax") {
            Some(200_000)
        } else {
            None
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        let m = model.to_lowercase();
        // Only OpenAI o-series models support the developer role.
        // Other providers behind OpenRouter (Anthropic, Google, Meta, etc.) do not.
        m.contains("/o1") || m.contains("/o3") || m.contains("/o4")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{ChatMessage, ChatRequest, ModelProvider};

    #[test]
    fn creates_with_key() {
        let provider = OpenRouterProvider::new(Some("sk-or-123"));
        assert_eq!(provider.api_key.as_deref(), Some("sk-or-123"));
    }

    #[test]
    fn creates_without_key() {
        let provider = OpenRouterProvider::new(None);
        assert!(provider.api_key.is_none());
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let provider = OpenRouterProvider::new(None);
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let provider = OpenRouterProvider::new(None);
        let messages = vec![ChatMessage::system("system"), ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = provider.chat(request, "openai/gpt-4o", 0.2).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_history_fails_without_key() {
        let provider = OpenRouterProvider::new(None);
        let messages = vec![
            ChatMessage::system("be concise"),
            ChatMessage::user("hello"),
        ];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = provider
            .chat(request, "anthropic/claude-sonnet-4", 0.7)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }
}
