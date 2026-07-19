//! xAI provider.
//!
//! Chat uses xAI's OpenAI-compatible chat completions surface. Provider-native
//! media operations use xAI-specific endpoints under `https://api.x.ai/v1`.

use anyhow::Context;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

use crate::audio_data_uri::decode_base64_data_uri;
use crate::compatible::{AuthStyle, OpenAiCompatibleProvider};
use crate::native::{
    EditImageRequest, EditVideoRequest, ExtendVideoRequest, GenerateVideoRequest,
    ImageToVideoRequest, MediaCapabilitiesProvider, MediaExecutionMode, MediaInputAsset,
    MediaOperation, MediaOutputAsset, MediaOutputFormat, MediaToolSpec, ModelMediaCapabilities,
    NativeMediaJob, NativeMediaJobStatus, NativeMediaRequest, NativeMediaResponse,
    NativeModelToolId, ProviderMediaCapabilities, ProviderNativeModelToolSpec,
    ReferenceToVideoRequest, TranscribeAudioRequest, TranscriptSegment, media_input_schema,
};
use crate::traits::{
    ChatMessage, ChatRequest, ChatResponse, ModelProvider, ProviderStreamEvent, ProviderToolTrace,
    TokenUsage, ToolCall,
};

pub const XAI_DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";

pub struct XAiProvider {
    api_key: Option<String>,
    base_url: String,
    chat: OpenAiCompatibleProvider,
    client: Client,
}

#[derive(Debug, Serialize)]
struct ImageGenerationRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct ImageEditRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    image: XaiMediaInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct VideoRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(rename = "duration", skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<XaiMediaInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_images: Option<Vec<XaiMediaInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video: Option<XaiMediaInput>,
}

#[derive(Debug, Serialize)]
struct XaiMediaInput {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
}

#[derive(Debug)]
struct DecodedAudioDataUri {
    mime_type: String,
    bytes: Vec<u8>,
    filename: String,
}

