//! OpenRouter aggregator provider. Authenticates via Bearer token, routes to
//! multiple upstream models with provider-order pinning.

use crate::ToolSpec;
use crate::native::{
    MediaInputAsset, NativeMediaRequest, NativeMediaResponse, TranscribeAudioRequest,
    TranscriptSegment,
};
use crate::openai_tools::{ProviderToolSpec, convert_tools};
use crate::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose};
use reqwest::Client;
use reqwest::header::ACCEPT_ENCODING;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

const OPENROUTER_MAX_TRANSPORT_ATTEMPTS: u32 = 3;
const OPENROUTER_CHAT_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_AUDIO_TRANSCRIPTIONS_URL: &str =
    "https://openrouter.ai/api/v1/audio/transcriptions";

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
    tools: Option<Vec<ProviderToolSpec>>,
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
struct OpenRouterTranscriptionRequest {
    model: String,
    input_audio: OpenRouterAudioInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<Value>,
}

#[derive(Debug, Serialize)]
struct OpenRouterAudioInput {
    data: String,
    format: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTranscriptionResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    segments: Vec<OpenRouterTranscriptionSegment>,
    #[serde(default)]
    usage: Option<OpenRouterTranscriptionUsage>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTranscriptionSegment {
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    #[serde(default)]
    text: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenRouterTranscriptionUsage {
    #[serde(default)]
    seconds: Option<f64>,
    #[serde(flatten)]
    extra: Map<String, Value>,
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
    /// Older OpenRouter responses exposed this at the top level; the current
    /// OpenAPI schema exposes it in `openrouter_metadata`.
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    openrouter_metadata: Option<NativeOpenRouterMetadata>,
    #[serde(default)]
    usage: Option<NativeUsage>,
}

#[derive(Debug, Deserialize)]
struct NativeOpenRouterMetadata {
    #[serde(default)]
    endpoints: Option<NativeEndpointsMetadata>,
}

#[derive(Debug, Deserialize)]
struct NativeEndpointsMetadata {
    #[serde(default)]
    available: Vec<NativeEndpointInfo>,
}

#[derive(Debug, Deserialize)]
struct NativeEndpointInfo {
    provider: String,
    #[serde(default)]
    selected: bool,
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

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<ProviderToolSpec>> {
        convert_tools(tools, crate::sanitize_tool_name).filter(|items| !items.is_empty())
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
            provider_tool_calls: vec![],
            usage: TokenUsage::default(),
        }
    }

    fn selected_provider_name(response: &NativeChatResponse) -> Option<String> {
        response.provider.clone().or_else(|| {
            response
                .openrouter_metadata
                .as_ref()
                .and_then(|metadata| metadata.endpoints.as_ref())
                .and_then(|endpoints| {
                    endpoints
                        .available
                        .iter()
                        .find(|endpoint| endpoint.selected)
                })
                .map(|endpoint| endpoint.provider.clone())
        })
    }

    fn transcription_request(
        request: &TranscribeAudioRequest,
    ) -> anyhow::Result<OpenRouterTranscriptionRequest> {
        let language = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|language| !language.is_empty())
            .map(str::to_string);
        let temperature = request
            .provider_options
            .get("temperature")
            .and_then(Value::as_f64);
        let provider = request
            .provider_options
            .get("provider")
            .filter(|provider| !provider.is_null())
            .cloned();

        Ok(OpenRouterTranscriptionRequest {
            model: request.model.clone(),
            input_audio: openrouter_audio_input(&request.audio)?,
            language,
            temperature,
            provider,
        })
    }

