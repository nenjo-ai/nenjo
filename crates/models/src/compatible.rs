//! Generic OpenAI-compatible provider.
//! Most LLM APIs follow the same `/v1/chat/completions` format.
//! This module provides a single implementation that works for all of them.

use crate::ToolSpec;
use crate::audio_data_uri::decode_base64_data_uri;
use crate::native::{
    MediaCapabilitiesProvider, MediaExecutionMode, MediaInputAsset, MediaOperation,
    MediaToolSpec as NativeMediaToolSpec, ModelMediaCapabilities, NativeMediaRequest,
    NativeMediaResponse, ProviderMediaCapabilities, TranscribeAudioRequest, TranscriptSegment,
};
use crate::openai_multimodal::{
    ChatArtifactDialect, ChatCompletionsContent, artifact_content, chat_artifact_transport,
};
use crate::openai_tools::{ProviderToolSpec, convert_tools};
use crate::traits::{
    ChatRequest, ChatResponse, ChatRole, ConversationMessage, ModelProvider, ProviderStreamEvent,
    TokenUsage, ToolCall,
};
use crate::{ArtifactInputTransport, MediaType, ModelCapabilityId};
use anyhow::Context;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
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
    artifact_dialect: ChatArtifactDialect,
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
        Self::new_with_dialect(
            name,
            base_url,
            api_key,
            auth_style,
            ChatArtifactDialect::OpenAi,
        )
    }

    pub(crate) fn new_with_dialect(
        name: &str,
        base_url: &str,
        api_key: Option<&str>,
        auth_style: AuthStyle,
        artifact_dialect: ChatArtifactDialect,
    ) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(ToString::to_string),
            auth_header: auth_style,
            supports_responses_fallback: true,
            artifact_dialect,
            client: Client::builder()
                .read_timeout(std::time::Duration::from_secs(120))
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
            artifact_dialect: ChatArtifactDialect::OpenAi,
            client: Client::builder()
                .read_timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Build the full URL for chat completions, detecting if base_url already includes the path.
    /// This allows custom providers with non-standard endpoints (e.g., VolcEngine ARK uses
    /// `/api/coding/v3/chat/completions` instead of `/v1/chat/completions`).
    fn chat_completions_url(&self) -> String {
        let path = reqwest::Url::parse(&self.base_url)
            .map(|url| url.path().trim_end_matches('/').to_string())
            .unwrap_or_else(|_| self.base_url.trim_end_matches('/').to_string());

        // If the base URL already contains a full chat endpoint path, use as-is.
        // Covers standard `/chat/completions` and provider-specific paths like
        // MiniMax's `/text/chatcompletion_v2`.
        let has_full_endpoint =
            path.ends_with("/chat/completions") || path.contains("/chatcompletion");

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

    fn audio_transcriptions_url(&self) -> String {
        if self.path_ends_with("/audio/transcriptions") {
            return self.base_url.clone();
        }

        let normalized_base = self.base_url.trim_end_matches('/');
        if self.has_explicit_api_path() {
            format!("{normalized_base}/audio/transcriptions")
        } else {
            format!("{normalized_base}/v1/audio/transcriptions")
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
    stream_options: Option<NativeStreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ProviderToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeStreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ChatCompletionsContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<NativeToolCall>>,
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

#[derive(Debug, Deserialize)]
struct ApiChatStreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<NativeUsage>,
    #[serde(default)]
    error: Option<StreamError>,
}

#[derive(Debug, Deserialize)]
struct StreamError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct ChatStreamState {
    text: String,
    tool_calls: Vec<StreamToolCallState>,
    usage: TokenUsage,
}

#[derive(Debug, Default)]
struct StreamToolCallState {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl ChatStreamState {
    fn absorb(
        &mut self,
        chunk: ApiChatStreamChunk,
        events: &tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
    ) -> anyhow::Result<()> {
        if let Some(error) = chunk.error {
            anyhow::bail!("OpenAI-compatible streaming error: {}", error.message);
        }
        if let Some(usage) = chunk.usage {
            self.usage = TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            };
        }
        for choice in chunk.choices {
            if let Some(content) = choice.delta.content
                && !content.is_empty()
            {
                self.text.push_str(&content);
                let _ = events.send(ProviderStreamEvent::TextDelta(content));
            }
            for tool_call in choice.delta.tool_calls {
                if self.tool_calls.len() <= tool_call.index {
                    self.tool_calls
                        .resize_with(tool_call.index.saturating_add(1), Default::default);
                }
                let state = &mut self.tool_calls[tool_call.index];
                if let Some(id) = tool_call.id {
                    state.id = Some(id);
                }
                if let Some(function) = tool_call.function {
                    if let Some(name) = function.name {
                        state.name.push_str(&name);
                    }
                    if let Some(arguments) = function.arguments {
                        state.arguments.push_str(&arguments);
                    }
                }
            }
        }
        Ok(())
    }

    fn into_response(self) -> ChatResponse {
        let tool_calls = self
            .tool_calls
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .map(|call| ToolCall {
                id: call.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: call.name,
                arguments: if call.arguments.is_empty() {
                    "{}".to_string()
                } else {
                    call.arguments
                },
            })
            .collect();
        ChatResponse {
            text: (!self.text.is_empty()).then_some(self.text),
            tool_calls,
            provider_tool_calls: Vec::new(),
            usage: self.usage,
        }
    }
}

fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    let (index, delimiter_len) = match (lf, crlf) {
        (Some(left), Some(right)) => {
            if left.0 <= right.0 {
                left
            } else {
                right
            }
        }
        (Some(found), None) | (None, Some(found)) => found,
        (None, None) => return None,
    };
    let remaining = buffer.split_off(index.saturating_add(delimiter_len));
    let mut frame = std::mem::replace(buffer, remaining);
    frame.truncate(index);
    Some(frame)
}

fn absorb_sse_frame(
    frame: &[u8],
    state: &mut ChatStreamState,
    events: &tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
) -> anyhow::Result<()> {
    let frame = std::str::from_utf8(frame).context("streaming response contained invalid UTF-8")?;
    let mut data = String::new();
    for line in frame.lines() {
        let Some(fragment) = line.strip_prefix("data:") else {
            continue;
        };
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(fragment.trim_start());
    }
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let chunk: ApiChatStreamChunk = serde_json::from_str(data).with_context(|| {
        format!(
            "OpenAI-compatible streaming response decode error; data: {}",
            &data[..data.floor_char_boundary(500)]
        )
    })?;
    state.absorb(chunk, events)
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

#[derive(Debug)]
struct DecodedAudioDataUri {
    mime_type: String,
    bytes: Vec<u8>,
    filename: String,
}

#[derive(Debug, Deserialize)]
struct AudioTranscriptionResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    segments: Vec<AudioTranscriptionSegment>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct AudioTranscriptionSegment {
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    text: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
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
            if content.kind.as_deref() == Some("output_text")
                && let Some(text) = first_nonempty(content.text.as_deref())
            {
                return Some(text);
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

fn compatible_transcribe_audio_tool_spec() -> NativeMediaToolSpec {
    let capability = MediaOperation::TranscribeAudio;
    NativeMediaToolSpec {
        capability,
        tool_name: capability.tool_name().unwrap().to_string(),
        description:
            "Transcribe an audio data URI with the configured OpenAI-compatible transcription model."
                .to_string(),
        execution: MediaExecutionMode::Immediate,
        parameters_schema: json!({
            "type": "object",
            "properties": {
                "audio": {
                    "type": "object",
                    "properties": {
                        "type": {"const": "data_uri"},
                        "data_uri": {"type": "string"}
                    },
                    "required": ["type", "data_uri"]
                },
                "language": {"type": "string"},
                "prompt": {
                    "type": "string",
                    "description": "Optional transcription guidance or vocabulary context."
                },
                "provider_options": {
                    "type": "object",
                    "properties": {
                        "response_format": {
                            "type": "string",
                            "enum": ["json", "verbose_json"]
                        },
                        "temperature": {"type": "number", "minimum": 0, "maximum": 1}
                    },
                    "additionalProperties": false
                }
            },
            "required": ["audio"]
        }),
    }
}

fn provider_option_str<'a>(options: &'a Value, key: &str) -> Option<&'a str> {
    options
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn provider_option_f64(options: &Value, key: &str) -> Option<f64> {
    options
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_f64)
}

fn audio_file_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "audio/webm" | "video/webm" => "webm",
        "audio/mp4" | "audio/m4a" => "m4a",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        _ => "audio",
    }
}