#[derive(Debug)]
enum XaiSttAudioInput {
    File(Box<Part>),
    Url(String),
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
struct VideoStartResponse {
    request_id: String,
}

#[derive(Debug, Deserialize)]
struct VideoPollResponse {
    status: String,
    #[serde(default)]
    video: Option<VideoAsset>,
    #[serde(default)]
    error: Option<XaiError>,
}

#[derive(Debug, Deserialize)]
struct VideoAsset {
    url: String,
    #[serde(default)]
    duration: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct XaiError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XaiSttResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    words: Vec<XaiSttWord>,
    #[serde(default)]
    channels: Vec<XaiSttChannel>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct XaiSttWord {
    text: String,
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    #[serde(default)]
    speaker: Option<i64>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct XaiSttChannel {
    index: u32,
    text: String,
    #[serde(default)]
    words: Vec<XaiSttWord>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInput>,
    tools: Vec<ResponsesTool>,
    temperature: f64,
    stream: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ResponsesInput {
    Message {
        role: String,
        content: String,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Serialize, PartialEq)]
struct ResponsesTool {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<ResponsesOutput>,
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesOutput {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    content: Vec<ResponsesContent>,
    #[serde(default)]
    status: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ResponsesContent {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    annotations: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesUsage {
    #[serde(default, alias = "prompt_tokens")]
    input_tokens: u64,
    #[serde(default, alias = "completion_tokens")]
    output_tokens: u64,
}

fn xai_media_input(asset: MediaInputAsset, image_edit_input: bool) -> XaiMediaInput {
    let kind = match &asset {
        MediaInputAsset::ProviderFileId { .. } => None,
        MediaInputAsset::Url { .. } | MediaInputAsset::DataUri { .. } => {
            image_edit_input.then_some("image_url")
        }
    };
    match asset {
        MediaInputAsset::Url { url } => XaiMediaInput {
            kind,
            url: Some(url),
            file_id: None,
        },
        MediaInputAsset::DataUri { data_uri } => XaiMediaInput {
            kind,
            url: Some(data_uri),
            file_id: None,
        },
        MediaInputAsset::ProviderFileId { file_id } => XaiMediaInput {
            kind,
            url: None,
            file_id: Some(file_id),
        },
    }
}

fn xai_image_tool_spec(operation: MediaOperation) -> MediaToolSpec {
    let mut properties = json!({
        "prompt": {"type": "string"},
        "n": {"type": "integer", "minimum": 1},
        "aspect_ratio": {
            "type": "string",
            "enum": [
                "1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3",
                "2:1", "1:2", "19.5:9", "9:19.5", "20:9", "9:20", "auto"
            ]
        },
        "resolution": {"type": "string", "enum": ["1k", "2k"]},
        "output_format": {"type": "string", "enum": ["url", "base64"]},
        "provider_options": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    });
    let required = match operation {
        MediaOperation::GenerateImage => vec!["prompt"],
        MediaOperation::EditImage => {
            properties["image"] = media_input_schema();
            vec!["prompt", "image"]
        }
        other => panic!("unsupported xAI image operation {other:?}"),
    };

    MediaToolSpec {
        capability: operation,
        tool_name: operation.tool_name().unwrap().to_string(),
        description: match operation {
            MediaOperation::GenerateImage => {
                "Generate an image with the configured xAI image model."
            }
            MediaOperation::EditImage => "Edit an image with the configured xAI image model.",
            _ => unreachable!(),
        }
        .to_string(),
        parameters_schema: json!({
            "type": "object",
            "properties": properties,
            "required": required
        }),
        execution: MediaExecutionMode::Immediate,
    }
}

fn xai_video_provider_options() -> Value {
    json!({
        "type": "object",
        "properties": {
            "poll_timeout_ms": {
                "type": "integer",
                "minimum": 1
            }
        },
        "additionalProperties": false
    })
}

fn xai_video_base_properties() -> Value {
    json!({
        "prompt": {"type": "string"},
        "duration_seconds": {"type": "integer", "minimum": 1},
        "aspect_ratio": {"type": "string", "enum": ["16:9", "9:16", "1:1"]},
        "resolution": {"type": "string", "enum": ["480p", "720p"]},
        "provider_options": xai_video_provider_options()
    })
}

fn xai_video_tool_spec(operation: MediaOperation) -> MediaToolSpec {
    let mut properties = xai_video_base_properties();
    let required = match operation {
        MediaOperation::GenerateVideo => vec!["prompt"],
        MediaOperation::ImageToVideo => {
            properties["image"] = media_input_schema();
            vec!["prompt", "image"]
        }
        MediaOperation::ReferenceToVideo => {
            properties["reference_images"] = json!({
                "type": "array",
                "items": media_input_schema(),
                "minItems": 1,
                "maxItems": 7
            });
            properties["duration_seconds"]["maximum"] = json!(10);
            vec!["prompt", "reference_images"]
        }
        MediaOperation::EditVideo => {
            properties = json!({
                "prompt": {"type": "string"},
                "video": media_input_schema(),
                "provider_options": xai_video_provider_options()
            });
            vec!["prompt", "video"]
        }
        MediaOperation::ExtendVideo => {
            properties = json!({
                "prompt": {"type": "string"},
                "video": media_input_schema(),
                "duration_seconds": {
                    "type": "integer",
                    "minimum": 2,
                    "maximum": 10
                },
                "provider_options": xai_video_provider_options()
            });
            vec!["prompt", "video"]
        }
        other => panic!("unsupported xAI video operation {other:?}"),
    };

    MediaToolSpec {
        capability: operation,
        tool_name: operation.tool_name().unwrap().to_string(),
        description: match operation {
            MediaOperation::GenerateVideo => "Start an asynchronous xAI video generation job. A successful call means the render was queued, not finished; use wait with kind=media for the returned operation_id until it completes. Do not call generate_video again for the same prompt unless the user explicitly asks for another independent video.",
            MediaOperation::EditVideo => "Start an asynchronous xAI video editing job. A successful call means the render was queued, not finished; use wait with kind=media for the returned operation_id until it completes. Do not call edit_video again for the same request unless the user explicitly asks for another independent edit.",
            MediaOperation::ImageToVideo => "Start an asynchronous xAI image-to-video job. A successful call means the render was queued, not finished; use wait with kind=media for the returned operation_id until it completes. Do not call image_to_video again for the same request unless the user explicitly asks for another independent video.",
            MediaOperation::ReferenceToVideo => "Start an asynchronous xAI reference-to-video job. A successful call means the render was queued, not finished; use wait with kind=media for the returned operation_id until it completes. Do not call reference_to_video again for the same request unless the user explicitly asks for another independent video.",
            MediaOperation::ExtendVideo => "Start an asynchronous xAI video extension job. A successful call means the render was queued, not finished; use wait with kind=media for the returned operation_id until it completes. Do not call extend_video again for the same request unless the user explicitly asks for another independent extension.",
            _ => unreachable!(),
        }
        .to_string(),
        parameters_schema: json!({
            "type": "object",
            "properties": properties,
            "required": required
        }),
        execution: MediaExecutionMode::AsyncJob {
            poll_supported: true,
        },
    }
}

fn xai_video_status(status: &str) -> anyhow::Result<NativeMediaJobStatus> {
    match status {
        "pending" => Ok(NativeMediaJobStatus::Running),
        "done" => Ok(NativeMediaJobStatus::Completed),
        "expired" => Ok(NativeMediaJobStatus::Expired),
        "failed" => Ok(NativeMediaJobStatus::Failed),
        other => anyhow::bail!("unknown xAI video job status '{other}'"),
    }
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

fn provider_option_str<'a>(options: &'a Value, key: &str) -> Option<&'a str> {
    options
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn provider_option_bool(options: &Value, key: &str) -> Option<bool> {
    options
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_bool)
}

fn provider_option_u64(options: &Value, key: &str) -> Option<u64> {
    options
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_u64)
}

fn provider_option_string_array(options: &Value, key: &str) -> Vec<String> {
    options
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn audio_file_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/wave" | "audio/x-wav" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/flac" => "flac",
        "audio/aac" => "aac",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "video/mp4" => "m4a",
        "video/x-matroska" | "audio/x-matroska" => "mkv",
        _ => "audio",
    }
}

fn prepare_xai_audio_data_uri(data_uri: &str) -> anyhow::Result<DecodedAudioDataUri> {
    let decoded = decode_base64_data_uri(data_uri)?;
    let mime_type = decoded.mime_type;

    let valid_audio_mime = mime_type.starts_with("audio/")
        || matches!(
            mime_type.as_str(),
            "video/mp4" | "video/x-matroska" | "application/octet-stream"
        );
    if !valid_audio_mime {
        anyhow::bail!("audio data URI MIME type '{mime_type}' is not supported by xAI STT");
    }

    let filename = format!("audio.{}", audio_file_extension(&mime_type));
    Ok(DecodedAudioDataUri {
        mime_type,
        bytes: decoded.bytes,
        filename,
    })
}

fn xai_stt_audio_input(asset: &MediaInputAsset) -> anyhow::Result<XaiSttAudioInput> {
    match asset {
        MediaInputAsset::DataUri { data_uri } => {
            let decoded = prepare_xai_audio_data_uri(data_uri)?;
            let file = Part::bytes(decoded.bytes)
                .file_name(decoded.filename)
                .mime_str(&decoded.mime_type)
                .context("failed to build xAI STT audio upload part")?;
            Ok(XaiSttAudioInput::File(Box::new(file)))
        }
        MediaInputAsset::Url { url } => Ok(XaiSttAudioInput::Url(url.clone())),
        MediaInputAsset::ProviderFileId { .. } => {
            anyhow::bail!(
                "xAI STT requires a data_uri or url input; provider file ids are not supported"
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

fn xai_stt_word_metadata(mut extra: Map<String, Value>, speaker: Option<i64>) -> Option<Value> {
    if let Some(speaker) = speaker {
        extra.insert("speaker".to_string(), json!(speaker));
    }
    json_object_or_none(extra)
}

fn xai_stt_segments(response: &XaiSttResponse) -> Vec<TranscriptSegment> {
    if !response.channels.is_empty() {
        return response
            .channels
            .iter()
            .flat_map(|channel| {
                channel.words.iter().map(|word| {
                    let mut metadata = word.extra.clone();
                    metadata.insert("channel_index".to_string(), json!(channel.index));
                    if !channel.text.trim().is_empty() {
                        metadata.insert("channel_text".to_string(), json!(channel.text));
                    }
                    for (key, value) in &channel.extra {
                        metadata.insert(format!("channel_{key}"), value.clone());
                    }
                    TranscriptSegment {
                        start_seconds: word.start,
                        end_seconds: word.end,
                        text: word.text.clone(),
                        metadata: xai_stt_word_metadata(metadata, word.speaker),
                    }
                })
            })
            .collect();
    }

    response
        .words
        .iter()
        .map(|word| TranscriptSegment {
            start_seconds: word.start,
            end_seconds: word.end,
            text: word.text.clone(),
            metadata: xai_stt_word_metadata(word.extra.clone(), word.speaker),
        })
        .collect()
}

fn xai_stt_tool_spec() -> MediaToolSpec {
    let capability = MediaOperation::TranscribeAudio;
    MediaToolSpec {
        capability,
        tool_name: capability.tool_name().unwrap().to_string(),
        description: "Transcribe an audio data URI or URL with xAI Speech to Text.".to_string(),
        execution: MediaExecutionMode::Immediate,
        parameters_schema: json!({
            "type": "object",
            "properties": {
                "audio": media_input_schema(),
                "language": {"type": "string"},
                "provider_options": {
                    "type": "object",
                    "properties": {
                        "format": {"type": "boolean"},
                        "diarize": {"type": "boolean"},
                        "filler_words": {"type": "boolean"},
                        "multichannel": {"type": "boolean"},
                        "channels": {"type": "integer", "minimum": 2, "maximum": 8},
                        "audio_format": {"type": "string", "enum": ["pcm", "mulaw", "alaw"]},
                        "sample_rate": {
                            "type": "integer",
                            "enum": [8000, 16000, 22050, 24000, 44100, 48000]
                        },
                        "keyterms": {
                            "type": "array",
                            "items": {"type": "string", "maxLength": 50},
                            "maxItems": 100
                        }
                    },
                    "additionalProperties": false
                }
            },
            "required": ["audio"]
        }),
    }
}

fn xai_native_model_tool_specs() -> Vec<ProviderNativeModelToolSpec> {
    vec![
        ProviderNativeModelToolSpec {
            id: NativeModelToolId::from("web_search"),
            provider_type: "web_search".to_string(),
            name: "web_search".to_string(),
            description: "Provider-native xAI web search for current web results and citations."
                .to_string(),
            parameters_schema: Some(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            config_schema: None,
        },
        ProviderNativeModelToolSpec {
            id: NativeModelToolId::from("x_search"),
            provider_type: "x_search".to_string(),
            name: "x_search".to_string(),
            description:
                "Provider-native xAI X search for posts, discussions, and current activity on X."
                    .to_string(),
            parameters_schema: Some(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            config_schema: None,
        },
    ]
}

fn xai_native_model_tool_spec(tool_id: &NativeModelToolId) -> Option<ProviderNativeModelToolSpec> {
    xai_native_model_tool_specs()
        .into_iter()
        .find(|spec| spec.id == *tool_id)
}

fn native_responses_tools(
    native_tools: &[NativeModelToolId],
    local_tools: Option<&[crate::ToolSpec]>,
) -> anyhow::Result<Vec<ResponsesTool>> {
    let mut tools = Vec::with_capacity(native_tools.len() + local_tools.map_or(0, <[_]>::len));
    for tool_id in native_tools {
        let tool = xai_native_model_tool_spec(tool_id)
            .ok_or_else(|| anyhow::anyhow!("xAI does not support native model tool '{tool_id}'"))?;
        tools.push(ResponsesTool {
            kind: tool.provider_type,
            name: None,
            description: None,
            parameters: None,
        });
    }

    if let Some(local_tools) = local_tools {
        tools.extend(local_tools.iter().map(|tool| ResponsesTool {
            kind: "function".to_string(),
            name: Some(crate::sanitize_tool_name(&tool.name)),
            description: Some(tool.description.clone()),
            parameters: Some(tool.parameters.clone()),
        }));
    }

    Ok(tools)
}

fn responses_input(messages: &[ChatMessage]) -> Vec<ResponsesInput> {
    let mut input = Vec::with_capacity(messages.len());

    for message in messages {
        if message.role == "assistant"
            && let Ok(value) = serde_json::from_str::<Value>(&message.content)
            && let Some(tool_calls_value) = value.get("tool_calls")
            && let Ok(tool_calls) =
                serde_json::from_value::<Vec<ToolCall>>(tool_calls_value.clone())
        {
            if let Some(content) = value
                .get("content")
                .and_then(Value::as_str)
                .and_then(|text| first_nonempty(Some(text)))
            {
                input.push(ResponsesInput::Message {
                    role: "assistant".to_string(),
                    content,
                });
            }

            input.extend(
                tool_calls
                    .into_iter()
                    .map(|call| ResponsesInput::FunctionCall {
                        kind: "function_call",
                        call_id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                    }),
            );
            continue;
        }

        if message.role == "tool"
            && let Ok(value) = serde_json::from_str::<Value>(&message.content)
            && let Some(call_id) = value.get("tool_call_id").and_then(Value::as_str)
        {
            let output = value
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            input.push(ResponsesInput::FunctionCallOutput {
                kind: "function_call_output",
                call_id: call_id.to_string(),
                output,
            });
            continue;
        }

        input.push(ResponsesInput::Message {
            role: message.role.clone(),
            content: message.content.clone(),
        });
    }

    input
}

fn responses_text(response: &ResponsesResponse) -> Option<String> {
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

fn responses_tool_calls(response: &ResponsesResponse) -> Vec<ToolCall> {
    response
        .output
        .iter()
        .filter(|item| item.kind.as_deref() == Some("function_call"))
        .filter_map(|item| {
            let name = item.name.clone()?;
            let arguments = match item.arguments.as_ref() {
                Some(Value::String(value)) => value.clone(),
                Some(value) => value.to_string(),
                None => "{}".to_string(),
            };
            Some(ToolCall {
                id: item
                    .call_id
                    .clone()
                    .or_else(|| item.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name,
                arguments,
            })
        })
        .collect()
}

fn xai_native_tool_name(output_kind: &str) -> Option<&'static str> {
    match output_kind {
        "web_search_call" => Some("web_search"),
        "x_search_call" => Some("x_search"),
        "code_interpreter_call" => Some("code_interpreter"),
        "file_search_call" => Some("file_search"),
        "mcp_call" => Some("mcp"),
        _ => None,
    }
}

fn provider_tool_trace_from_responses_output(item: &ResponsesOutput) -> Option<ProviderToolTrace> {
    let kind = item.kind.as_deref()?;
    let name = xai_native_tool_name(kind)?;
    let id = item
        .call_id
        .clone()
        .or_else(|| item.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut input = serde_json::Map::new();
    input.insert(
        "response_item_type".to_string(),
        Value::String(kind.to_string()),
    );
    if let Some(status) = &item.status {
        input.insert("status".to_string(), Value::String(status.clone()));
    }
    if let Some(arguments) = &item.arguments {
        input.insert("arguments".to_string(), arguments.clone());
    }
    if let Some(name) = &item.name {
        input.insert("name".to_string(), Value::String(name.clone()));
    }
    for key in [
        "action",
        "query",
        "queries",
        "server_label",
        "server_url",
        "vector_store_ids",
    ] {
        if let Some(value) = item.extra.get(key) {
            input.insert(key.to_string(), value.clone());
        }
    }

    let mut output = item.extra.clone();
    output.remove("action");
    output.remove("query");
    output.remove("queries");
    output.remove("server_label");
    output.remove("server_url");
    output.remove("vector_store_ids");
    if !item.content.is_empty() {
        output.insert(
            "content".to_string(),
            serde_json::to_value(&item.content).unwrap_or(Value::Null),
        );
    }

    let mut citations = Vec::new();
    for key in ["citations", "sources", "results"] {
        if let Some(value) = item.extra.get(key) {
            citations.push(value.clone());
        }
    }
    for content in &item.content {
        citations.extend(content.annotations.iter().cloned());
    }

    Some(ProviderToolTrace {
        id,
        name: name.to_string(),
        provider: "xai".to_string(),
        input: Value::Object(input),
        output: (!output.is_empty()).then_some(Value::Object(output)),
        citations,
    })
}

fn responses_provider_tool_traces(response: &ResponsesResponse) -> Vec<ProviderToolTrace> {
    response
        .output
        .iter()
        .filter_map(provider_tool_trace_from_responses_output)
        .collect()
}

#[derive(Default)]
struct ResponsesStreamState {
    text: String,
    output: HashMap<String, ResponsesOutput>,
    final_response: Option<ResponsesResponse>,
    started_provider_tools: HashSet<String>,
    completed_provider_tools: HashSet<String>,
}

impl ResponsesStreamState {
    fn into_response(self) -> ResponsesResponse {
        self.final_response.unwrap_or_else(|| ResponsesResponse {
            output: self.output.into_values().collect(),
            output_text: (!self.text.is_empty()).then_some(self.text),
            usage: None,
        })
    }
}

fn stream_event_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn stream_text_delta(value: &Value) -> Option<&str> {
    let kind = stream_event_type(value).unwrap_or_default();
    if kind.contains("output_text.delta") || kind.contains("text.delta") {
        return value.get("delta").and_then(Value::as_str);
    }
    None
}

fn stream_response(value: &Value) -> Option<ResponsesResponse> {
    let kind = stream_event_type(value).unwrap_or_default();
    if !(kind.ends_with(".completed") || kind == "response.completed") {
        return None;
    }
    value
        .get("response")
        .cloned()
        .and_then(|response| serde_json::from_value(response).ok())
}

fn stream_output_item(value: &Value) -> Option<ResponsesOutput> {
    for key in ["item", "output_item", "response_item"] {
        if let Some(item) = value.get(key)
            && let Ok(output) = serde_json::from_value::<ResponsesOutput>(item.clone())
        {
            return Some(output);
        }
    }
    serde_json::from_value::<ResponsesOutput>(value.clone()).ok()
}

fn stream_tool_phase(value: &Value, output: &ResponsesOutput) -> Option<&'static str> {
    let kind = stream_event_type(value).unwrap_or_default();
    if kind.contains(".added") || kind.contains(".in_progress") || kind.contains(".started") {
        return Some("started");
    }
    if kind.contains(".done") || kind.contains(".completed") {
        return Some("completed");
    }
    match output.status.as_deref() {
        Some("in_progress" | "running" | "searching" | "started") => Some("started"),
        Some("completed" | "done") => Some("completed"),
        _ => None,
    }
}

fn native_kind_from_stream_type(kind: &str) -> Option<&'static str> {
    [
        "web_search_call",
        "x_search_call",
        "code_interpreter_call",
        "file_search_call",
        "mcp_call",
    ]
    .into_iter()
    .find(|candidate| kind.contains(candidate))
}

fn stream_raw_provider_tool_trace(value: &Value) -> Option<ProviderToolTrace> {
    let kind = stream_event_type(value)?;
    let response_item_type = native_kind_from_stream_type(kind)?;
    let name = xai_native_tool_name(response_item_type)?;
    let id = value
        .get("call_id")
        .or_else(|| value.get("item_id"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut input = serde_json::Map::new();
    input.insert(
        "response_item_type".to_string(),
        Value::String(response_item_type.to_string()),
    );
    input.insert(
        "stream_event_type".to_string(),
        Value::String(kind.to_string()),
    );
    if let Some(status) = value.get("status").and_then(Value::as_str) {
        input.insert("status".to_string(), Value::String(status.to_string()));
    }
    for key in ["action", "query", "queries", "server_label", "server_url"] {
        if let Some(field) = value.get(key) {
            input.insert(key.to_string(), field.clone());
        }
    }

    Some(ProviderToolTrace {
        id,
        name: name.to_string(),
        provider: "xai".to_string(),
        input: Value::Object(input),
        output: None,
        citations: Vec::new(),
    })
}

fn stream_raw_provider_tool_phase(value: &Value) -> Option<&'static str> {
    let kind = stream_event_type(value)?;
    native_kind_from_stream_type(kind)?;
    if kind.contains(".done") || kind.contains(".completed") {
        Some("completed")
    } else {
        Some("started")
    }
}

fn handle_responses_stream_value(
    value: Value,
    state: &mut ResponsesStreamState,
    events: &tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
) {
    if let Some(delta) = stream_text_delta(&value)
        && !delta.is_empty()
    {
        state.text.push_str(delta);
        let _ = events.send(ProviderStreamEvent::TextDelta(delta.to_string()));
    }

    if let Some(response) = stream_response(&value) {
        state.final_response = Some(response);
    }

    if let Some(output) = stream_output_item(&value)
        && let Some(trace) = provider_tool_trace_from_responses_output(&output)
    {
        let phase = stream_tool_phase(&value, &output);
        state.output.insert(trace.id.clone(), output);
        match phase {
            Some("started") => {
                if state.started_provider_tools.insert(trace.id.clone()) {
                    let _ = events.send(ProviderStreamEvent::ProviderToolStarted(trace));
                }
            }
            Some("completed") => {
                if state.started_provider_tools.insert(trace.id.clone()) {
                    let _ = events.send(ProviderStreamEvent::ProviderToolStarted(trace.clone()));
                }
                if state.completed_provider_tools.insert(trace.id.clone()) {
                    let _ = events.send(ProviderStreamEvent::ProviderToolCompleted(trace));
                }
            }
            _ => {}
        }
        return;
    }

    if let Some(trace) = stream_raw_provider_tool_trace(&value) {
        match stream_raw_provider_tool_phase(&value) {
            Some("completed") => {
                if state.started_provider_tools.insert(trace.id.clone()) {
                    let _ = events.send(ProviderStreamEvent::ProviderToolStarted(trace.clone()));
                }
                if state.completed_provider_tools.insert(trace.id.clone()) {
                    let _ = events.send(ProviderStreamEvent::ProviderToolCompleted(trace));
                }
            }
            Some("started") => {
                if state.started_provider_tools.insert(trace.id.clone()) {
                    let _ = events.send(ProviderStreamEvent::ProviderToolStarted(trace));
                }
            }
            _ => {}
        }
    }
}

impl XAiProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self::with_base_url(api_key, XAI_DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: Option<&str>, base_url: &str) -> Self {
        let normalized_base_url = base_url.trim_end_matches('/').to_string();
        Self {
            api_key: api_key.map(ToString::to_string),
            base_url: normalized_base_url.clone(),
            chat: OpenAiCompatibleProvider::new(
                "xai",
                &normalized_base_url,
                api_key,
                AuthStyle::Bearer,
            ),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn api_key(&self) -> anyhow::Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!("xAI API key not set. Set XAI_API_KEY or edit config.toml.")
        })
    }

    async fn chat_with_native_model_tools(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        native_tools: &[NativeModelToolId],
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.api_key()?;
        let body = ResponsesRequest {
            model: model.to_string(),
            input: responses_input(request.messages),
            tools: native_responses_tools(native_tools, request.tools)?,
            temperature,
            stream: false,
        };

        let response = self
            .client
            .post(self.endpoint("/responses"))
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("xAI", response).await);
        }

        let body_text = response.text().await?;
        let response: ResponsesResponse = serde_json::from_str(&body_text).map_err(|error| {
            anyhow::anyhow!(
                "xAI Responses API decode error: {error}\nBody: {}",
                &body_text[..body_text.len().min(500)]
            )
        })?;

        let usage = response
            .usage
            .as_ref()
            .map(|usage| TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            })
            .unwrap_or_default();
        let text = responses_text(&response);
        let tool_calls = responses_tool_calls(&response);
        let provider_tool_calls = responses_provider_tool_traces(&response);

        Ok(ChatResponse {
            text,
            tool_calls,
            provider_tool_calls,
            usage,
        })
    }

    async fn chat_with_native_model_tools_streaming(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        native_tools: &[NativeModelToolId],
        events: tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.api_key()?;
        let body = ResponsesRequest {
            model: model.to_string(),
            input: responses_input(request.messages),
            tools: native_responses_tools(native_tools, request.tools)?,
            temperature,
            stream: true,
        };
        let response = self
            .client
            .post(self.endpoint("/responses"))
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("xAI", response).await);
        }

        let mut state = ResponsesStreamState::default();
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            if buffer.contains("\r\n") {
                buffer = buffer.replace("\r\n", "\n");
            }

            while let Some(split_at) = buffer.find("\n\n") {
                let frame = buffer[..split_at].to_string();
                buffer = buffer[split_at + 2..].to_string();

                for line in frame.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        handle_responses_stream_value(value, &mut state, &events);
                    }
                }
            }
        }

        if !buffer.trim().is_empty() {
            for line in buffer.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    handle_responses_stream_value(value, &mut state, &events);
                }
            }
        }

