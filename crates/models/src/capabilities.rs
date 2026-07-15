//! Model capability metadata.
//!
//! These types describe what a configured model can do independently of the
//! provider-native tool list used inside chat calls.
//!
//! # Capability taxonomy
//!
//! - **Assignable operation capabilities** — operations the platform can route
//!   to a model (`chat`, `generate_image`, …). Non-`chat` ops may appear in
//!   package `model_requirements` and resource model assignments.
//! - **Feature capabilities** — catalog/filter metadata only (`tool_calling`,
//!   `reasoning`, …). Never used in assignments or package requirements.
//! - **Modalities** — independent input/output metadata (`text`, `image`,
//!   `file`, …). Modalities describe the data a model accepts or returns; they
//!   are not model capabilities.

use serde::{Deserialize, Serialize};

use crate::native::MediaOperation;

// ---------------------------------------------------------------------------
// Known capability ID constants
// ---------------------------------------------------------------------------

/// A stable, executable Nenjo-facing model operation.
///
/// This closed enum is the canonical capability vocabulary for configured
/// models, package requirements, and resource assignments. It serializes as
/// its existing snake-case wire value, so persisted model configuration and
/// API payloads retain their shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapabilityId {
    Chat,
    TranscribeAudio,
    GenerateSpeech,
    GenerateImage,
    EditImage,
    GenerateVideo,
    EditVideo,
    ImageToVideo,
    ReferenceToVideo,
    ExtendVideo,
}

impl ModelCapabilityId {
    /// Every assignable operation, in stable display and serialization order.
    pub const ALL: &'static [Self] = &[
        Self::Chat,
        Self::TranscribeAudio,
        Self::GenerateSpeech,
        Self::GenerateImage,
        Self::EditImage,
        Self::GenerateVideo,
        Self::EditVideo,
        Self::ImageToVideo,
        Self::ReferenceToVideo,
        Self::ExtendVideo,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::TranscribeAudio => "transcribe_audio",
            Self::GenerateSpeech => "generate_speech",
            Self::GenerateImage => "generate_image",
            Self::EditImage => "edit_image",
            Self::GenerateVideo => "generate_video",
            Self::EditVideo => "edit_video",
            Self::ImageToVideo => "image_to_video",
            Self::ReferenceToVideo => "reference_to_video",
            Self::ExtendVideo => "extend_video",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::TranscribeAudio => "Transcribe audio",
            Self::GenerateSpeech => "Generate speech",
            Self::GenerateImage => "Generate image",
            Self::EditImage => "Edit image",
            Self::GenerateVideo => "Generate video",
            Self::EditVideo => "Edit video",
            Self::ImageToVideo => "Image to video",
            Self::ReferenceToVideo => "Reference to video",
            Self::ExtendVideo => "Extend video",
        }
    }
}

impl std::fmt::Display for ModelCapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ModelCapabilityId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "chat" => Ok(Self::Chat),
            "transcribe_audio" => Ok(Self::TranscribeAudio),
            "generate_speech" => Ok(Self::GenerateSpeech),
            "generate_image" => Ok(Self::GenerateImage),
            "edit_image" => Ok(Self::EditImage),
            "generate_video" => Ok(Self::GenerateVideo),
            "edit_video" => Ok(Self::EditVideo),
            "image_to_video" => Ok(Self::ImageToVideo),
            "reference_to_video" => Ok(Self::ReferenceToVideo),
            "extend_video" => Ok(Self::ExtendVideo),
            "" => Err("model capability id cannot be empty".to_string()),
            other => Err(format!("unknown model capability '{other}'")),
        }
    }
}

impl<'de> Deserialize<'de> for ModelCapabilityId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl From<ModelCapabilityId> for String {
    fn from(value: ModelCapabilityId) -> Self {
        value.as_str().to_owned()
    }
}

impl TryFrom<MediaOperation> for ModelCapabilityId {
    type Error = String;

    fn try_from(value: MediaOperation) -> Result<Self, Self::Error> {
        value.as_str().parse()
    }
}

// ---------------------------------------------------------------------------
// Canonical capability sets
// ---------------------------------------------------------------------------

