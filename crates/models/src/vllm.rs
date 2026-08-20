//! vLLM provider built on the shared OpenAI-compatible HTTP transport.
//!
//! vLLM intentionally implements only a subset of OpenAI content parts. This
//! adapter keeps that wire contract separate so OpenAI document `file` parts
//! cannot leak into a vLLM request.

use async_trait::async_trait;

use crate::compatible::{AuthStyle, OpenAiCompatibleProvider};
use crate::openai_multimodal::{ChatArtifactDialect, chat_artifact_transport};
use crate::{
    ArtifactInputTransport, ChatRequest, ChatResponse, MediaType, ModelCapabilityId, ModelProvider,
    NativeMediaJob, NativeMediaRequest, NativeMediaResponse, ProviderMediaCapabilities,
    ProviderStreamEvent,
};

pub const VLLM_DEFAULT_BASE_URL: &str = "http://localhost:8000/v1";

/// Response delivery mode requested from the vLLM Chat Completions API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VllmStreaming {
    Enabled,
    Disabled,
}

impl From<bool> for VllmStreaming {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// A first-class vLLM endpoint with vLLM-specific content-part semantics.
pub struct VllmProvider {
    compatible: OpenAiCompatibleProvider,
    streaming: VllmStreaming,
}

impl VllmProvider {
    pub fn new(base_url: Option<&str>, api_key: Option<&str>) -> Self {
        Self::with_streaming(base_url, api_key, VllmStreaming::Enabled)
    }

    pub fn with_streaming(
        base_url: Option<&str>,
        api_key: Option<&str>,
        streaming: VllmStreaming,
    ) -> Self {
        Self {
            compatible: OpenAiCompatibleProvider::new_with_dialect(
                "vllm",
                base_url.unwrap_or(VLLM_DEFAULT_BASE_URL),
                Some(api_key.unwrap_or_default()),
                AuthStyle::Bearer,
                ChatArtifactDialect::Vllm,
            ),
            streaming,
        }
    }
}

#[async_trait]
impl ModelProvider for VllmProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        self.compatible.chat(request, model, temperature).await
    }

    async fn chat_stream(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        events: tokio::sync::mpsc::UnboundedSender<ProviderStreamEvent>,
    ) -> anyhow::Result<ChatResponse> {
        match self.streaming {
            VllmStreaming::Enabled => {
                self.compatible
                    .chat_stream(request, model, temperature, events)
                    .await
            }
            VllmStreaming::Disabled => self.compatible.chat(request, model, temperature).await,
        }
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        self.compatible.context_window(model)
    }

    fn supports_native_tools(&self) -> bool {
        self.compatible.supports_native_tools()
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        self.compatible.supports_developer_role(model)
    }

    fn artifact_input_transport(
        &self,
        model: &str,
        capability: ModelCapabilityId,
        media_type: &MediaType,
    ) -> ArtifactInputTransport {
        match capability {
            ModelCapabilityId::Chat
            | ModelCapabilityId::AnalyzeImage
            | ModelCapabilityId::AnalyzeVideo
            | ModelCapabilityId::AnalyzeDocument => {
                chat_artifact_transport(ChatArtifactDialect::Vllm, media_type.essence_str())
            }
            ModelCapabilityId::TranscribeAudio => self.compatible.artifact_input_transport(
                model,
                ModelCapabilityId::TranscribeAudio,
                media_type,
            ),
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

    fn media_capabilities(&self) -> Option<ProviderMediaCapabilities> {
        self.compatible.media_capabilities()
    }

    async fn submit_media(
        &self,
        request: NativeMediaRequest,
    ) -> anyhow::Result<NativeMediaResponse> {
        self.compatible.submit_media(request).await
    }

    async fn poll_media_job(&self, job: &NativeMediaJob) -> anyhow::Result<NativeMediaResponse> {
        self.compatible.poll_media_job(job).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nenjo_tool_api::{ArtifactId, ArtifactRef, ArtifactSize, MediaType, Sha256Digest};
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::*;
    use crate::{
        ArtifactInput, ArtifactInputSource, ChatMessage, ConversationMessage, PreparedArtifact,
        PreparedArtifactInputs,
    };

    #[test]
    fn boolean_streaming_config_maps_to_named_modes() {
        assert_eq!(VllmStreaming::from(true), VllmStreaming::Enabled);
        assert_eq!(VllmStreaming::from(false), VllmStreaming::Disabled);
    }

    fn prepared_artifact(
        media_type: &str,
        bytes: &'static [u8],
    ) -> (ArtifactRef, PreparedArtifactInputs) {
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

    #[test]
    fn vllm_transport_supports_text_and_media_but_not_document_file_parts() {
        let provider = VllmProvider::new(None, None);

        for media_type in ["text/markdown", "image/png", "audio/wav", "video/mp4"] {
            assert_ne!(
                provider.artifact_input_transport(
                    "model",
                    ModelCapabilityId::Chat,
                    &MediaType::parse(media_type).unwrap(),
                ),
                ArtifactInputTransport::Unsupported,
                "{media_type} should have a vLLM transport"
            );
        }

        for media_type in [
            "application/pdf",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ] {
            assert_eq!(
                provider.artifact_input_transport(
                    "model",
                    ModelCapabilityId::Chat,
                    &MediaType::parse(media_type).unwrap(),
                ),
                ArtifactInputTransport::Unsupported,
                "{media_type} must be extracted or analyzed before vLLM dispatch"
            );
        }
    }

    #[tokio::test]
    async fn vllm_rejects_pdf_before_making_an_http_request() {
        let provider = VllmProvider::new(Some("http://127.0.0.1:9/v1"), None);
        let (reference, prepared) = prepared_artifact("application/pdf", b"pdf");
        let messages = [ConversationMessage::chat(
            ChatMessage::user("Read this document").with_artifacts(vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )]),
        )];

        let error = provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    native_tools: None,
                    prepared_artifacts: Some(&prepared),
                },
                "text-model",
                0.0,
            )
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported Chat Completions media type")
        );
        assert!(error.to_string().contains("application/pdf"));
    }
}