        let response = state.into_response();
        let usage = response
            .usage
            .as_ref()
            .map(|usage| TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            })
            .unwrap_or_default();
        let text = responses_text(&response);
        let tool_calls = responses_tool_calls(&response);
        let provider_tool_calls = responses_provider_tool_traces(&response);

        Ok(ChatResponse {
            text,
            tool_calls,
            provider_tool_calls,
            usage,
        })
    }

    async fn transcribe_audio(
        &self,
        request: TranscribeAudioRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key()?;
        let requested_language = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let mut form = Form::new();

        if let Some(language) = requested_language.as_ref() {
            form = form.text("language", language.clone());
        }
        if let Some(format) = provider_option_bool(&request.provider_options, "format") {
            if format && requested_language.is_none() {
                anyhow::bail!("xAI STT format=true requires a language code");
            }
            form = form.text("format", format.to_string());
        }
        if let Some(diarize) = provider_option_bool(&request.provider_options, "diarize") {
            form = form.text("diarize", diarize.to_string());
        }
        if let Some(filler_words) = provider_option_bool(&request.provider_options, "filler_words")
        {
            form = form.text("filler_words", filler_words.to_string());
        }
        if let Some(multichannel) = provider_option_bool(&request.provider_options, "multichannel")
        {
            form = form.text("multichannel", multichannel.to_string());
        }
        if let Some(channels) = provider_option_u64(&request.provider_options, "channels") {
            form = form.text("channels", channels.to_string());
        }
        if let Some(audio_format) = provider_option_str(&request.provider_options, "audio_format") {
            form = form.text("audio_format", audio_format.to_string());
        }
        if let Some(sample_rate) = provider_option_u64(&request.provider_options, "sample_rate") {
            form = form.text("sample_rate", sample_rate.to_string());
        }
        for keyterm in provider_option_string_array(&request.provider_options, "keyterms") {
            form = form.text("keyterm", keyterm);
        }

        form = match xai_stt_audio_input(&request.audio)? {
            XaiSttAudioInput::File(file) => form.part("file", *file),
            XaiSttAudioInput::Url(url) => form.text("url", url),
        };

        let response = self
            .client
            .post(self.endpoint("/stt"))
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("xAI", response).await);
        }

        let transcription: XaiSttResponse = response.json().await?;
        if transcription.text.trim().is_empty() {
            anyhow::bail!("xAI STT returned an empty transcript");
        }

        let segments = xai_stt_segments(&transcription);
        let mut metadata = transcription.extra.clone();
        if !transcription.channels.is_empty() {
            metadata.insert(
                "channels".to_string(),
                serde_json::to_value(&transcription.channels)?,
            );
        }
        metadata.insert("model".to_string(), json!(request.model));

        Ok(NativeMediaResponse::Transcript {
            text: transcription.text,
            language: transcription.language.or(requested_language),
            duration_seconds: transcription.duration,
            segments,
            metadata: json_object_or_none(metadata),
        })
    }

    async fn generate_image(
        &self,
        request: crate::native::GenerateImageRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key()?;

        let response_format = match request.output_format {
            MediaOutputFormat::Url => None,
            MediaOutputFormat::Base64 => Some("b64_json"),
        };
        let body = ImageGenerationRequest {
            model: &request.model,
            prompt: &request.prompt,
            n: request.n,
            response_format,
            aspect_ratio: request.aspect_ratio.as_deref(),
            resolution: request.resolution.as_deref(),
        };

        let response = self
            .client
            .post(self.endpoint("/images/generations"))
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("xAI", response).await);
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
                    mime_type: Some("image/jpeg".to_string()),
                });
            } else if let Some(data) = image.b64_json {
                assets.push(MediaOutputAsset::Base64 {
                    data,
                    mime_type: Some("image/jpeg".to_string()),
                });
            }
        }

        if assets.is_empty() {
            anyhow::bail!("xAI image generation returned no assets");
        }

        let metadata = if revised_prompts.is_empty() {
            None
        } else {
            Some(serde_json::json!({ "revised_prompts": revised_prompts }))
        };

        Ok(NativeMediaResponse::Assets { assets, metadata })
    }

    async fn edit_image(&self, request: EditImageRequest) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key()?;
        let response_format = match request.output_format {
            MediaOutputFormat::Url => None,
            MediaOutputFormat::Base64 => Some("b64_json"),
        };
        let body = ImageEditRequest {
            model: &request.model,
            prompt: &request.prompt,
            image: xai_media_input(request.image, true),
            response_format,
            aspect_ratio: request.aspect_ratio.as_deref(),
            resolution: request.resolution.as_deref(),
        };

        let response = self
            .client
            .post(self.endpoint("/images/edits"))
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("xAI", response).await);
        }

        self.parse_image_response(response).await
    }

    async fn parse_image_response(
        &self,
        response: reqwest::Response,
    ) -> anyhow::Result<NativeMediaResponse> {
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
                    mime_type: Some("image/jpeg".to_string()),
                });
            } else if let Some(data) = image.b64_json {
                assets.push(MediaOutputAsset::Base64 {
                    data,
                    mime_type: Some("image/jpeg".to_string()),
                });
            }
        }

        if assets.is_empty() {
            anyhow::bail!("xAI image operation returned no assets");
        }

        let metadata = if revised_prompts.is_empty() {
            None
        } else {
            Some(json!({ "revised_prompts": revised_prompts }))
        };

        Ok(NativeMediaResponse::Assets { assets, metadata })
    }

    async fn start_video_job<T: Serialize + ?Sized>(
        &self,
        path: &str,
        operation: MediaOperation,
        model: &str,
        body: &T,
    ) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key()?;
        let response = self
            .client
            .post(self.endpoint(path))
            .header("Authorization", format!("Bearer {api_key}"))
            .json(body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("xAI", response).await);
        }

        let started: VideoStartResponse = response.json().await?;
        Ok(NativeMediaResponse::Job {
            job: NativeMediaJob {
                provider: "xai".to_string(),
                operation,
                job_id: started.request_id,
                status: NativeMediaJobStatus::Queued,
                model: Some(model.to_string()),
                metadata: None,
            },
        })
    }

    async fn generate_video(
        &self,
        request: GenerateVideoRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let body = VideoRequest {
            model: &request.model,
            prompt: &request.prompt,
            duration_seconds: request.duration_seconds,
            aspect_ratio: request.aspect_ratio.as_deref(),
            resolution: request.resolution.as_deref(),
            image: None,
            reference_images: None,
            video: None,
        };
        self.start_video_job(
            "/videos/generations",
            MediaOperation::GenerateVideo,
            &request.model,
            &body,
        )
        .await
    }

    async fn image_to_video(
        &self,
        request: ImageToVideoRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let body = VideoRequest {
            model: &request.model,
            prompt: &request.prompt,
            duration_seconds: request.duration_seconds,
            aspect_ratio: request.aspect_ratio.as_deref(),
            resolution: request.resolution.as_deref(),
            image: Some(xai_media_input(request.image, false)),
            reference_images: None,
            video: None,
        };
        self.start_video_job(
            "/videos/generations",
            MediaOperation::ImageToVideo,
            &request.model,
            &body,
        )
        .await
    }

    async fn reference_to_video(
        &self,
        request: ReferenceToVideoRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let body = VideoRequest {
            model: &request.model,
            prompt: &request.prompt,
            duration_seconds: request.duration_seconds,
            aspect_ratio: request.aspect_ratio.as_deref(),
            resolution: request.resolution.as_deref(),
            image: None,
            reference_images: Some(
                request
                    .reference_images
                    .into_iter()
                    .map(|asset| xai_media_input(asset, false))
                    .collect(),
            ),
            video: None,
        };
        self.start_video_job(
            "/videos/generations",
            MediaOperation::ReferenceToVideo,
            &request.model,
            &body,
        )
        .await
    }

    async fn edit_video(&self, request: EditVideoRequest) -> anyhow::Result<NativeMediaResponse> {
        let body = VideoRequest {
            model: &request.model,
            prompt: &request.prompt,
            duration_seconds: None,
            aspect_ratio: None,
            resolution: None,
            image: None,
            reference_images: None,
            video: Some(xai_media_input(request.video, false)),
        };
        self.start_video_job(
            "/videos/edits",
            MediaOperation::EditVideo,
            &request.model,
            &body,
        )
        .await
    }

    async fn extend_video(
        &self,
        request: ExtendVideoRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        let body = VideoRequest {
            model: &request.model,
            prompt: &request.prompt,
            duration_seconds: request.duration_seconds,
            aspect_ratio: None,
            resolution: None,
            image: None,
            reference_images: None,
            video: Some(xai_media_input(request.video, false)),
        };
        self.start_video_job(
            "/videos/extensions",
            MediaOperation::ExtendVideo,
            &request.model,
            &body,
        )
        .await
    }
}