/// Full assignable operation set (including `chat`).
///
/// This is an alias for [`ModelCapabilityId::ALL`] so all operation consumers
/// share one closed vocabulary. Package requirements and resource assignments
/// apply their `chat` restriction as contextual validation, not a second set.
pub const ASSIGNABLE_OPERATION_CAPABILITIES: &[ModelCapabilityId] = ModelCapabilityId::ALL;

/// Return whether `cap` is an assignable operation capability (including `chat`).
pub fn is_assignable_operation_capability(cap: &str) -> bool {
    cap.parse::<ModelCapabilityId>().is_ok()
}

/// Return whether `cap` is a known assignable operation capability.
pub fn is_known_capability(cap: &str) -> bool {
    is_assignable_operation_capability(cap)
}

/// Validate that every capability string is a known assignable operation ID.
///
/// Unknown strings are rejected. Empty slices are accepted.
pub fn validate_model_capabilities(caps: &[String]) -> Result<(), String> {
    for cap in caps {
        let trimmed = cap.trim();
        if trimmed.is_empty() {
            return Err("model capability cannot be empty".to_string());
        }
        trimmed.parse::<ModelCapabilityId>()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Modality hints for assignable operations
// ---------------------------------------------------------------------------

/// Implied input/output modalities for an assignable operation capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityModalityHints {
    /// Modalities the operation consumes.
    pub inputs: &'static [ModelModality],
    /// Modalities the operation produces.
    pub outputs: &'static [ModelModality],
}

/// Return modality hints for an assignable operation capability ID.
///
/// Returns `None` for unknown or feature-only capability IDs.
pub fn assignable_operation_modality_hints(cap: &str) -> Option<CapabilityModalityHints> {
    let operation = cap.parse::<ModelCapabilityId>().ok()?;
    Some(match operation {
        ModelCapabilityId::Chat => CapabilityModalityHints {
            inputs: &[ModelModality::Text],
            outputs: &[ModelModality::Text],
        },
        ModelCapabilityId::TranscribeAudio => CapabilityModalityHints {
            inputs: &[ModelModality::Audio],
            outputs: &[ModelModality::Text],
        },
        ModelCapabilityId::GenerateSpeech => CapabilityModalityHints {
            inputs: &[ModelModality::Text],
            outputs: &[ModelModality::Audio],
        },
        ModelCapabilityId::GenerateImage => CapabilityModalityHints {
            inputs: &[ModelModality::Text],
            outputs: &[ModelModality::Image],
        },
        ModelCapabilityId::EditImage => CapabilityModalityHints {
            inputs: &[ModelModality::Text, ModelModality::Image],
            outputs: &[ModelModality::Image],
        },
        ModelCapabilityId::GenerateVideo => CapabilityModalityHints {
            inputs: &[ModelModality::Text],
            outputs: &[ModelModality::Video],
        },
        ModelCapabilityId::EditVideo => CapabilityModalityHints {
            inputs: &[ModelModality::Text, ModelModality::Video],
            outputs: &[ModelModality::Video],
        },
        ModelCapabilityId::ImageToVideo | ModelCapabilityId::ReferenceToVideo => {
            CapabilityModalityHints {
                inputs: &[ModelModality::Text, ModelModality::Image],
                outputs: &[ModelModality::Video],
            }
        }
        ModelCapabilityId::ExtendVideo => CapabilityModalityHints {
            inputs: &[ModelModality::Text, ModelModality::Video],
            outputs: &[ModelModality::Video],
        },
    })
}

// ---------------------------------------------------------------------------
// Modalities and execution modes
// ---------------------------------------------------------------------------

/// Input or output modality supported by a configured model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelModality {
    Text,
    Audio,
    Image,
    File,
    Video,
}

impl ModelModality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Audio => "audio",
            Self::Image => "image",
            Self::File => "file",
            Self::Video => "video",
        }
    }
}

impl std::fmt::Display for ModelModality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ModelModality {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "text" => Ok(Self::Text),
            "audio" => Ok(Self::Audio),
            "image" => Ok(Self::Image),
            "file" => Ok(Self::File),
            "video" => Ok(Self::Video),
            other => Err(format!("unknown model modality '{other}'")),
        }
    }
}

