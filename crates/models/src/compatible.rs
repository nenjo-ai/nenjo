//! Generic OpenAI-compatible provider.
//! Most LLM APIs follow the same `/v1/chat/completions` format.
//! This module provides a single implementation that works for all of them.

use crate::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};
use async_trait::async_trait;
use nenjo_tools::ToolSpec;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// A provider that speaks the OpenAI-compatible chat completions API.
/// Used by: Venice, Vercel AI Gateway, Cloudflare AI Gateway, Moonshot,
/// Synthetic, `OpenCode` Zen, `Z.AI`, `GLM`, `MiniMax`, Bedrock, Qianfan, Groq, Mistral, `xAI`, etc.
pub struct OpenAiCompatibleProvider {
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) auth_header: AuthStyle,
    /// When false, do not fall back to /v1/responses on chat completions 404.
    /// GLM/Zhipu does not support the responses API.
    supports_responses_fallback: bool,
    client: Client,
}

/// How the provider expects the API key to be sent.
#[derive(Debug, Clone)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>` (used by some Chinese providers)
    XApiKey,
    /// Custom header name
    Custom(String),
}

impl OpenAiCompatibleProvider {
    pub fn new(name: &str, base_url: &str, api_key: Option<&str>, auth_style: AuthStyle) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(ToString::to_string),
            auth_header: auth_style,
            supports_responses_fallback: true,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Same as `new` but skips the /v1/responses fallback on 404.
    /// Use for providers (e.g. GLM) that only support chat completions.
    pub fn new_no_responses_fallback(
        name: &str,
        base_url: &str,
        api_key: Option<&str>,
        auth_style: AuthStyle,
    ) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(ToString::to_string),
            auth_header: auth_style,
            supports_responses_fallback: false,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Build the full URL for chat completions, detecting if base_url already includes the path.
    /// This allows custom providers with non-standard endpoints (e.g., VolcEngine ARK uses
    /// `/api/coding/v3/chat/completions` instead of `/v1/chat/completions`).
    fn chat_completions_url(&self) -> String {
        let has_full_endpoint = reqwest::Url::parse(&self.base_url)
            .map(|url| {
                url.path()
                    .trim_end_matches('/')
                    .ends_with("/chat/completions")
            })
            .unwrap_or_else(|_| {
                self.base_url
                    .trim_end_matches('/')
                    .ends_with("/chat/completions")
            });

        if has_full_endpoint {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }

    fn path_ends_with(&self, suffix: &str) -> bool {
        if let Ok(url) = reqwest::Url::parse(&self.base_url) {
            return url.path().trim_end_matches('/').ends_with(suffix);
        }

        self.base_url.trim_end_matches('/').ends_with(suffix)
    }

    fn has_explicit_api_path(&self) -> bool {
        let Ok(url) = reqwest::Url::parse(&self.base_url) else {
            return false;
        };

        let path = url.path().trim_end_matches('/');
        !path.is_empty() && path != "/"
    }

    /// Build the full URL for responses API, detecting if base_url already includes the path.
    fn responses_url(&self) -> String {
        if self.path_ends_with("/responses") {
            return self.base_url.clone();
        }

        let normalized_base = self.base_url.trim_end_matches('/');

        // If chat endpoint is explicitly configured, derive sibling responses endpoint.
        if let Some(prefix) = normalized_base.strip_suffix("/chat/completions") {
            return format!("{prefix}/responses");
        }

        // If an explicit API path already exists (e.g. /v1, /openai, /api/coding/v3),
        // append responses directly to avoid duplicate /v1 segments.
        if self.has_explicit_api_path() {
            format!("{normalized_base}/responses")
        } else {
            format!("{normalized_base}/v1/responses")
        }
    }
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct Message {
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
struct ApiChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<NativeUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponseToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<ResponseFunction>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponseFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ResponsesInput {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<ResponsesOutput>,
    #[serde(default)]
    output_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutput {
    #[serde(default)]
    content: Vec<ResponsesContent>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContent {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

fn first_nonempty(text: Option<&str>) -> Option<String> {
    text.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn extract_responses_text(response: ResponsesResponse) -> Option<String> {
    if let Some(text) = first_nonempty(response.output_text.as_deref()) {
        return Some(text);
    }

    for item in &response.output {
        for content in &item.content {
            if content.kind.as_deref() == Some("output_text") {
                if let Some(text) = first_nonempty(content.text.as_deref()) {
                    return Some(text);
                }
            }
        }
    }

    for item in &response.output {
        for content in &item.content {
            if let Some(text) = first_nonempty(content.text.as_deref()) {
                return Some(text);
            }
        }
    }

    None
}

impl OpenAiCompatibleProvider {
    fn apply_auth_header(
        &self,
        req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        match &self.auth_header {
            AuthStyle::Bearer => req.header("Authorization", format!("Bearer {api_key}")),
            AuthStyle::XApiKey => req.header("x-api-key", api_key),
            AuthStyle::Custom(header) => req.header(header, api_key),
        }
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        tools.map(|items| {
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
                .collect()
        })
    }

    /// Reconstruct native OpenAI-compatible messages from the turn loop's
    /// JSON-encoded `ChatMessage` values.
    ///
    /// Assistant messages with `tool_calls` JSON are converted into native
    /// tool call messages. Tool result messages with `tool_call_id` JSON
    /// are converted into native tool result messages.
    fn convert_messages(messages: &[ChatMessage]) -> Vec<Message> {
        messages
            .iter()
            .map(|m| {
                // Assistant message with tool calls encoded as JSON
                if m.role == "assistant" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        if let Some(tool_calls_value) = value.get("tool_calls") {
                            if let Ok(parsed_calls) =
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
                                return Message {
                                    role: "assistant".to_string(),
                                    content,
                                    tool_call_id: None,
                                    tool_calls: Some(tool_calls),
                                };
                            }
                        }
                    }
                }

                // Tool result message with tool_call_id encoded as JSON
                if m.role == "tool" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        let tool_call_id = value
                            .get("tool_call_id")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string);
                        let content = value
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string);
                        return Message {
                            role: "tool".to_string(),
                            content,
                            tool_call_id,
                            tool_calls: None,
                        };
                    }
                }

                // Regular message (system, user, plain assistant)
                Message {
                    role: m.role.clone(),
                    content: Some(m.content.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect()
    }

    async fn chat_via_responses(
        &self,
        api_key: &str,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
    ) -> anyhow::Result<String> {
        let request = ResponsesRequest {
            model: model.to_string(),
            input: vec![ResponsesInput {
                role: "user".to_string(),
                content: message.to_string(),
            }],
            instructions: system_prompt.map(str::to_string),
            stream: Some(false),
        };

        let url = self.responses_url();

        let response = self
            .apply_auth_header(self.client.post(&url).json(&request), api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("{} Responses API error: {error}", self.name);
        }

        let responses: ResponsesResponse = response.json().await?;

        extract_responses_text(responses)
            .ok_or_else(|| anyhow::anyhow!("No response from {} Responses API", self.name))
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `nenjo onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        let tools = Self::convert_tools(request.tools);
        let chat_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            temperature,
            stream: Some(false),
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        let url = self.chat_completions_url();
        let response = self
            .apply_auth_header(self.client.post(&url).json(&chat_request), api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();

            // 404 may mean this provider uses the Responses API instead
            if status == reqwest::StatusCode::NOT_FOUND && self.supports_responses_fallback {
                warn!(
                    provider = %self.name,
                    url = %url,
                    "Chat completions returned 404 — falling back to Responses API (tool calls will be unavailable)"
                );
                let system = request.messages.iter().find(|m| m.role == "system");
                let last_user = request.messages.iter().rfind(|m| m.role == "user");
                if let Some(user_msg) = last_user {
                    let text = self
                        .chat_via_responses(
                            api_key,
                            system.map(|m| m.content.as_str()),
                            &user_msg.content,
                            model,
                        )
                        .await
                        .map_err(|responses_err| {
                            anyhow::anyhow!(
                                "{} API error (chat completions unavailable; responses fallback failed: {responses_err})",
                                self.name
                            )
                        })?;
                    return Ok(ChatResponse {
                        text: Some(text),
                        tool_calls: vec![],
                        usage: TokenUsage::default(),
                    });
                }
            }

            return Err(crate::api_error(&self.name, response).await);
        }

        let body_text = response.text().await?;

        // Some providers (e.g. OpenRouter routing to Clarifai) return HTTP 200
        // with an error payload instead of a valid chat completion.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(err) = value.get("error") {
                let msg = err
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown error");
                return Err(anyhow::anyhow!(
                    "{} returned an error in a 200 response: {msg}",
                    self.name
                ));
            }
        }

        let chat_response: ApiChatResponse = serde_json::from_str(&body_text).map_err(|e| {
            anyhow::anyhow!(
                "{} response decode error: {e}\nBody: {}",
                self.name,
                &body_text[..body_text.len().min(500)]
            )
        })?;

        let usage = chat_response
            .usage
            .map(|u| TokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            })
            .unwrap_or_default();

        let message = chat_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))?
            .message;

        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                let function = tc.function?;
                let name = function.name?;
                let arguments = function.arguments.unwrap_or_else(|| "{}".to_string());
                Some(ToolCall {
                    id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    name,
                    arguments,
                })
            })
            .collect::<Vec<_>>();

        Ok(ChatResponse {
            text: message.content,
            tool_calls,
            usage,
        })
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        let m = model.to_lowercase();
        // Match known models served through OpenAI-compatible endpoints.
        if m.contains("deepseek") {
            Some(128_000)
        } else if m.contains("mistral-large") || m.contains("mistral-medium") {
            Some(256_000)
        } else if m.contains("mistral") {
            Some(128_000)
        } else if m.contains("qwen") {
            Some(256_000)
        } else if m.contains("grok-4") && (m.contains("fast") || m.contains("4.1")) {
            Some(2_000_000)
        } else if m.contains("grok-4") {
            Some(256_000)
        } else if m.contains("grok-3") || m.contains("llama-4") || m.contains("llama4") {
            Some(1_000_000)
        } else if m.contains("llama-3") || m.contains("llama3") {
            Some(128_000)
        } else if m.contains("kimi") || m.contains("moonshot") {
            Some(256_000)
        } else if m.contains("minimax") {
            Some(200_000)
        } else {
            // Unknown model on a compatible endpoint — no opinion
            None
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        let m = model.to_lowercase();
        m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(name: &str, url: &str, key: Option<&str>) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(name, url, key, AuthStyle::Bearer)
    }

    #[test]
    fn creates_with_key() {
        let p = make_provider("venice", "https://api.venice.ai", Some("vn-key"));
        assert_eq!(p.name, "venice");
        assert_eq!(p.base_url, "https://api.venice.ai");
        assert_eq!(p.api_key.as_deref(), Some("vn-key"));
    }

    #[test]
    fn creates_without_key() {
        let p = make_provider("test", "https://example.com", None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn strips_trailing_slash() {
        let p = make_provider("test", "https://example.com/", None);
        assert_eq!(p.base_url, "https://example.com");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        use crate::traits::{ChatMessage, ChatRequest};
        let p = make_provider("Venice", "https://api.venice.ai", None);
        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = p.chat(request, "llama-3.3-70b", 0.7).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Venice API key not set")
        );
    }

    #[test]
    fn request_serializes_correctly() {
        let req = NativeChatRequest {
            model: "llama-3.3-70b".to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: Some("You are Nenjo".to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                },
                Message {
                    role: "user".to_string(),
                    content: Some("hello".to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                },
            ],
            temperature: 0.4,
            stream: Some(false),
            tools: None,
            tool_choice: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("llama-3.3-70b"));
        assert!(json.contains("system"));
        assert!(json.contains("user"));
        // Optional fields should be omitted
        assert!(!json.contains("tool_call_id"));
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("tool_choice"));
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"choices":[{"message":{"content":"Hello from Venice!"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.content,
            Some("Hello from Venice!".to_string())
        );
    }

    #[test]
    fn response_empty_choices() {
        let json = r#"{"choices":[]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn x_api_key_auth_style() {
        let p = OpenAiCompatibleProvider::new(
            "moonshot",
            "https://api.moonshot.cn",
            Some("ms-key"),
            AuthStyle::XApiKey,
        );
        assert!(matches!(p.auth_header, AuthStyle::XApiKey));
    }

    #[test]
    fn custom_auth_style() {
        let p = OpenAiCompatibleProvider::new(
            "custom",
            "https://api.example.com",
            Some("key"),
            AuthStyle::Custom("X-Custom-Key".into()),
        );
        assert!(matches!(p.auth_header, AuthStyle::Custom(_)));
    }

    #[tokio::test]
    async fn all_compatible_providers_fail_without_key() {
        use crate::traits::{ChatMessage, ChatRequest};
        let providers = vec![
            make_provider("Venice", "https://api.venice.ai", None),
            make_provider("Moonshot", "https://api.moonshot.cn", None),
            make_provider("GLM", "https://open.bigmodel.cn", None),
            make_provider("MiniMax", "https://api.minimaxi.com/v1", None),
            make_provider("Groq", "https://api.groq.com/openai", None),
            make_provider("Mistral", "https://api.mistral.ai", None),
            make_provider("xAI", "https://api.x.ai", None),
        ];

        for p in providers {
            let messages = vec![ChatMessage::user("test")];
            let request = ChatRequest {
                messages: &messages,
                tools: None,
            };
            let result = p.chat(request, "model", 0.7).await;
            assert!(result.is_err(), "{} should fail without key", p.name);
            assert!(
                result.unwrap_err().to_string().contains("API key not set"),
                "{} error should mention key",
                p.name
            );
        }
    }

    #[test]
    fn responses_extracts_top_level_output_text() {
        let json = r#"{"output_text":"Hello from top-level","output":[]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(response).as_deref(),
            Some("Hello from top-level")
        );
    }

    #[test]
    fn responses_extracts_nested_output_text() {
        let json =
            r#"{"output":[{"content":[{"type":"output_text","text":"Hello from nested"}]}]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(response).as_deref(),
            Some("Hello from nested")
        );
    }

    #[test]
    fn responses_extracts_any_text_as_fallback() {
        let json = r#"{"output":[{"content":[{"type":"message","text":"Fallback text"}]}]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(response).as_deref(),
            Some("Fallback text")
        );
    }

    // ══════════════════════════════════════════════════════════
    // Custom endpoint path tests (Issue #114)
    // ══════════════════════════════════════════════════════════

    #[test]
    fn chat_completions_url_standard_openai() {
        // Standard OpenAI-compatible providers get /chat/completions appended
        let p = make_provider("openai", "https://api.openai.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_trailing_slash() {
        // Trailing slash is stripped, then /chat/completions appended
        let p = make_provider("test", "https://api.example.com/v1/", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_volcengine_ark() {
        // VolcEngine ARK uses custom path - should use as-is
        let p = make_provider(
            "volcengine",
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_custom_full_endpoint() {
        // Custom provider with full endpoint path
        let p = make_provider(
            "custom",
            "https://my-api.example.com/v2/llm/chat/completions",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-api.example.com/v2/llm/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_requires_exact_suffix_match() {
        let p = make_provider(
            "custom",
            "https://my-api.example.com/v2/llm/chat/completions-proxy",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-api.example.com/v2/llm/chat/completions-proxy/chat/completions"
        );
    }

    #[test]
    fn responses_url_standard() {
        // Standard providers get /v1/responses appended
        let p = make_provider("test", "https://api.example.com", None);
        assert_eq!(p.responses_url(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn responses_url_custom_full_endpoint() {
        // Custom provider with full responses endpoint
        let p = make_provider(
            "custom",
            "https://my-api.example.com/api/v2/responses",
            None,
        );
        assert_eq!(
            p.responses_url(),
            "https://my-api.example.com/api/v2/responses"
        );
    }

    #[test]
    fn responses_url_requires_exact_suffix_match() {
        let p = make_provider(
            "custom",
            "https://my-api.example.com/api/v2/responses-proxy",
            None,
        );
        assert_eq!(
            p.responses_url(),
            "https://my-api.example.com/api/v2/responses-proxy/responses"
        );
    }

    #[test]
    fn responses_url_derives_from_chat_endpoint() {
        let p = make_provider(
            "custom",
            "https://my-api.example.com/api/v2/chat/completions",
            None,
        );
        assert_eq!(
            p.responses_url(),
            "https://my-api.example.com/api/v2/responses"
        );
    }

    #[test]
    fn responses_url_base_with_v1_no_duplicate() {
        let p = make_provider("test", "https://api.example.com/v1", None);
        assert_eq!(p.responses_url(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn responses_url_non_v1_api_path_uses_raw_suffix() {
        let p = make_provider("test", "https://api.example.com/api/coding/v3", None);
        assert_eq!(
            p.responses_url(),
            "https://api.example.com/api/coding/v3/responses"
        );
    }

    #[test]
    fn chat_completions_url_without_v1() {
        // Provider configured without /v1 in base URL
        let p = make_provider("test", "https://api.example.com", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_base_with_v1() {
        // Provider configured with /v1 in base URL
        let p = make_provider("test", "https://api.example.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    // ══════════════════════════════════════════════════════════
    // Provider-specific endpoint tests (Issue #167)
    // ══════════════════════════════════════════════════════════

    #[test]
    fn chat_completions_url_zai() {
        // Z.AI uses /api/paas/v4 base path
        let p = make_provider("zai", "https://api.z.ai/api/paas/v4", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_minimax() {
        // MiniMax OpenAI-compatible endpoint requires /v1 base path.
        let p = make_provider("minimax", "https://api.minimaxi.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.minimaxi.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_glm() {
        // GLM (BigModel) uses /api/paas/v4 base path
        let p = make_provider("glm", "https://open.bigmodel.cn/api/paas/v4", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_opencode() {
        // OpenCode Zen uses /zen/v1 base path
        let p = make_provider("opencode", "https://opencode.ai/zen/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://opencode.ai/zen/v1/chat/completions"
        );
    }
}