#[async_trait]
impl ModelProvider for XAiProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        if let Some(native_tools) = request.native_tools
            && !native_tools.is_empty()
        {
            return self
                .chat_with_native_model_tools(request, model, temperature, native_tools)
                .await;
        }
        self.chat.chat(request, model, temperature).await
    }

    async fn chat_stream(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        events: tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
    ) -> anyhow::Result<ChatResponse> {
        if let Some(native_tools) = request.native_tools
            && !native_tools.is_empty()
        {
            return self
                .chat_with_native_model_tools_streaming(
                    request,
                    model,
                    temperature,
                    native_tools,
                    events,
                )
                .await;
        }
        self.chat
            .chat_stream(request, model, temperature, events)
            .await
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        self.chat.context_window(model)
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        self.chat.supports_developer_role(model)
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

    async fn poll_media_job(&self, job: &NativeMediaJob) -> anyhow::Result<NativeMediaResponse> {
        MediaCapabilitiesProvider::poll_media_job(self, job).await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        self.chat.warmup().await
    }
}

#[async_trait]
impl MediaCapabilitiesProvider for XAiProvider {
    fn media_capabilities(&self) -> ProviderMediaCapabilities {
        ProviderMediaCapabilities {
            provider: "xai".to_string(),
            model_tools: xai_native_model_tool_specs(),
            models: vec![
                ModelMediaCapabilities {
                    model_pattern: "grok-imagine-image*".to_string(),
                    tools: vec![
                        xai_image_tool_spec(MediaOperation::GenerateImage),
                        xai_image_tool_spec(MediaOperation::EditImage),
                    ],
                },
                ModelMediaCapabilities {
                    model_pattern: "grok-imagine-video*".to_string(),
                    tools: vec![
                        xai_video_tool_spec(MediaOperation::GenerateVideo),
                        xai_video_tool_spec(MediaOperation::EditVideo),
                        xai_video_tool_spec(MediaOperation::ImageToVideo),
                        xai_video_tool_spec(MediaOperation::ReferenceToVideo),
                        xai_video_tool_spec(MediaOperation::ExtendVideo),
                    ],
                },
                ModelMediaCapabilities {
                    model_pattern: "xai-stt*".to_string(),
                    tools: vec![xai_stt_tool_spec()],
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
            NativeMediaRequest::EditImage(request) => self.edit_image(request).await,
            NativeMediaRequest::GenerateVideo(request) => self.generate_video(request).await,
            NativeMediaRequest::EditVideo(request) => self.edit_video(request).await,
            NativeMediaRequest::ImageToVideo(request) => self.image_to_video(request).await,
            NativeMediaRequest::ReferenceToVideo(request) => self.reference_to_video(request).await,
            NativeMediaRequest::ExtendVideo(request) => self.extend_video(request).await,
            NativeMediaRequest::TranscribeAudio(request) => self.transcribe_audio(request).await,
            NativeMediaRequest::GenerateSpeech(_) => {
                anyhow::bail!(
                    "xAI media operation {operation:?} is declared but not implemented in this pass"
                )
            }
        }
    }

    async fn poll_media_job(&self, job: &NativeMediaJob) -> anyhow::Result<NativeMediaResponse> {
        let api_key = self.api_key()?;
        let response = self
            .client
            .get(self.endpoint(format!("/videos/{}", job.job_id).as_str()))
            .header("Authorization", format!("Bearer {api_key}"))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(crate::api_error("xAI", response).await);
        }

        let polled: VideoPollResponse = response.json().await?;
        let status = xai_video_status(&polled.status)?;
        if status == NativeMediaJobStatus::Completed {
            let video = polled.video.ok_or_else(|| {
                anyhow::anyhow!("xAI video job {} completed without a video", job.job_id)
            })?;
            let metadata = video
                .duration
                .map(|duration| json!({ "duration_seconds": duration }));
            return Ok(NativeMediaResponse::Assets {
                assets: vec![MediaOutputAsset::Url {
                    url: video.url,
                    mime_type: Some("video/mp4".to_string()),
                }],
                metadata,
            });
        }

        let metadata = polled
            .error
            .and_then(|error| serde_json::to_value(error).ok());
        Ok(NativeMediaResponse::Job {
            job: NativeMediaJob {
                provider: job.provider.clone(),
                operation: job.operation,
                job_id: job.job_id.clone(),
                status,
                model: job.model.clone(),
                metadata,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_default_base_url() {
        let provider = XAiProvider::new(Some("xai-key"));
        assert_eq!(provider.base_url, XAI_DEFAULT_BASE_URL);
    }

    #[test]
    fn capabilities_include_xai_video_modes() {
        let provider = XAiProvider::new(None);
        let capabilities = MediaCapabilitiesProvider::media_capabilities(&provider);
        let video = capabilities
            .models
            .iter()
            .find(|model| model.model_pattern == "grok-imagine-video*")
            .expect("video capability");

        assert!(
            video
                .operations()
                .any(|op| op == MediaOperation::ImageToVideo)
        );
        assert!(
            video
                .operations()
                .any(|op| op == MediaOperation::ReferenceToVideo)
        );
        assert!(
            video
                .operations()
                .any(|op| op == MediaOperation::ExtendVideo)
        );
    }

    #[test]
    fn capabilities_include_all_xai_image_model_variants() {
        let provider = XAiProvider::new(None);
        let capabilities = MediaCapabilitiesProvider::media_capabilities(&provider);
        let image = capabilities
            .models
            .iter()
            .find(|model| model.model_pattern == "grok-imagine-image*")
            .expect("image capability");

        assert!(
            image
                .operations()
                .any(|op| op == MediaOperation::GenerateImage)
        );
        assert!(image.operations().any(|op| op == MediaOperation::EditImage));
    }

    #[test]
    fn capabilities_include_xai_stt_modes_without_reclassifying_voice_agents() {
        let provider = XAiProvider::new(None);
        let capabilities = MediaCapabilitiesProvider::media_capabilities(&provider);
        let stt = capabilities
            .models
            .iter()
            .find(|model| model.model_pattern == "xai-stt*")
            .expect("STT capability");

        assert!(
            stt.operations()
                .any(|op| op == MediaOperation::TranscribeAudio)
        );
        assert!(
            capabilities
                .models
                .iter()
                .all(|model| model.model_pattern != "grok-voice*")
        );
    }

    #[test]
    fn xai_stt_response_words_become_transcript_segments() {
        let response: XaiSttResponse = serde_json::from_value(json!({
            "text": "Hello world",
            "language": "English",
            "duration": 1.25,
            "words": [
                {"text": "Hello", "start": 0.0, "end": 0.5},
                {"text": "world", "start": 0.6, "end": 1.0, "speaker": 1}
            ]
        }))
        .expect("stt response should parse");

        let segments = xai_stt_segments(&response);

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "Hello");
        assert_eq!(segments[0].start_seconds, Some(0.0));
        assert_eq!(segments[1].text, "world");
        assert_eq!(
            segments[1].metadata.as_ref().unwrap()["speaker"],
            serde_json::json!(1)
        );
    }

    #[test]
    fn xai_audio_data_uri_decodes_supported_audio_mime_type() {
        let decoded = prepare_xai_audio_data_uri("data:audio/ogg;base64,YXVkaW8=")
            .expect("valid audio data uri");

        assert_eq!(decoded.mime_type, "audio/ogg");
        assert_eq!(decoded.bytes, b"audio");
        assert_eq!(decoded.filename, "audio.ogg");
    }

    #[test]
    fn xai_video_status_maps_to_native_status() {
        assert_eq!(
            xai_video_status("pending").expect("pending"),
            NativeMediaJobStatus::Running
        );
        assert_eq!(
            xai_video_status("done").expect("done"),
            NativeMediaJobStatus::Completed
        );
        assert_eq!(
            xai_video_status("expired").expect("expired"),
            NativeMediaJobStatus::Expired
        );
        assert_eq!(
            xai_video_status("failed").expect("failed"),
            NativeMediaJobStatus::Failed
        );
    }

    #[test]
    fn xai_video_poll_response_matches_rest_done_shape() {
        let response: VideoPollResponse = serde_json::from_value(json!({
            "status": "done",
            "video": {
                "url": "https://vidgen.x.ai/example/video.mp4",
                "duration": 8,
                "respect_moderation": true
            },
            "model": "grok-imagine-video"
        }))
        .expect("poll response should parse");

        assert_eq!(response.status, "done");
        let video = response.video.expect("video asset");
        assert_eq!(video.url, "https://vidgen.x.ai/example/video.mp4");
        assert_eq!(video.duration, Some(8.0));
    }

    #[test]
    fn xai_image_edit_input_uses_image_url_shape() {
        let input = xai_media_input(
            MediaInputAsset::Url {
                url: "https://example.com/image.png".to_string(),
            },
            true,
        );
        let value = serde_json::to_value(input).expect("serialize");

        assert_eq!(value["type"], "image_url");
        assert_eq!(value["url"], "https://example.com/image.png");
    }

    #[test]
    fn xai_responses_tools_include_native_and_local_tools() {
        let tools = native_responses_tools(
            &[
                NativeModelToolId::from("web_search"),
                NativeModelToolId::from("x_search"),
            ],
            Some(&[crate::ToolSpec {
                name: "shell".to_string(),
                description: "Run a shell command.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string" }
                    },
                    "required": ["cmd"]
                }),
                category: crate::ToolCategory::Write,
            }]),
        )
        .expect("supported tools");

        assert_eq!(tools[0].kind, "web_search");
        assert_eq!(tools[1].kind, "x_search");
        assert_eq!(tools[2].kind, "function");
        assert_eq!(tools[2].name.as_deref(), Some("shell"));
    }

