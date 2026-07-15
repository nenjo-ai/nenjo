//! Provider media and provider-native model tool contracts.
//!
//! These APIs are separate from chat/tool calling. They represent direct
//! provider endpoints such as image generation, video jobs, TTS, and STT.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Stable Nenjo-facing identifier for a provider-native model tool.
///
/// This is intentionally open-ended. Providers publish the concrete tool
/// contracts they support, while model configuration stores the enabled IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct NativeModelToolId(String);

impl NativeModelToolId {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("native model tool id cannot be empty".to_string());
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<NativeModelToolId> for String {
    fn from(value: NativeModelToolId) -> Self {
        value.0
    }
}

impl std::fmt::Display for NativeModelToolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for NativeModelToolId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for NativeModelToolId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl From<&str> for NativeModelToolId {
    fn from(value: &str) -> Self {
        Self::new(value).expect("static native model tool id should be valid")
    }
}

/// Provider-published model-native tool contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderNativeModelToolSpec {
    /// Stable Nenjo-facing id used in model configuration.
    pub id: NativeModelToolId,
    /// Exact provider API tool type.
    pub provider_type: String,
    /// Model-visible tool name.
    pub name: String,
    /// Human-readable description for the tool belt and UI.
    pub description: String,
    /// Optional model-visible parameter schema when the provider supports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters_schema: Option<serde_json::Value>,
    /// Optional provider/tool configuration schema for dashboard controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
}

/// A media operation exposed outside the chat turn loop.
///
/// Most variants are configurable through non-`chat` model assignments. Some
/// variants may remain reserved for provider integrations that are not yet
/// assignable by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOperation {
    GenerateImage,
    EditImage,
    GenerateVideo,
    EditVideo,
    ImageToVideo,
    ReferenceToVideo,
    ExtendVideo,
    GenerateSpeech,
    TranscribeAudio,
    RealtimeVoiceAgent,
}

impl MediaOperation {
    /// Every media operation variant, in stable declaration order.
    pub const ALL: &'static [Self] = &[
        Self::GenerateImage,
        Self::EditImage,
        Self::GenerateVideo,
        Self::EditVideo,
        Self::ImageToVideo,
        Self::ReferenceToVideo,
        Self::ExtendVideo,
        Self::GenerateSpeech,
        Self::TranscribeAudio,
        Self::RealtimeVoiceAgent,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GenerateImage => "generate_image",
            Self::EditImage => "edit_image",
            Self::GenerateVideo => "generate_video",
            Self::EditVideo => "edit_video",
            Self::ImageToVideo => "image_to_video",
            Self::ReferenceToVideo => "reference_to_video",
            Self::ExtendVideo => "extend_video",
            Self::GenerateSpeech => "generate_speech",
            Self::TranscribeAudio => "transcribe_audio",
            Self::RealtimeVoiceAgent => "realtime_voice_agent",
        }
    }

    pub fn tool_name(self) -> Option<&'static str> {
        match self {
            Self::RealtimeVoiceAgent => None,
            operation => Some(operation.as_str()),
        }
    }
}

impl std::fmt::Display for MediaOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MediaOperation {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "generate_image" => Ok(Self::GenerateImage),
            "edit_image" => Ok(Self::EditImage),
            "generate_video" => Ok(Self::GenerateVideo),
            "edit_video" => Ok(Self::EditVideo),
            "image_to_video" => Ok(Self::ImageToVideo),
            "reference_to_video" => Ok(Self::ReferenceToVideo),
            "extend_video" => Ok(Self::ExtendVideo),
            "generate_speech" => Ok(Self::GenerateSpeech),
            "transcribe_audio" => Ok(Self::TranscribeAudio),
            "realtime_voice_agent" => Ok(Self::RealtimeVoiceAgent),
            other => Err(format!("unknown media operation '{other}'")),
        }
    }
}