    async fn transcribe_audio(
        &self,
        request: TranscribeAudioRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenRouter API key not set. Set OPENROUTER_API_KEY env var.")
        })?;
        let native_request = Self::transcription_request(&request)?;

        let response = self
            .client
            .post(OPENROUTER_AUDIO_TRANSCRIPTIONS_URL)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("HTTP-Referer", "https://github.com/nenjo-ai/nenjo")
            .header("X-Title", "Nenjo")
            .header(ACCEPT_ENCODING, "identity")
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("OpenRouter", response).await);
        }

        let body_text = response.text().await?;
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_text)
            && let Some(error) = value.get("error")
        {
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("OpenRouter returned an error in a 200 response: {message}");
        }

        let transcription: OpenRouterTranscriptionResponse = serde_json::from_str(&body_text)
            .map_err(|error| {
                anyhow::anyhow!(
                    "OpenRouter transcription response decode error: {error}\\nBody: {}",
                    body_text.chars().take(500).collect::<String>()
                )
            })?;
        let text = (!transcription.text.trim().is_empty())
            .then(|| transcription.text.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("OpenRouter transcription returned no text"))?;
        let duration_seconds = transcription
            .duration
            .or_else(|| transcription.usage.as_ref().and_then(|usage| usage.seconds));
        let segments = transcription
            .segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                start_seconds: segment.start,
                end_seconds: segment.end,
                text: segment.text,
                metadata: (!segment.extra.is_empty()).then(|| Value::Object(segment.extra)),
            })
            .collect();
        let mut metadata = transcription.extra;
        metadata.insert("transport".to_string(), json!("audio_transcriptions"));
        if let Some(usage) = transcription.usage {
            metadata.insert("usage".to_string(), serde_json::to_value(usage)?);
        }

        Ok(NativeMediaResponse::Transcript {
            text,
            language: transcription.language.or(native_request.language),
            duration_seconds,
            segments,
            metadata: Some(Value::Object(metadata)),
        })
    }
}