fn prepare_compatible_audio_data_uri(data_uri: &str) -> anyhow::Result<DecodedAudioDataUri> {
    let decoded = decode_base64_data_uri(data_uri)?;
    let mime_type = decoded.mime_type;
    let valid_audio_mime = mime_type.starts_with("audio/")
        || matches!(
            mime_type.as_str(),
            "video/webm" | "video/mp4" | "application/octet-stream"
        );
    if !valid_audio_mime {
        anyhow::bail!("audio data URI MIME type '{mime_type}' is not supported");
    }

    let filename = format!("audio.{}", audio_file_extension(&mime_type));
    Ok(DecodedAudioDataUri {
        mime_type,
        bytes: decoded.bytes,
        filename,
    })
}

fn supports_compatible_transcription_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "audio/mpeg"
            | "audio/mp3"
            | "audio/wav"
            | "audio/wave"
            | "audio/x-wav"
            | "audio/webm"
            | "audio/mp4"
            | "audio/m4a"
            | "audio/x-m4a"
            | "audio/ogg"
            | "audio/flac"
    )
}

fn compatible_audio_part(asset: &MediaInputAsset) -> anyhow::Result<Part> {
    match asset {
        MediaInputAsset::DataUri { data_uri } => {
            let decoded = prepare_compatible_audio_data_uri(data_uri)?;
            Part::bytes(decoded.bytes)
                .file_name(decoded.filename)
                .mime_str(&decoded.mime_type)
                .context("failed to build OpenAI-compatible audio upload part")
        }
        MediaInputAsset::Url { .. } => {
            anyhow::bail!(
                "OpenAI-compatible audio transcription requires a data_uri input; worker-side URL fetching is not supported"
            )
        }
        MediaInputAsset::ProviderFileId { .. } => {
            anyhow::bail!(
                "OpenAI-compatible audio transcription requires a data_uri input; provider file ids are not supported"
            )
        }
    }
}