/// Supported output representation for provider-generated media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MediaOutputFormat {
    #[default]
    Url,
    Base64,
}

/// A media input asset passed to a provider media operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaInputAsset {
    Url { url: String },
    DataUri { data_uri: String },
    ProviderFileId { file_id: String },
}

/// A media asset returned by a provider media operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaOutputAsset {
    Url {
        url: String,
        mime_type: Option<String>,
    },
    Base64 {
        data: String,
        mime_type: Option<String>,
    },
    ProviderFileId {
        file_id: String,
        mime_type: Option<String>,
    },
}

/// Request for image generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateImageRequest {
    pub model: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default)]
    pub output_format: MediaOutputFormat,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for image editing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditImageRequest {
    pub model: String,
    pub prompt: String,
    pub image: MediaInputAsset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default)]
    pub output_format: MediaOutputFormat,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for text-to-video generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateVideoRequest {
    pub model: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for video editing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditVideoRequest {
    pub model: String,
    pub prompt: String,
    pub video: MediaInputAsset,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for image-to-video generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageToVideoRequest {
    pub model: String,
    pub prompt: String,
    pub image: MediaInputAsset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for reference-guided video generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceToVideoRequest {
    pub model: String,
    pub prompt: String,
    pub reference_images: Vec<MediaInputAsset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for extending an existing video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtendVideoRequest {
    pub model: String,
    pub prompt: String,
    pub video: MediaInputAsset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for text-to-speech generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateSpeechRequest {
    pub model: String,
    pub text: String,
    pub voice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// Request for speech-to-text transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeAudioRequest {
    pub model: String,
    pub audio: MediaInputAsset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Optional provider-compatible guidance for the transcription.
    ///
    /// OpenAI-style transcription endpoints use this as decoding context,
    /// while conversational audio models may treat it as an instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// A direct provider media request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum NativeMediaRequest {
    GenerateImage(GenerateImageRequest),
    EditImage(EditImageRequest),
    GenerateVideo(GenerateVideoRequest),
    EditVideo(EditVideoRequest),
    ImageToVideo(ImageToVideoRequest),
    ReferenceToVideo(ReferenceToVideoRequest),
    ExtendVideo(ExtendVideoRequest),
    GenerateSpeech(GenerateSpeechRequest),
    TranscribeAudio(TranscribeAudioRequest),
}

impl NativeMediaRequest {
    pub fn operation(&self) -> MediaOperation {
        match self {
            Self::GenerateImage(_) => MediaOperation::GenerateImage,
            Self::EditImage(_) => MediaOperation::EditImage,
            Self::GenerateVideo(_) => MediaOperation::GenerateVideo,
            Self::EditVideo(_) => MediaOperation::EditVideo,
            Self::ImageToVideo(_) => MediaOperation::ImageToVideo,
            Self::ReferenceToVideo(_) => MediaOperation::ReferenceToVideo,
            Self::ExtendVideo(_) => MediaOperation::ExtendVideo,
            Self::GenerateSpeech(_) => MediaOperation::GenerateSpeech,
            Self::TranscribeAudio(_) => MediaOperation::TranscribeAudio,
        }
    }
}

/// Provider async job state for media operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeMediaJobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

/// A submitted provider media job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeMediaJob {
    pub provider: String,
    pub operation: MediaOperation,
    pub job_id: String,
    pub status: NativeMediaJobStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Result from a provider media operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeMediaResponse {
    Assets {
        assets: Vec<MediaOutputAsset>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    Transcript {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_seconds: Option<f64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        segments: Vec<TranscriptSegment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    Job {
        job: NativeMediaJob,
    },
}

/// Timestamped transcript segment returned by a speech-to-text provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_seconds: Option<f64>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Media capability metadata for a model or model family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMediaCapabilities {
    pub model_pattern: String,
    pub tools: Vec<MediaToolSpec>,
}

impl ModelMediaCapabilities {
    pub fn operations(&self) -> impl Iterator<Item = MediaOperation> + '_ {
        self.tools.iter().map(|tool| tool.capability)
    }
}

/// How a provider media operation completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MediaExecutionMode {
    Immediate,
    AsyncJob { poll_supported: bool },
}

/// Complete model-visible contract for a provider media operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaToolSpec {
    pub capability: MediaOperation,
    pub tool_name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
    pub execution: MediaExecutionMode,
}

/// Provider media capability metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMediaCapabilities {
    pub provider: String,
    #[serde(default)]
    pub model_tools: Vec<ProviderNativeModelToolSpec>,
    pub models: Vec<ModelMediaCapabilities>,
}

/// Provider support for direct media/model-specific endpoints.
#[async_trait]
pub trait MediaCapabilitiesProvider: Send + Sync {
    fn media_capabilities(&self) -> ProviderMediaCapabilities;

    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse>;

    async fn poll_media_job(&self, job: &NativeMediaJob) -> anyhow::Result<NativeMediaResponse> {
        let _ = job;
        anyhow::bail!("provider does not support polling media jobs")
    }
}

pub(crate) fn media_input_schema() -> serde_json::Value {
    serde_json::json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "type": {"const": "url"},
                    "url": {"type": "string"}
                },
                "required": ["type", "url"]
            },
            {
                "type": "object",
                "properties": {
                    "type": {"const": "data_uri"},
                    "data_uri": {"type": "string"}
                },
                "required": ["type", "data_uri"]
            },
            {
                "type": "object",
                "properties": {
                    "type": {"const": "provider_file_id"},
                    "file_id": {"type": "string"}
                },
                "required": ["type", "file_id"]
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_request_reports_operation() {
        let request = NativeMediaRequest::GenerateImage(GenerateImageRequest {
            model: "example-image-model".to_string(),
            prompt: "draw a diagram".to_string(),
            n: None,
            size: None,
            aspect_ratio: None,
            resolution: None,
            output_format: MediaOutputFormat::Url,
            provider_options: serde_json::Value::Null,
        });

        assert_eq!(request.operation(), MediaOperation::GenerateImage);
    }

    #[test]
    fn transcription_prompt_is_an_explicit_request_field() {
        let request = TranscribeAudioRequest {
            model: "transcription-model".to_string(),
            audio: MediaInputAsset::Url {
                url: "https://example.test/audio.webm".to_string(),
            },
            language: Some("en".to_string()),
            prompt: Some("Use the product name Nenjo when it is spoken.".to_string()),
            provider_options: serde_json::Value::Null,
        };

        let value = serde_json::to_value(request).unwrap();
        assert_eq!(
            value["prompt"],
            "Use the product name Nenjo when it is spoken."
        );
    }

    #[test]
    fn native_model_tool_id_serializes_as_valid_string() {
        let id: NativeModelToolId =
            serde_json::from_value(serde_json::json!(" provider_search ")).expect("valid id");

        assert_eq!(id.as_str(), "provider_search");
        assert_eq!(
            serde_json::to_value(&id).unwrap(),
            serde_json::json!("provider_search")
        );
        assert!(serde_json::from_value::<NativeModelToolId>(serde_json::json!("")).is_err());
    }

    #[test]
    fn media_operation_wire_values_are_stable() {
        for operation in MediaOperation::ALL {
            let parsed: MediaOperation = operation.as_str().parse().expect("parse");
            assert_eq!(parsed, *operation);
            assert_eq!(parsed.to_string(), operation.as_str());
            assert_eq!(serde_json::to_value(operation).unwrap(), operation.as_str());
            assert_eq!(
                serde_json::from_value::<MediaOperation>(serde_json::json!(operation.as_str()))
                    .unwrap(),
                *operation
            );
        }
        assert!("chat".parse::<MediaOperation>().is_err());
        assert!("make_poster".parse::<MediaOperation>().is_err());
    }
}