fn openrouter_audio_input(asset: &MediaInputAsset) -> anyhow::Result<OpenRouterAudioInput> {
    let MediaInputAsset::DataUri { data_uri } = asset else {
        anyhow::bail!(
            "OpenRouter audio transcription requires a base64 data_uri input; URLs and provider file ids are not supported"
        );
    };
    let (metadata, encoded) = data_uri
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("audio data URI must contain metadata and base64 data"))?;
    let metadata = metadata
        .strip_prefix("data:")
        .ok_or_else(|| anyhow::anyhow!("audio input must be a data URI"))?;
    let mut metadata_parts = metadata.split(';');
    let mime_type = metadata_parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream")
        .to_ascii_lowercase();
    if !metadata_parts.any(|part| part.eq_ignore_ascii_case("base64")) {
        anyhow::bail!("audio data URI must be base64 encoded");
    }

    let format = match mime_type.as_str() {
        "audio/wav" | "audio/wave" | "audio/x-wav" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/webm" | "video/webm" => "webm",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "video/mp4" => "m4a",
        "audio/aac" => "aac",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        "audio/pcm" | "audio/l16" => "pcm16",
        other => anyhow::bail!("audio data URI MIME type '{other}' is not supported"),
    };
    let encoded = encoded.trim();
    let bytes = general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(encoded))
        .or_else(|_| general_purpose::URL_SAFE.decode(encoded))
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(encoded))
        .map_err(|error| anyhow::anyhow!("invalid base64 audio data URI: {error}"))?;
    if bytes.is_empty() {
        anyhow::bail!("audio data URI cannot be empty");
    }

    Ok(OpenRouterAudioInput {
        data: encoded.to_string(),
        format: format.to_string(),
    })
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

        let body_text = {
            let mut last_error = None;
            let mut body = None;

            for attempt in 1..=OPENROUTER_MAX_TRANSPORT_ATTEMPTS {
                let response = match self
                    .client
                    .post(OPENROUTER_CHAT_COMPLETIONS_URL)
                    .header("Authorization", format!("Bearer {api_key}"))
                    .header("HTTP-Referer", "https://github.com/nenjo-ai/nenjo")
                    .header("X-Title", "Nenjo")
                    .header(ACCEPT_ENCODING, "identity")
                    .json(&native_request)
                    .send()
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        last_error = Some(anyhow::anyhow!(
                            "OpenRouter: request failed (~{estimated_tokens} input tokens, \
                             {messages_count} messages, attempt {attempt}/{OPENROUTER_MAX_TRANSPORT_ATTEMPTS}): {error}",
                            messages_count = native_request.messages.len(),
                        ));
                        if attempt < OPENROUTER_MAX_TRANSPORT_ATTEMPTS {
                            tokio::time::sleep(std::time::Duration::from_millis(
                                250 * u64::from(attempt),
                            ))
                            .await;
                            continue;
                        }
                        break;
                    }
                };

                let status = response.status();
                if !status.is_success() {
                    return Err(crate::api_error("OpenRouter", response).await);
                }

                match response.text().await {
                    Ok(text) => {
                        body = Some(text);
                        break;
                    }
                    Err(error) => {
                        last_error = Some(anyhow::anyhow!(
                            "OpenRouter: failed to read response body (status {status}, \
                             ~{estimated_tokens} input tokens, {messages_count} messages, \
                             attempt {attempt}/{OPENROUTER_MAX_TRANSPORT_ATTEMPTS}): {error}",
                            messages_count = native_request.messages.len(),
                        ));
                        if attempt < OPENROUTER_MAX_TRANSPORT_ATTEMPTS {
                            tokio::time::sleep(std::time::Duration::from_millis(
                                250 * u64::from(attempt),
                            ))
                            .await;
                        }
                    }
                }
            }

            body.ok_or_else(|| {
                last_error.unwrap_or_else(|| anyhow::anyhow!("OpenRouter: empty response body"))
            })?
        };
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
        if let Some(provider_name) = Self::selected_provider_name(&native_response)
            && let Ok(mut guard) = self.last_good_provider.lock()
        {
            *guard = Some(provider_name);
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
        // Only OpenAI newer models support the developer role.
        // Other providers behind OpenRouter (Anthropic, Google, Meta, etc.) do not.
        (m.contains("openai/") || m.contains("azure/"))
            && (m.contains("/o1")
                || m.contains("/o3")
                || m.contains("/o4")
                || m.contains("/gpt-5")
                || m.contains("/gpt-4.5")
                || m.contains("/gpt-4.1"))
    }

    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        match request {
            NativeMediaRequest::TranscribeAudio(request) => self.transcribe_audio(request).await,
            request => anyhow::bail!(
                "OpenRouter does not implement media operation {}",
                request.operation()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{MediaInputAsset, TranscribeAudioRequest};
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

    #[test]
    fn transcription_request_uses_openrouter_stt_shape() {
        let request = OpenRouterProvider::transcription_request(&TranscribeAudioRequest {
            model: "nvidia/parakeet-tdt-0.6b-v3".to_string(),
            audio: MediaInputAsset::DataUri {
                data_uri: "data:audio/webm;base64,YXVkaW8=".to_string(),
            },
            language: Some("en".to_string()),
            prompt: Some("Transcribe the attached English audio faithfully.".to_string()),
            provider_options: serde_json::json!({
                "temperature": 0.2,
                "provider": {"order": ["NVIDIA"]}
            }),
        })
        .expect("valid OpenRouter transcription request");

        let value = serde_json::to_value(request).expect("request serializes");
        assert_eq!(value["model"], "nvidia/parakeet-tdt-0.6b-v3");
        assert_eq!(
            value["input_audio"],
            serde_json::json!({"data": "YXVkaW8=", "format": "webm"})
        );
        assert_eq!(value["language"], "en");
        assert_eq!(value["temperature"], 0.2);
        assert_eq!(value["provider"], serde_json::json!({"order": ["NVIDIA"]}));
        assert!(value.get("messages").is_none());
        assert!(value.get("prompt").is_none());
    }

    #[test]
    fn transcription_request_rejects_non_audio_data() {
        let error = OpenRouterProvider::transcription_request(&TranscribeAudioRequest {
            model: "google/gemini-3.5-flash".to_string(),
            audio: MediaInputAsset::DataUri {
                data_uri: "data:text/plain;base64,YXVkaW8=".to_string(),
            },
            language: None,
            prompt: Some("Transcribe the attached audio.".to_string()),
            provider_options: serde_json::Value::Null,
        })
        .expect_err("non-audio content must be rejected");

        assert!(error.to_string().contains("MIME type 'text/plain'"));
    }

    #[tokio::test]
    async fn transcribe_media_dispatches_to_openrouter_adapter() {
        let provider = OpenRouterProvider::new(None);
        let error = provider
            .submit_media(NativeMediaRequest::TranscribeAudio(
                TranscribeAudioRequest {
                    model: "google/gemini-3.5-flash".to_string(),
                    audio: MediaInputAsset::DataUri {
                        data_uri: "data:audio/webm;base64,YXVkaW8=".to_string(),
                    },
                    language: None,
                    prompt: Some("Transcribe the attached audio.".to_string()),
                    provider_options: serde_json::Value::Null,
                },
            ))
            .await
            .expect_err("the adapter must require an OpenRouter key");

        assert!(error.to_string().contains("OpenRouter API key not set"));
    }

    #[test]
    fn transcription_request_does_not_require_a_prompt() {
        let request = OpenRouterProvider::transcription_request(&TranscribeAudioRequest {
            model: "nvidia/parakeet-tdt-0.6b-v3".to_string(),
            audio: MediaInputAsset::DataUri {
                data_uri: "data:audio/webm;base64,YXVkaW8=".to_string(),
            },
            language: None,
            prompt: None,
            provider_options: serde_json::Value::Null,
        })
        .expect("the dedicated transcription endpoint does not require a prompt");

        let value = serde_json::to_value(request).expect("request serializes");
        assert!(value.get("prompt").is_none());
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let provider = OpenRouterProvider::new(None);
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    #[test]
    fn developer_role_only_for_openai_newer_models() {
        let provider = OpenRouterProvider::new(None);
        assert!(provider.supports_developer_role("openai/gpt-5.1"));
        assert!(provider.supports_developer_role("openai/gpt-4.1"));
        assert!(provider.supports_developer_role("openai/o3"));
        assert!(!provider.supports_developer_role("openai/gpt-4o"));
        assert!(!provider.supports_developer_role("anthropic/claude-sonnet-4"));
        assert!(!provider.supports_developer_role("minimax/minimax-m2.5"));
    }

    #[test]
    fn selected_provider_uses_openrouter_metadata() {
        let response: NativeChatResponse = serde_json::from_value(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "ok"
                }
            }],
            "openrouter_metadata": {
                "endpoints": {
                    "available": [
                        {
                            "model": "minimax/minimax-m2.5",
                            "provider": "Clarifai",
                            "selected": false
                        },
                        {
                            "model": "minimax/minimax-m2.5",
                            "provider": "Minimax",
                            "selected": true
                        }
                    ],
                    "total": 2
                }
            }
        }))
        .unwrap();

        assert_eq!(
            OpenRouterProvider::selected_provider_name(&response).as_deref(),
            Some("Minimax")
        );
    }

    #[test]
    fn selected_provider_preserves_legacy_top_level_provider() {
        let response: NativeChatResponse = serde_json::from_value(serde_json::json!({
            "provider": "SambaNova",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "ok"
                }
            }],
            "openrouter_metadata": {
                "endpoints": {
                    "available": [{
                        "model": "meta-llama/llama-3",
                        "provider": "Together",
                        "selected": true
                    }],
                    "total": 1
                }
            }
        }))
        .unwrap();

        assert_eq!(
            OpenRouterProvider::selected_provider_name(&response).as_deref(),
            Some("SambaNova")
        );
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let provider = OpenRouterProvider::new(None);
        let messages = vec![ChatMessage::system("system"), ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
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
            native_tools: None,
        };
        let result = provider
            .chat(request, "anthropic/claude-sonnet-4", 0.7)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }
}