/// Execution style supported by a model capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelExecutionMode {
    Immediate,
    AsyncJob,
    RealtimeSession,
}

impl ModelExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::AsyncJob => "async_job",
            Self::RealtimeSession => "realtime_session",
        }
    }
}

impl std::fmt::Display for ModelExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ModelExecutionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "immediate" => Ok(Self::Immediate),
            "async_job" => Ok(Self::AsyncJob),
            "realtime_session" => Ok(Self::RealtimeSession),
            other => Err(format!("unknown model execution mode '{other}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_id_trims_and_serializes() {
        let capability: ModelCapabilityId =
            serde_json::from_value(serde_json::json!(" transcribe_audio "))
                .expect("valid capability");

        assert_eq!(capability, ModelCapabilityId::TranscribeAudio);
        assert_eq!(
            serde_json::to_value(capability).unwrap(),
            serde_json::json!("transcribe_audio")
        );
    }

    #[test]
    fn modality_and_execution_modes_use_snake_case() {
        assert_eq!(
            serde_json::to_value(ModelModality::Audio).unwrap(),
            serde_json::json!("audio")
        );
        assert_eq!(
            "file".parse::<ModelModality>().unwrap(),
            ModelModality::File
        );
        assert_eq!(
            serde_json::to_value(ModelExecutionMode::RealtimeSession).unwrap(),
            serde_json::json!("realtime_session")
        );
    }

    #[test]
    fn native_operations_convert_to_capability_ids() {
        let capability = ModelCapabilityId::try_from(MediaOperation::GenerateSpeech).unwrap();

        assert_eq!(capability.as_str(), "generate_speech");
        assert!(ModelCapabilityId::try_from(MediaOperation::RealtimeVoiceAgent).is_err());
    }

    #[test]
    fn assignable_operation_predicate_uses_the_canonical_enum() {
        assert!(is_assignable_operation_capability("chat"));
        assert!(is_assignable_operation_capability("generate_image"));
        assert!(!is_assignable_operation_capability("realtime_voice_agent"));
    }

    #[test]
    fn modality_and_catalog_feature_ids_are_not_capabilities() {
        assert!(!is_known_capability("vision_input"));
        assert!(!is_known_capability("file_input"));
        assert!(!is_known_capability("chat_stream"));
        assert!(!is_known_capability("tool_calling"));
        assert!(!is_known_capability("structured_outputs"));
        assert!(!is_known_capability("reasoning"));
        assert!(!is_known_capability("realtime_voice_agent"));
    }

    #[test]
    fn known_capability_covers_assignable_operations_only() {
        assert!(is_known_capability(ModelCapabilityId::Chat.as_str()));
        assert!(is_known_capability(
            ModelCapabilityId::GenerateImage.as_str()
        ));
        assert!(!is_known_capability("make_poster"));
    }

    #[test]
    fn validate_model_capabilities_rejects_unknown() {
        assert!(validate_model_capabilities(&[]).is_ok());
        assert!(
            validate_model_capabilities(&[
                ModelCapabilityId::Chat.to_string(),
                ModelCapabilityId::GenerateImage.to_string(),
            ])
            .is_ok()
        );
        let err = validate_model_capabilities(&["make_poster".to_string()]).unwrap_err();
        assert!(err.contains("unknown model capability"));
    }

    #[test]
    fn modality_hints_cover_all_assignable_ops() {
        for cap in ASSIGNABLE_OPERATION_CAPABILITIES {
            assert!(
                assignable_operation_modality_hints(cap.as_str()).is_some(),
                "missing modality hints for {cap}"
            );
        }
        assert!(assignable_operation_modality_hints("tool_calling").is_none());
    }

    #[test]
    fn canonical_operation_set_is_chat_plus_executable_native_operations() {
        assert_eq!(ASSIGNABLE_OPERATION_CAPABILITIES, ModelCapabilityId::ALL);
        for operation in ASSIGNABLE_OPERATION_CAPABILITIES {
            if *operation == ModelCapabilityId::Chat {
                continue;
            }
            assert!(
                MediaOperation::ALL
                    .iter()
                    .any(|native| native.as_str() == operation.as_str()),
                "{} must have a worker-native operation",
                operation
            );
        }
    }
}
