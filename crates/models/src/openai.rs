//! OpenAI provider. Authenticates via Bearer token.

use crate::ToolSpec;
use crate::audio_data_uri::decode_base64_data_uri;
use crate::native::{
    MediaCapabilitiesProvider, MediaExecutionMode, MediaInputAsset, MediaOperation,
    MediaOutputAsset, MediaOutputFormat, MediaToolSpec as NativeMediaToolSpec,
    ModelMediaCapabilities, NativeMediaRequest, NativeMediaResponse, ProviderMediaCapabilities,
    TranscribeAudioRequest, TranscriptSegment,
};
use crate::openai_tools::{ProviderToolSpec, convert_tools};
use crate::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};
use anyhow::Context;
use async_trait::async_trait;
use reqwest::Client;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub struct OpenAiProvider {
    api_key: Option<String>,
    client: Client,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<NativeMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ProviderToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
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

#[derive(Debug, Serialize)]
struct ImageGenerationRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    background: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct ImageGenerationResponse {
    data: Vec<ImageGenerationData>,
}

#[derive(Debug, Deserialize)]
struct ImageGenerationData {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
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
    #[serde(default)]
    text: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedAudioDataUri {
    mime_type: String,
    bytes: Vec<u8>,
    filename: String,
}

fn provider_option_str<'a>(options: &'a Value, key: &str) -> Option<&'a str> {
    options.get(key).and_then(Value::as_str)
}

fn provider_option_f64(options: &Value, key: &str) -> Option<f64> {
    options.get(key).and_then(Value::as_f64)
}

fn openai_generate_image_tool_spec() -> NativeMediaToolSpec {
    let capability = MediaOperation::GenerateImage;
    NativeMediaToolSpec {
        capability,
        tool_name: capability.tool_name().unwrap().to_string(),
        description: "Generate an image with the configured OpenAI image model.".to_string(),
        execution: MediaExecutionMode::Immediate,
        parameters_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string"},
                "n": {"type": "integer", "minimum": 1},
                "size": {
                    "type": "string",
                    "enum": ["1024x1024", "1024x1536", "1536x1024", "auto"]
                },
                "output_format": {"type": "string", "enum": ["url", "base64"]},
                "provider_options": {
                    "type": "object",
                    "properties": {
                        "background": {
                            "type": "string",
                            "enum": ["transparent", "opaque", "auto"]
                        },
                        "output_format": {
                            "type": "string",
                            "enum": ["png", "webp", "jpeg"]
                        },
                        "quality": {
                            "type": "string",
                            "enum": ["low", "medium", "high", "auto"]
                        }
                    },
                    "additionalProperties": false
                }
            },
            "required": ["prompt"]
        }),
    }
}