    #[test]
    fn xai_responses_tools_reject_unknown_native_tool_ids() {
        let error = native_responses_tools(&[NativeModelToolId::from("unknown_tool")], None)
            .expect_err("unsupported tool should fail");

        assert!(error.to_string().contains("unknown_tool"));
    }

    #[test]
    fn xai_responses_extracts_function_calls() {
        let response: ResponsesResponse = serde_json::from_value(json!({
            "output": [
                {
                    "type": "function_call",
                    "call_id": "call_123",
                    "name": "shell",
                    "arguments": "{\"cmd\":\"date\"}"
                }
            ],
            "usage": {
                "input_tokens": 5,
                "output_tokens": 3
            }
        }))
        .expect("responses payload should parse");

        let calls = responses_tool_calls(&response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_123");
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, "{\"cmd\":\"date\"}");
    }

    #[test]
    fn xai_responses_extracts_provider_native_tool_traces() {
        let response: ResponsesResponse = serde_json::from_value(json!({
            "output": [
                {
                    "id": "ws_123",
                    "type": "web_search_call",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "latest xAI models"
                    },
                    "results": [
                        { "title": "xAI Docs", "url": "https://docs.x.ai/developers/models" }
                    ]
                },
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "xAI has new models.",
                            "annotations": [
                                { "type": "url_citation", "url": "https://docs.x.ai/developers/models" }
                            ]
                        }
                    ]
                }
            ]
        }))
        .expect("responses payload should parse");

        let traces = responses_provider_tool_traces(&response);
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].id, "ws_123");
        assert_eq!(traces[0].name, "web_search");
        assert_eq!(traces[0].provider, "xai");
        assert_eq!(traces[0].input["status"], "completed");
        assert!(traces[0].output.is_some());
        assert_eq!(traces[0].citations.len(), 1);
    }

    #[test]
    fn xai_stream_parser_emits_provider_tool_start_and_completion() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = ResponsesStreamState::default();

        handle_responses_stream_value(
            json!({
                "type": "response.output_item.added",
                "item": {
                    "id": "ws_123",
                    "type": "web_search_call",
                    "status": "in_progress",
                    "action": {
                        "type": "search",
                        "query": "latest xAI models"
                    }
                }
            }),
            &mut state,
            &tx,
        );
        handle_responses_stream_value(
            json!({
                "type": "response.output_item.done",
                "item": {
                    "id": "ws_123",
                    "type": "web_search_call",
                    "status": "completed",
                    "results": [
                        { "title": "xAI Docs", "url": "https://docs.x.ai/developers/models" }
                    ]
                }
            }),
            &mut state,
            &tx,
        );

        match rx.try_recv().expect("start event") {
            ProviderStreamEvent::ProviderToolStarted(trace) => {
                assert_eq!(trace.id, "ws_123");
                assert_eq!(trace.name, "web_search");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        match rx.try_recv().expect("completion event") {
            ProviderStreamEvent::ProviderToolCompleted(trace) => {
                assert_eq!(trace.id, "ws_123");
                assert_eq!(trace.name, "web_search");
                assert!(!trace.citations.is_empty());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn xai_stream_parser_tolerates_raw_provider_tool_events() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = ResponsesStreamState::default();

        handle_responses_stream_value(
            json!({
                "type": "response.web_search_call.in_progress",
                "item_id": "ws_raw_123",
                "query": "current events"
            }),
            &mut state,
            &tx,
        );

        match rx.try_recv().expect("start event") {
            ProviderStreamEvent::ProviderToolStarted(trace) => {
                assert_eq!(trace.id, "ws_raw_123");
                assert_eq!(trace.name, "web_search");
                assert_eq!(trace.input["query"], "current events");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
