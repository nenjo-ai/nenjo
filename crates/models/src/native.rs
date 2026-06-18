//! Provider-native media and model capability contracts.
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

/// A provider-native operation exposed outside the chat turn loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOperation {
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

impl NativeOperation {
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

/// Supported output representation for provider-generated media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MediaOutputFormat {
    #[default]
    Url,
    Base64,
}

/// A media input asset passed to a provider-native operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaInputAsset {
    Url { url: String },
    DataUri { data_uri: String },
    ProviderFileId { file_id: String },
}

/// A media asset returned by a provider-native operation.
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
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

/// A provider-native media request.
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
    pub fn operation(&self) -> NativeOperation {
        match self {
            Self::GenerateImage(_) => NativeOperation::GenerateImage,
            Self::EditImage(_) => NativeOperation::EditImage,
            Self::GenerateVideo(_) => NativeOperation::GenerateVideo,
            Self::EditVideo(_) => NativeOperation::EditVideo,
            Self::ImageToVideo(_) => NativeOperation::ImageToVideo,
            Self::ReferenceToVideo(_) => NativeOperation::ReferenceToVideo,
            Self::ExtendVideo(_) => NativeOperation::ExtendVideo,
            Self::GenerateSpeech(_) => NativeOperation::GenerateSpeech,
            Self::TranscribeAudio(_) => NativeOperation::TranscribeAudio,
        }
    }
}

/// Provider async job state for native media operations.
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

/// A submitted provider-native media job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeMediaJob {
    pub provider: String,
    pub operation: NativeOperation,
    pub job_id: String,
    pub status: NativeMediaJobStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Result from a provider-native media operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeMediaResponse {
    Assets {
        assets: Vec<MediaOutputAsset>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    Job {
        job: NativeMediaJob,
    },
}

/// Capability metadata for a model or model family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelNativeCapabilities {
    pub model_pattern: String,
    pub tools: Vec<NativeToolSpec>,
}

impl ModelNativeCapabilities {
    pub fn operations(&self) -> impl Iterator<Item = NativeOperation> + '_ {
        self.tools.iter().map(|tool| tool.capability)
    }
}

/// How a provider-native tool completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NativeExecutionMode {
    Immediate,
    AsyncJob { poll_supported: bool },
}

/// Complete model-visible tool contract for a provider-native capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeToolSpec {
    pub capability: NativeOperation,
    pub tool_name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
    pub execution: NativeExecutionMode,
}

/// Provider-native capability metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderNativeCapabilities {
    pub provider: String,
    #[serde(default)]
    pub model_tools: Vec<ProviderNativeModelToolSpec>,
    pub models: Vec<ModelNativeCapabilities>,
}

/// Provider support for direct media/model-specific endpoints.
#[async_trait]
pub trait NativeCapabilitiesProvider: Send + Sync {
    fn native_capabilities(&self) -> ProviderNativeCapabilities;

    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse>;

    async fn poll_media_job(&self, job: &NativeMediaJob) -> anyhow::Result<NativeMediaResponse> {
        let _ = job;
        anyhow::bail!("provider does not support polling native media jobs")
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

        assert_eq!(request.operation(), NativeOperation::GenerateImage);
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
}