fn openai_transcribe_audio_tool_spec() -> NativeMediaToolSpec {
    let capability = MediaOperation::TranscribeAudio;
    NativeMediaToolSpec {
        capability,
        tool_name: capability.tool_name().unwrap().to_string(),
        description: "Transcribe an audio data URI with the configured OpenAI transcription model."
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

fn image_mime_type(output_format: Option<&str>) -> String {
    match output_format {
        Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
    .to_string()
}

fn audio_file_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/wave" | "audio/x-wav" => "wav",
        "audio/webm" | "video/webm" => "webm",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "video/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        _ => "bin",
    }
}

fn prepare_openai_audio_data_uri(data_uri: &str) -> anyhow::Result<DecodedAudioDataUri> {
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

fn openai_audio_part(asset: &MediaInputAsset) -> anyhow::Result<Part> {
    match asset {
        MediaInputAsset::DataUri { data_uri } => {
            let decoded = prepare_openai_audio_data_uri(data_uri)?;
            Part::bytes(decoded.bytes)
                .file_name(decoded.filename)
                .mime_str(&decoded.mime_type)
                .context("failed to build OpenAI audio upload part")
        }
        MediaInputAsset::Url { .. } => {
            anyhow::bail!(
                "OpenAI audio transcription requires a data_uri input; worker-side URL fetching is not supported"
            )
        }
        MediaInputAsset::ProviderFileId { .. } => {
            anyhow::bail!(
                "OpenAI audio transcription requires a data_uri input; provider file ids are not supported"
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

impl OpenAiProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self {
            api_key: api_key.map(ToString::to_string),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn is_reasoning_model(model: &str) -> bool {
        let m = model.to_lowercase();
        m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
    }

    fn is_developer_role_model(model: &str) -> bool {
        let m = model.to_lowercase();
        Self::is_reasoning_model(&m)
            || m.starts_with("gpt-5")
            || m.starts_with("gpt-4.5")
            || m.starts_with("gpt-4.1")
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<ProviderToolSpec>> {
        convert_tools(tools, crate::sanitize_tool_name)
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

    async fn generate_image(
        &self,
        request: crate::native::GenerateImageRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let response_format = match request.output_format {
            MediaOutputFormat::Url => None,
            MediaOutputFormat::Base64 => Some("b64_json"),
        };
        let body = ImageGenerationRequest {
            model: &request.model,
            prompt: &request.prompt,
            n: request.n,
            size: request.size.as_deref(),
            response_format,
            background: provider_option_str(&request.provider_options, "background"),
            output_format: provider_option_str(&request.provider_options, "output_format"),
            quality: provider_option_str(&request.provider_options, "quality"),
        };
        let mime_type = image_mime_type(body.output_format);

        let response = self
            .client
            .post("https://api.openai.com/v1/images/generations")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("OpenAI", response).await);
        }

        let images: ImageGenerationResponse = response.json().await?;
        let mut assets = Vec::new();
        let mut revised_prompts = Vec::new();

        for image in images.data {
            if let Some(prompt) = image.revised_prompt {
                revised_prompts.push(prompt);
            }
            if let Some(url) = image.url {
                assets.push(MediaOutputAsset::Url {
                    url,
                    mime_type: Some(mime_type.clone()),
                });
            } else if let Some(data) = image.b64_json {
                assets.push(MediaOutputAsset::Base64 {
                    data,
                    mime_type: Some(mime_type.clone()),
                });
            }
        }

        if assets.is_empty() {
            anyhow::bail!("OpenAI image generation returned no assets");
        }

        let metadata = if revised_prompts.is_empty() {
            None
        } else {
            Some(serde_json::json!({ "revised_prompts": revised_prompts }))
        };

        Ok(NativeMediaResponse::Assets { assets, metadata })
    }

    async fn transcribe_audio(
        &self,
        request: TranscribeAudioRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let response_format =
            provider_option_str(&request.provider_options, "response_format").unwrap_or("json");
        if !matches!(response_format, "json" | "verbose_json") {
            anyhow::bail!("OpenAI transcription response_format must be 'json' or 'verbose_json'");
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
            .part("file", openai_audio_part(&request.audio)?);

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

        let response = self
            .client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("OpenAI", response).await);
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
impl ModelProvider for OpenAiProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let is_reasoning = Self::is_reasoning_model(model);
        let tools = Self::convert_tools(request.tools);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            // Reasoning models (o1/o3/o4) require temperature=1; omit it to use the default.
            temperature: if is_reasoning {
                None
            } else {
                Some(temperature)
            },
            max_completion_tokens: Some(if is_reasoning { 65536 } else { 16384 }),
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("OpenAI", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
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
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        let m = model.to_lowercase();
        Some(if m.contains("gpt-5") {
            // GPT-5.x: 1M
            1_000_000
        } else if m.contains("o1") || m.contains("o3") || m.contains("o4") {
            // Reasoning models: 200K
            200_000
        } else if m.contains("gpt-4o") {
            // GPT-4o / GPT-4o-mini: 128K
            128_000
        } else {
            128_000
        })
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        Self::is_developer_role_model(model)
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
}

#[async_trait]
impl MediaCapabilitiesProvider for OpenAiProvider {
    fn media_capabilities(&self) -> ProviderMediaCapabilities {
        ProviderMediaCapabilities {
            provider: "openai".to_string(),
            model_tools: Vec::new(),
            models: vec![
                ModelMediaCapabilities {
                    model_pattern: "gpt-image-*".to_string(),
                    tools: vec![openai_generate_image_tool_spec()],
                },
                ModelMediaCapabilities {
                    model_pattern: "whisper-*".to_string(),
                    tools: vec![openai_transcribe_audio_tool_spec()],
                },
                ModelMediaCapabilities {
                    model_pattern: "gpt-4o-transcribe*".to_string(),
                    tools: vec![openai_transcribe_audio_tool_spec()],
                },
                ModelMediaCapabilities {
                    model_pattern: "gpt-4o-mini-transcribe*".to_string(),
                    tools: vec![openai_transcribe_audio_tool_spec()],
                },
            ],
        }
    }

    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let operation = request.operation();
        match request {
            NativeMediaRequest::GenerateImage(request) => self.generate_image(request).await,
            NativeMediaRequest::TranscribeAudio(request) => self.transcribe_audio(request).await,
            NativeMediaRequest::EditImage(_)
            | NativeMediaRequest::GenerateVideo(_)
            | NativeMediaRequest::EditVideo(_)
            | NativeMediaRequest::ImageToVideo(_)
            | NativeMediaRequest::ReferenceToVideo(_)
            | NativeMediaRequest::ExtendVideo(_)
            | NativeMediaRequest::GenerateSpeech(_) => {
                anyhow::bail!(
                    "OpenAI media operation {operation:?} is declared but not implemented in this pass"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = OpenAiProvider::new(Some("sk-proj-abc123"));
        assert_eq!(p.api_key.as_deref(), Some("sk-proj-abc123"));
    }

    #[test]
    fn developer_role_supported_for_newer_openai_models() {
        let p = OpenAiProvider::new(None);
        assert!(p.supports_developer_role("gpt-5.1"));
        assert!(p.supports_developer_role("gpt-4.1"));
        assert!(p.supports_developer_role("o3"));
        assert!(!p.supports_developer_role("gpt-4o"));
    }

    #[test]
    fn creates_without_key() {
        let p = OpenAiProvider::new(None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn creates_with_empty_key() {
        let p = OpenAiProvider::new(Some(""));
        assert_eq!(p.api_key.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = OpenAiProvider::new(None);
        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
        };
        let result = p.chat(request, "gpt-4o", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = OpenAiProvider::new(None);
        let messages = vec![
            ChatMessage::system("You are Nenjo"),
            ChatMessage::user("test"),
        ];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
        };
        let result = p.chat(request, "gpt-4o", 0.5).await;
        assert!(result.is_err());
    }

    #[test]
    fn media_capabilities_include_image_generation() {
        let p = OpenAiProvider::new(None);
        let capabilities = MediaCapabilitiesProvider::media_capabilities(&p);
        assert_eq!(capabilities.provider, "openai");
        assert!(capabilities.models.iter().any(|model| {
            model
                .operations()
                .any(|op| op == MediaOperation::GenerateImage)
        }));
    }

    #[test]
    fn media_capabilities_include_audio_transcription() {
        let p = OpenAiProvider::new(None);
        let capabilities = MediaCapabilitiesProvider::media_capabilities(&p);

        assert!(capabilities.models.iter().any(|model| {
            model.model_pattern == "whisper-*"
                && model
                    .operations()
                    .any(|op| op == MediaOperation::TranscribeAudio)
        }));
    }

    #[test]
    fn transcription_tool_exposes_a_top_level_prompt() {
        let schema = openai_transcribe_audio_tool_spec().parameters_schema;

        assert_eq!(schema["properties"]["prompt"]["type"], "string");
        assert!(
            schema["properties"]["provider_options"]["properties"]
                .get("prompt")
                .is_none()
        );
    }

    #[test]
    fn audio_data_uri_decodes_supported_mime_type() {
        let decoded = prepare_openai_audio_data_uri("data:audio/webm;base64,YXVkaW8=")
            .expect("valid audio data uri");

        assert_eq!(decoded.mime_type, "audio/webm");
        assert_eq!(decoded.filename, "audio.webm");
        assert_eq!(decoded.bytes, b"audio");
    }

    #[test]
    fn audio_data_uri_rejects_non_audio_mime_type() {
        let error = prepare_openai_audio_data_uri("data:text/plain;base64,YXVkaW8=")
            .expect_err("unsupported mime type");

        assert!(error.to_string().contains("MIME type 'text/plain'"));
    }
}