fn json_object_or_none(object: Map<String, Value>) -> Option<Value> {
    if object.is_empty() {
        None
    } else {
        Some(Value::Object(object))
    }
}

impl OpenAiCompatibleProvider {
    fn apply_auth_header(
        &self,
        req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        if api_key.trim().is_empty() {
            return req;
        }

        match &self.auth_header {
            AuthStyle::Bearer => req.header("Authorization", format!("Bearer {api_key}")),
            AuthStyle::XApiKey => req.header("x-api-key", api_key),
            AuthStyle::Custom(header) => req.header(header, api_key),
        }
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<ProviderToolSpec>> {
        convert_tools(tools, crate::sanitize_tool_name)
    }

    #[cfg(test)]
    fn convert_messages(
        request: &ChatRequest<'_>,
        supports_developer_role: bool,
    ) -> anyhow::Result<Vec<Message>> {
        Self::convert_messages_for_dialect(
            request,
            supports_developer_role,
            ChatArtifactDialect::OpenAi,
        )
    }

    fn convert_messages_for_dialect(
        request: &ChatRequest<'_>,
        supports_developer_role: bool,
        artifact_dialect: ChatArtifactDialect,
    ) -> anyhow::Result<Vec<Message>> {
        let mut native = Vec::new();
        for message in request.messages {
            match message {
                ConversationMessage::AssistantToolCalls { text, tool_calls } => {
                    native.push(Message {
                        role: "assistant".to_string(),
                        content: text.clone().map(ChatCompletionsContent::text),
                        tool_call_id: None,
                        tool_calls: Some(
                            tool_calls
                                .iter()
                                .map(|tc| NativeToolCall {
                                    id: Some(tc.id.clone()),
                                    kind: Some("function".to_string()),
                                    function: NativeFunctionCall {
                                        name: tc.name.clone(),
                                        arguments: tc.arguments.clone(),
                                    },
                                })
                                .collect(),
                        ),
                    });
                }
                ConversationMessage::ToolResults(results) => {
                    for result in results {
                        native.push(Message {
                            role: "tool".to_string(),
                            content: Some(ChatCompletionsContent::text(
                                result.output.text_content(),
                            )),
                            tool_call_id: Some(result.tool_call_id.clone()),
                            tool_calls: None,
                        });
                    }
                    let references = results.iter().flat_map(|result| {
                        result.output.parts().iter().filter_map(|part| match part {
                            crate::ToolOutputPart::Artifact(reference) => Some((reference, None)),
                            crate::ToolOutputPart::Text(_) => None,
                        })
                    });
                    let content = artifact_content(
                        "Inspect the attached artifact.",
                        references,
                        request.prepared_artifacts,
                        artifact_dialect,
                    )?;
                    if content.has_media() {
                        native.push(Message {
                            role: "user".to_string(),
                            content: Some(content),
                            tool_call_id: None,
                            tool_calls: None,
                        });
                    }
                }
                ConversationMessage::Chat(message) => native.push(Message {
                    role: if message.role == ChatRole::Developer && !supports_developer_role {
                        "user".to_string()
                    } else {
                        message.role.to_string()
                    },
                    content: Some(artifact_content(
                        &message.content,
                        message.artifacts.iter().map(|input| {
                            (
                                input.artifact(),
                                input.instruction().map(|value| value.as_str()),
                            )
                        }),
                        request.prepared_artifacts,
                        artifact_dialect,
                    )?),
                    tool_call_id: None,
                    tool_calls: None,
                }),
                ConversationMessage::ArtifactAnalysis(analysis) => native.push(Message {
                    role: "user".to_string(),
                    content: Some(ChatCompletionsContent::text(analysis.model_context())),
                    tool_call_id: None,
                    tool_calls: None,
                }),
            }
        }
        Ok(native)
    }

    fn responses_fallback_allowed(&self, request: &ChatRequest<'_>) -> bool {
        self.supports_responses_fallback
            && !request
                .messages
                .iter()
                .any(ConversationMessage::has_artifact_references)
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

    async fn transcribe_audio(
        &self,
        request: TranscribeAudioRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key.as_deref().unwrap_or("");
        let response_format =
            provider_option_str(&request.provider_options, "response_format").unwrap_or("json");
        if !matches!(response_format, "json" | "verbose_json") {
            anyhow::bail!(
                "{} transcription response_format must be 'json' or 'verbose_json'",
                self.name
            );
        }

        let requested_language = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let mut form = Form::new()
            .text("model", request.model.clone())
            .text("response_format", response_format.to_string())
            .part("file", compatible_audio_part(&request.audio)?);

        if let Some(language) = requested_language.as_ref() {
            form = form.text("language", language.clone());
        }
        if let Some(prompt) = request
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        {
            form = form.text("prompt", prompt.to_string());
        }
        if let Some(temperature) = provider_option_f64(&request.provider_options, "temperature") {
            form = form.text("temperature", temperature.to_string());
        }

        let url = self.audio_transcriptions_url();
        let response = self
            .apply_auth_header(self.client.post(&url), api_key)
            .multipart(form)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error(&self.name, response).await);
        }

        let transcription: AudioTranscriptionResponse = response.json().await?;
        let segments = transcription
            .segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                start_seconds: segment.start,
                end_seconds: segment.end,
                text: segment.text,
                metadata: json_object_or_none(segment.extra),
            })
            .collect();

        Ok(NativeMediaResponse::Transcript {
            text: transcription.text,
            language: transcription.language.or(requested_language),
            duration_seconds: transcription.duration,
            segments,
            metadata: json_object_or_none(transcription.extra),
        })
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
        request.ensure_artifacts_prepared()?;
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `nenjo onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        let tools = Self::convert_tools(request.tools);
        let chat_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages_for_dialect(
                &request,
                self.supports_developer_role(model),
                self.artifact_dialect,
            )?,
            temperature,
            stream: Some(false),
            stream_options: None,
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
            if status == reqwest::StatusCode::NOT_FOUND && self.responses_fallback_allowed(&request)
            {
                warn!(
                    provider = %self.name,
                    "Chat completions returned 404 — falling back to Responses API (tool calls will be unavailable)"
                );
                let system = request.messages.iter().find_map(|message| {
                    message
                        .as_chat()
                        .filter(|chat| chat.role == ChatRole::System)
                });
                let last_user = request.messages.iter().rev().find_map(|message| {
                    message.as_chat().filter(|chat| chat.role == ChatRole::User)
                });
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
                        provider_tool_calls: vec![],
                        usage: TokenUsage::default(),
                    });
                }
            }

            return Err(crate::api_error(&self.name, response).await);
        }

        let body_text = response.text().await?;

        // Some providers (e.g. OpenRouter routing to Clarifai) return HTTP 200
        // with an error payload instead of a valid chat completion.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_text)
            && let Some(err) = value.get("error")
        {
            let msg = err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            return Err(anyhow::anyhow!(
                "{} returned an error in a 200 response: {msg}",
                self.name
            ));
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
            provider_tool_calls: vec![],
            usage,
        })
    }

    async fn chat_stream(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        events: tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
    ) -> anyhow::Result<ChatResponse> {
        if self.artifact_dialect != ChatArtifactDialect::Vllm {
            return self.chat(request, model, temperature).await;
        }
        request.ensure_artifacts_prepared()?;
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `nenjo onboard` or set the appropriate env var.",
                self.name
            )
        })?;
        let tools = Self::convert_tools(request.tools);
        let chat_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages_for_dialect(
                &request,
                self.supports_developer_role(model),
                self.artifact_dialect,
            )?,
            temperature,
            stream: Some(true),
            stream_options: Some(NativeStreamOptions {
                include_usage: true,
            }),
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
            if status == reqwest::StatusCode::NOT_FOUND && self.responses_fallback_allowed(&request)
            {
                warn!(
                    provider = %self.name,
                    "Streaming chat completions returned 404 — falling back to the buffered provider path"
                );
                return self.chat(request, model, temperature).await;
            }
            return Err(crate::api_error(&self.name, response).await);
        }

        let mut state = ChatStreamState::default();
        let mut buffer = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk?);
            while let Some(frame) = take_sse_frame(&mut buffer) {
                absorb_sse_frame(&frame, &mut state, &events)?;
            }
        }
        if !buffer.iter().all(u8::is_ascii_whitespace) {
            absorb_sse_frame(&buffer, &mut state, &events)?;
        }
        Ok(state.into_response())
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

    fn media_capabilities(&self) -> Option<ProviderMediaCapabilities> {
        Some(MediaCapabilitiesProvider::media_capabilities(self))
    }

    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        MediaCapabilitiesProvider::submit_media(self, request).await
    }

    fn supports_developer_role(&self, _model: &str) -> bool {
        // A generic compatible endpoint does not identify the server-side chat
        // template. Use `system`, which is the portable role across these APIs.
        false
    }

    fn artifact_input_transport(
        &self,
        _model: &str,
        capability: ModelCapabilityId,
        media_type: &MediaType,
    ) -> ArtifactInputTransport {
        const MAX_INLINE_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
        match capability {
            ModelCapabilityId::Chat
            | ModelCapabilityId::AnalyzeImage
            | ModelCapabilityId::AnalyzeDocument => {
                chat_artifact_transport(self.artifact_dialect, media_type.essence_str())
            }
            ModelCapabilityId::AnalyzeVideo => ArtifactInputTransport::Unsupported,
            ModelCapabilityId::TranscribeAudio => {
                if supports_compatible_transcription_media_type(media_type.essence_str()) {
                    ArtifactInputTransport::Inline {
                        max_bytes: std::num::NonZeroU64::new(MAX_INLINE_ARTIFACT_BYTES)
                            .expect("inline artifact limit is non-zero"),
                    }
                } else {
                    ArtifactInputTransport::Unsupported
                }
            }
            ModelCapabilityId::GenerateSpeech
            | ModelCapabilityId::GenerateImage
            | ModelCapabilityId::EditImage
            | ModelCapabilityId::GenerateVideo
            | ModelCapabilityId::EditVideo
            | ModelCapabilityId::ImageToVideo
            | ModelCapabilityId::ReferenceToVideo
            | ModelCapabilityId::ExtendVideo => ArtifactInputTransport::Unsupported,
        }
    }
}

#[async_trait]
impl MediaCapabilitiesProvider for OpenAiCompatibleProvider {
    fn media_capabilities(&self) -> ProviderMediaCapabilities {
        ProviderMediaCapabilities {
            provider: self.name.clone(),
            model_tools: Vec::new(),
            models: vec![ModelMediaCapabilities {
                model_pattern: "*".to_string(),
                tools: vec![compatible_transcribe_audio_tool_spec()],
            }],
        }
    }

    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let operation = request.operation();
        match request {
            NativeMediaRequest::TranscribeAudio(request) => self.transcribe_audio(request).await,
            NativeMediaRequest::GenerateImage(_)
            | NativeMediaRequest::EditImage(_)
            | NativeMediaRequest::GenerateVideo(_)
            | NativeMediaRequest::EditVideo(_)
            | NativeMediaRequest::ImageToVideo(_)
            | NativeMediaRequest::ReferenceToVideo(_)
            | NativeMediaRequest::ExtendVideo(_)
            | NativeMediaRequest::GenerateSpeech(_) => {
                anyhow::bail!(
                    "{} does not support media operation {}",
                    self.name,
                    operation.as_str()
                )
            }
        }
    }
}

#[cfg(test)]
mod media_capability_tests {
    use super::*;

    #[test]
    fn transcription_tool_exposes_a_top_level_prompt() {
        let schema = compatible_transcribe_audio_tool_spec().parameters_schema;

        assert_eq!(schema["properties"]["prompt"]["type"], "string");
        assert!(
            schema["properties"]["provider_options"]["properties"]
                .get("prompt")
                .is_none()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nenjo_tool_api::{ArtifactId, ArtifactSize, Sha256Digest};
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use uuid::Uuid;

    use super::*;
    use crate::{
        ArtifactInput, ArtifactInputSource, ArtifactRef, ChatMessage, PreparedArtifact,
        PreparedArtifactInputs, ToolResultMessage,
    };

    fn make_provider(name: &str, url: &str, key: Option<&str>) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(name, url, key, AuthStyle::Bearer)
    }

    fn prepared_artifact(media_type: &str, bytes: &[u8]) -> (ArtifactRef, PreparedArtifactInputs) {
        let bytes: Arc<[u8]> = Arc::from(bytes);
        let reference = ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap(),
            MediaType::parse(media_type).unwrap(),
            ArtifactSize::new(bytes.len() as u64),
        );
        let prepared = PreparedArtifact::new(reference.clone(), bytes).unwrap();
        (reference, PreparedArtifactInputs::new([prepared]))
    }

    async fn capture_one_json_request() -> (
        String,
        tokio::sync::oneshot::Receiver<(String, serde_json::Value)>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock compatible endpoint");
        let address = listener.local_addr().expect("mock endpoint address");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept provider request");
            let mut request = Vec::new();
            let header_end = loop {
                let mut chunk = [0_u8; 4096];
                let read = stream
                    .read(&mut chunk)
                    .await
                    .expect("read provider request");
                assert!(read > 0, "provider request ended before headers");
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8(request[..header_end].to_vec())
                .expect("provider request headers are UTF-8");
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("content length"))
                })
                .expect("provider request content length");
            while request.len() - header_end < content_length {
                let mut chunk = [0_u8; 4096];
                let read = stream.read(&mut chunk).await.expect("read provider body");
                assert!(read > 0, "provider request body ended early");
                request.extend_from_slice(&chunk[..read]);
            }
            let body = serde_json::from_slice(&request[header_end..header_end + content_length])
                .expect("provider request body is JSON");
            sender
                .send((headers, body))
                .expect("return captured provider request");
            let response = r#"{"choices":[{"message":{"content":"grounded"}}],"usage":{"prompt_tokens":7,"completion_tokens":2}}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write provider response");
        });
        (format!("http://{address}/v1"), receiver)
    }

    async fn capture_one_sse_request() -> (String, tokio::sync::oneshot::Receiver<serde_json::Value>)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock compatible streaming endpoint");
        let address = listener.local_addr().expect("mock endpoint address");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept provider request");
            let mut request = Vec::new();
            let header_end = loop {
                let mut chunk = [0_u8; 4096];
                let read = stream
                    .read(&mut chunk)
                    .await
                    .expect("read provider request");
                assert!(read > 0, "provider request ended before headers");
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8(request[..header_end].to_vec())
                .expect("provider request headers are UTF-8");
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("content length"))
                })
                .expect("provider request content length");
            while request.len() - header_end < content_length {
                let mut chunk = [0_u8; 4096];
                let read = stream.read(&mut chunk).await.expect("read provider body");
                assert!(read > 0, "provider request body ended early");
                request.extend_from_slice(&chunk[..read]);
            }
            let body = serde_json::from_slice(&request[header_end..header_end + content_length])
                .expect("provider request body is JSON");
            sender.send(body).expect("return captured provider request");

            let frames = [
                json!({"choices": [{"delta": {
                    "content": "hel",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call-1",
                        "function": {"name": "read_", "arguments": "{\"start\":"}
                    }]
                }}]}),
                json!({"choices": [{"delta": {
                    "content": "lo",
                    "tool_calls": [{
                        "index": 0,
                        "function": {"name": "artifact", "arguments": "1}"}
                    }]
                }}]}),
                json!({
                    "choices": [],
                    "usage": {"prompt_tokens": 7, "completion_tokens": 2}
                }),
            ];
            let mut response = frames
                .iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect::<String>();
            response.push_str("data: [DONE]\n\n");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write streaming provider response");
        });
        (format!("http://{address}/v1"), receiver)
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

    #[test]
    fn developer_role_is_not_assumed_for_generic_compatible_endpoints() {
        let p = make_provider("OpenAI-compatible", "https://example.com", None);
        assert!(!p.supports_developer_role("gpt-5.1"));
        assert!(!p.supports_developer_role("gpt-4.1"));
        assert!(!p.supports_developer_role("o4-mini"));
        assert!(!p.supports_developer_role("gpt-4o"));
        assert!(!p.supports_developer_role("llama-3.3-70b"));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        use crate::traits::{ChatRequest, ConversationMessage};
        let p = make_provider("Venice", "https://api.venice.ai", None);
        let messages = vec![ConversationMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
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
                    content: Some(ChatCompletionsContent::text("You are Nenjo")),
                    tool_call_id: None,
                    tool_calls: None,
                },
                Message {
                    role: "user".to_string(),
                    content: Some(ChatCompletionsContent::text("hello")),
                    tool_call_id: None,
                    tool_calls: None,
                },
            ],
            temperature: 0.4,
            stream: Some(false),
            stream_options: None,
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
    fn developer_role_is_mapped_to_user_for_generic_compatible_endpoints() {
        let messages = vec![ConversationMessage::developer("Use the response tool")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };
        let converted = OpenAiCompatibleProvider::convert_messages(&request, false).unwrap();

        assert_eq!(converted[0].role, "user");
        assert_eq!(
            serde_json::to_value(converted[0].content.as_ref().unwrap()).unwrap(),
            serde_json::json!("Use the response tool")
        );
    }

    #[test]
    fn developer_role_is_preserved_when_supported() {
        let messages = vec![ConversationMessage::developer("Use the response tool")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };
        let converted = OpenAiCompatibleProvider::convert_messages(&request, true).unwrap();

        assert_eq!(converted[0].role, "developer");
    }

    #[test]
    fn compatible_chat_serializes_prepared_image_bytes_as_a_data_url() {
        let (reference, prepared) = prepared_artifact("image/png", b"png");
        let messages = vec![ConversationMessage::chat(
            ChatMessage::user("Describe this image").with_artifacts(vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )]),
        )];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: Some(&prepared),
        };

        let converted = OpenAiCompatibleProvider::convert_messages(&request, false).unwrap();

        assert_eq!(
            serde_json::to_value(converted[0].content.as_ref().unwrap()).unwrap(),
            serde_json::json!([
                {"type": "text", "text": "Describe this image"},
                {
                    "type": "image_url",
                    "image_url": {
                        "url": "data:image/png;base64,cG5n",
                        "detail": "auto"
                    }
                }
            ])
        );
    }

    #[tokio::test]
    async fn compatible_http_request_carries_prepared_image_bytes_and_auth() {
        let (base_url, captured) = capture_one_json_request().await;
        let provider = make_provider("compatible", &base_url, Some("test-key"));
        let (reference, prepared) = prepared_artifact("image/png", b"png");
        let messages = vec![ConversationMessage::chat(
            ChatMessage::user("Describe this image").with_artifacts(vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )]),
        )];

        let response = provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    native_tools: None,
                    prepared_artifacts: Some(&prepared),
                },
                "vision-model",
                0.0,
            )
            .await
            .expect("compatible chat response");
        let (headers, body) = captured.await.expect("captured provider request");

        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        assert_eq!(
            body["messages"][0]["content"],
            serde_json::json!([
                {"type": "text", "text": "Describe this image"},
                {"type": "image_url", "image_url": {
                    "url": "data:image/png;base64,cG5n", "detail": "auto"
                }}
            ])
        );
        assert_eq!(response.text.as_deref(), Some("grounded"));
        assert_eq!(response.usage.input_tokens, 7);
        assert_eq!(response.usage.output_tokens, 2);
    }

    #[tokio::test]
    async fn compatible_streaming_accumulates_text_tools_and_usage() {
        let (base_url, captured) = capture_one_sse_request().await;
        let provider = OpenAiCompatibleProvider::new_with_dialect(
            "vllm",
            &base_url,
            Some("test-key"),
            AuthStyle::Bearer,
            ChatArtifactDialect::Vllm,
        );
        let messages = vec![ConversationMessage::user("hello")];
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();

        let response = provider
            .chat_stream(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    native_tools: None,
                    prepared_artifacts: None,
                },
                "test-model",
                0.0,
                events_tx,
            )
            .await
            .expect("streaming chat response");
        let body = captured.await.expect("captured provider request");

        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(response.text.as_deref(), Some("hello"));
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call-1");
        assert_eq!(response.tool_calls[0].name, "read_artifact");
        assert_eq!(response.tool_calls[0].arguments, r#"{"start":1}"#);
        assert_eq!(response.usage.input_tokens, 7);
        assert_eq!(response.usage.output_tokens, 2);
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProviderStreamEvent::TextDelta(delta)) if delta == "hel"
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProviderStreamEvent::TextDelta(delta)) if delta == "lo"
        ));
    }

    #[tokio::test]
    async fn generic_compatible_provider_preserves_buffered_chat_behavior() {
        let (base_url, captured) = capture_one_json_request().await;
        let provider = make_provider("compatible", &base_url, Some("test-key"));
        let messages = vec![ConversationMessage::user("hello")];
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();

        let response = provider
            .chat_stream(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    native_tools: None,
                    prepared_artifacts: None,
                },
                "test-model",
                0.0,
                events_tx,
            )
            .await
            .expect("buffered compatible response");
        let (_, body) = captured.await.expect("captured provider request");

        assert_eq!(body["stream"], false);
        assert_eq!(response.text.as_deref(), Some("grounded"));
        assert!(events_rx.try_recv().is_err());
    }

    #[test]
    fn compatible_chat_projects_tool_artifacts_into_a_user_media_message() {
        let (reference, prepared) = prepared_artifact("application/pdf", b"pdf");
        let artifact_id = reference.id();
        let messages = vec![ConversationMessage::tool_result(
            ToolResultMessage::text("call-1", "Artifact metadata").with_artifact(reference),
        )];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: Some(&prepared),
        };

        let converted = OpenAiCompatibleProvider::convert_messages(&request, false).unwrap();

        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].role, "tool");
        assert_eq!(converted[1].role, "user");
        assert_eq!(
            serde_json::to_value(converted[1].content.as_ref().unwrap()).unwrap(),
            serde_json::json!([
                {"type": "text", "text": "Inspect the attached artifact."},
                {"type": "file", "file": {
                    "filename": format!("artifact-{artifact_id}.pdf"),
                    "file_data": "data:application/pdf;base64,cGRm"
                }}
            ])
        );
    }

    #[test]
    fn compatible_chat_rejects_video_parts() {
        let (reference, prepared) = prepared_artifact("video/mp4", b"video");
        let messages = vec![ConversationMessage::chat(
            ChatMessage::user("Describe this video").with_artifacts(vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )]),
        )];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: Some(&prepared),
        };

        let error = OpenAiCompatibleProvider::convert_messages(&request, false).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported Chat Completions media type")
        );
    }

    #[test]
    fn compatible_transport_supports_standard_artifacts_and_transcription_but_not_video() {
        let provider = make_provider("compatible", "https://example.com/v1", Some("key"));
        let image = MediaType::parse("image/png").unwrap();
        let document = MediaType::parse("application/pdf").unwrap();
        let chat_audio = MediaType::parse("audio/wav").unwrap();
        let transcription_audio = MediaType::parse("audio/flac").unwrap();
        let video = MediaType::parse("video/mp4").unwrap();

        assert!(matches!(
            provider.artifact_input_transport("vision", ModelCapabilityId::Chat, &image),
            ArtifactInputTransport::Inline { .. }
        ));
        assert!(matches!(
            provider.artifact_input_transport(
                "document-model",
                ModelCapabilityId::AnalyzeDocument,
                &document,
            ),
            ArtifactInputTransport::Inline { .. }
        ));
        assert!(matches!(
            provider.artifact_input_transport("audio-model", ModelCapabilityId::Chat, &chat_audio),
            ArtifactInputTransport::Inline { .. }
        ));
        assert!(matches!(
            provider.artifact_input_transport(
                "transcriber",
                ModelCapabilityId::TranscribeAudio,
                &transcription_audio,
            ),
            ArtifactInputTransport::Inline { .. }
        ));
        assert_eq!(
            provider.artifact_input_transport("video", ModelCapabilityId::Chat, &video),
            ArtifactInputTransport::Unsupported
        );
        assert_eq!(
            provider.artifact_input_transport("video", ModelCapabilityId::AnalyzeVideo, &video,),
            ArtifactInputTransport::Unsupported
        );
    }

    #[test]
    fn responses_fallback_is_disabled_for_artifact_requests() {
        let provider = make_provider("compatible", "https://example.com/v1", Some("key"));
        let text_messages = vec![ConversationMessage::user("hello")];
        let text_request = ChatRequest {
            messages: &text_messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };
        assert!(provider.responses_fallback_allowed(&text_request));

        let (reference, prepared) = prepared_artifact("image/png", b"png");
        let artifact_messages = vec![ConversationMessage::chat(
            ChatMessage::user("inspect").with_artifacts(vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )]),
        )];
        let artifact_request = ChatRequest {
            messages: &artifact_messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: Some(&prepared),
        };
        assert!(!provider.responses_fallback_allowed(&artifact_request));
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
        use crate::traits::{ChatRequest, ConversationMessage};
        let providers = vec![
            make_provider("Venice", "https://api.venice.ai", None),
            make_provider("Moonshot", "https://api.moonshot.cn", None),
            make_provider("GLM", "https://open.bigmodel.cn", None),
            make_provider("MiniMax", "https://api.minimax.io/v1", None),
            make_provider("Groq", "https://api.groq.com/openai", None),
            make_provider("Mistral", "https://api.mistral.ai", None),
            make_provider("xAI", "https://api.x.ai", None),
        ];

        for p in providers {
            let messages = vec![ConversationMessage::user("test")];
            let request = ChatRequest {
                messages: &messages,
                tools: None,
                native_tools: None,
                prepared_artifacts: None,
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
        // MiniMax uses /v1/text/chatcompletion_v2 — non-standard path used as-is.
        let p = make_provider(
            "minimax",
            "https://api.minimax.io/v1/text/chatcompletion_v2",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://api.minimax.io/v1/text/chatcompletion_v2"
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
