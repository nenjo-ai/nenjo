//! Deterministic direct-versus-analysis routing for immutable artifact inputs.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nenjo::Slug;
use nenjo::manifest::ModelManifest;
use nenjo_models::{
    ArtifactAnalysisMessage, ArtifactAnalyzerProvenance, ArtifactId, ArtifactInput,
    ArtifactInputTransport, ArtifactRef, MediaType, ModelCapabilityId, ModelModality, TokenUsage,
};
use thiserror::Error;

use super::{ModelAssignmentResolveError, ResolvedModelEndpoint};

/// Primary configured chat model facts needed for artifact input routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryArtifactModel {
    pub slug: Slug,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub input_modalities: Vec<ModelModality>,
}

impl From<&ModelManifest> for PrimaryArtifactModel {
    fn from(manifest: &ModelManifest) -> Self {
        Self {
            slug: manifest.slug.clone(),
            provider: manifest.model_provider.clone(),
            model: manifest.model.clone(),
            base_url: manifest.base_url.clone(),
            input_modalities: manifest.input_modalities.clone(),
        }
    }
}

/// Provider/model/capability target used for a transport query.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactTransportTarget<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub base_url: Option<&'a str>,
    pub capability: ModelCapabilityId,
}

/// One artifact routed to the primary model's provider transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectArtifactInput {
    pub input: ArtifactInput,
    pub transport: ArtifactInputTransport,
}

/// Explicit auxiliary model selected to analyze an artifact batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedArtifactAnalyzer {
    pub endpoint: ResolvedModelEndpoint,
    pub inputs: Vec<ArtifactInput>,
}

/// Verified plaintext made available only for one auxiliary analysis call.
#[derive(Clone)]
pub struct MaterializedAnalysisInput {
    input: ArtifactInput,
    bytes: Arc<[u8]>,
}

impl fmt::Debug for MaterializedAnalysisInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaterializedAnalysisInput")
            .field("input", &self.input)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

impl MaterializedAnalysisInput {
    /// Bind verified plaintext to the durable input that authorized its fetch.
    pub fn new(input: ArtifactInput, bytes: Arc<[u8]>) -> Self {
        Self { input, bytes }
    }

    pub fn input(&self) -> &ArtifactInput {
        &self.input
    }

    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }
}

/// Ordered request sent to one explicitly assigned analyzer endpoint.
#[derive(Debug, Clone)]
pub struct ArtifactAnalysisRequest {
    pub endpoint: ResolvedModelEndpoint,
    pub inputs: Vec<MaterializedAnalysisInput>,
}

/// Model-derived text with source and analyzer provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactAnalysisResult {
    pub text: String,
    pub source_inputs: Vec<ArtifactInput>,
    pub analyzer: ArtifactAnalyzerProvenance,
    pub usage: TokenUsage,
    pub elapsed: Duration,
}

impl ArtifactAnalysisResult {
    /// Construct a result while deriving provenance from the executed request.
    pub fn from_request(
        request: &ArtifactAnalysisRequest,
        text: impl Into<String>,
        usage: TokenUsage,
        elapsed: Duration,
    ) -> Self {
        Self {
            text: text.into(),
            source_inputs: request
                .inputs
                .iter()
                .map(|input| input.input().clone())
                .collect(),
            analyzer: ArtifactAnalyzerProvenance {
                model_id: request.endpoint.model_id,
                model_slug: request.endpoint.slug.to_string(),
                capability: request.endpoint.capability,
                assignment_source: request.endpoint.source,
            },
            usage,
            elapsed,
        }
    }

    pub fn into_message(self) -> ArtifactAnalysisMessage {
        ArtifactAnalysisMessage {
            text: self.text,
            source_inputs: self.source_inputs,
            analyzer: self.analyzer,
        }
    }
}

/// Open runtime seam for executing explicitly assigned auxiliary analysis.
#[async_trait]
pub trait ArtifactAnalyzer: Send + Sync {
    async fn analyze(
        &self,
        request: ArtifactAnalysisRequest,
    ) -> anyhow::Result<ArtifactAnalysisResult>;
}

/// Complete routing decision for one ordered artifact batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactInputRoute {
    pub ordered: Vec<ArtifactInputDisposition>,
    pub analysis_batches: Vec<AssignedArtifactAnalyzer>,
}

/// Per-input disposition retained in the artifact batch's original order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactInputDisposition {
    Direct(DirectArtifactInput),
    Analyze {
        input: ArtifactInput,
        analyzer_model_id: uuid::Uuid,
        capability: ModelCapabilityId,
    },
}

/// Provider-side input transport lookup for configured models.
pub trait ArtifactTransportResolver: Send + Sync {
    fn resolve_transport(
        &self,
        target: ArtifactTransportTarget<'_>,
        media_type: &MediaType,
    ) -> ArtifactInputTransport;
}

/// Explicit analysis assignment lookup. Implementations must retain normal
/// local, package, then organization-default precedence.
pub trait ArtifactAnalyzerResolver: Send + Sync {
    fn resolve_analyzer(
        &self,
        capability: ModelCapabilityId,
    ) -> Result<ResolvedModelEndpoint, ModelAssignmentResolveError>;
}

impl ArtifactAnalyzerResolver for (&super::ModelAssignmentResolver, super::ResourceRef<'_>) {
    fn resolve_analyzer(
        &self,
        capability: ModelCapabilityId,
    ) -> Result<ResolvedModelEndpoint, ModelAssignmentResolveError> {
        self.0.resolve(self.1, capability)
    }
}

/// A batch artifact cannot be represented by a direct or explicitly assigned route.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArtifactInputRouteError {
    #[error("artifact {artifact} has unsupported media type {media_type}")]
    UnsupportedMediaType {
        artifact: ArtifactId,
        media_type: MediaType,
    },
    #[error("artifact {artifact} requires capability {capability}, but no analyzer is assigned")]
    MissingAnalyzer {
        artifact: ArtifactId,
        capability: ModelCapabilityId,
    },
    #[error(
        "artifact {artifact} ({media_type}, {size_bytes} bytes) exceeds the primary provider inline limit of {max_bytes} bytes; use bounded artifact reads or assign capability {fallback_capability}"
    )]
    DirectInputTooLarge {
        artifact: ArtifactId,
        media_type: MediaType,
        size_bytes: u64,
        max_bytes: u64,
        fallback_capability: ModelCapabilityId,
    },
    #[error("artifact analyzer assignment for {capability} is invalid: {source}")]
    InvalidAnalyzer {
        capability: ModelCapabilityId,
        source: ModelAssignmentResolveError,
    },
    #[error(
        "assigned analyzer {analyzer} cannot accept artifact {artifact} as {modality} via its provider transport"
    )]
    AnalyzerInputUnsupported {
        artifact: ArtifactId,
        analyzer: Slug,
        modality: ModelModality,
    },
}

impl ArtifactInputRouteError {
    /// Identify a historical input that may be omitted when no route exists.
    ///
    /// Invalid or incompatible configured analyzers remain hard failures because
    /// they indicate an operator configuration error rather than absent support.
    pub(super) const fn omittable_historical_artifact(&self) -> Option<ArtifactId> {
        match self {
            Self::UnsupportedMediaType { artifact, .. }
            | Self::MissingAnalyzer { artifact, .. } => Some(*artifact),
            Self::DirectInputTooLarge { .. }
            | Self::InvalidAnalyzer { .. }
            | Self::AnalyzerInputUnsupported { .. } => None,
        }
    }
}

impl ArtifactInputRoute {
    /// Route an ordered artifact batch without scanning model inventory.
    pub fn resolve<T, A>(
        primary: &PrimaryArtifactModel,
        inputs: &[ArtifactInput],
        transport: &T,
        analyzers: &A,
    ) -> Result<Self, ArtifactInputRouteError>
    where
        T: ArtifactTransportResolver,
        A: ArtifactAnalyzerResolver,
    {
        let mut ordered = Vec::with_capacity(inputs.len());
        let mut analysis_batches = Vec::<AssignedArtifactAnalyzer>::new();

        for input in inputs {
            let reference = input.artifact();
            let requirement = ArtifactInputRequirement::for_reference(reference)?;
            let direct_transport = transport.resolve_transport(
                ArtifactTransportTarget {
                    provider: &primary.provider,
                    model: &primary.model,
                    base_url: primary.base_url.as_deref(),
                    capability: ModelCapabilityId::Chat,
                },
                reference.media_type(),
            );
            let direct_modality = requirement.modality_for(direct_transport);
            let primary_supports_direct_modality =
                primary.input_modalities.contains(&direct_modality);
            if primary_supports_direct_modality && direct_transport.accepts(reference.size()) {
                ordered.push(ArtifactInputDisposition::Direct(DirectArtifactInput {
                    input: input.clone(),
                    transport: direct_transport,
                }));
                continue;
            }
            let direct_size_limit = primary_supports_direct_modality
                .then(|| direct_transport.max_bytes())
                .flatten()
                .filter(|max_bytes| reference.size().bytes() > max_bytes.get());

            let endpoint = match analyzers.resolve_analyzer(requirement.analysis_capability) {
                Ok(endpoint) => endpoint,
                Err(ModelAssignmentResolveError::MissingAssignment { capability }) => {
                    if let Some(max_bytes) = direct_size_limit {
                        return Err(ArtifactInputRouteError::DirectInputTooLarge {
                            artifact: reference.id(),
                            media_type: reference.media_type().clone(),
                            size_bytes: reference.size().bytes(),
                            max_bytes: max_bytes.get(),
                            fallback_capability: capability,
                        });
                    }
                    return Err(ArtifactInputRouteError::MissingAnalyzer {
                        artifact: reference.id(),
                        capability,
                    });
                }
                Err(source) => {
                    return Err(ArtifactInputRouteError::InvalidAnalyzer {
                        capability: requirement.analysis_capability,
                        source,
                    });
                }
            };
            let analyzer_transport = transport.resolve_transport(
                ArtifactTransportTarget {
                    provider: &endpoint.provider,
                    model: &endpoint.model,
                    base_url: endpoint.base_url.as_deref(),
                    capability: endpoint.capability,
                },
                reference.media_type(),
            );
            let analyzer_modality = requirement.modality_for(analyzer_transport);
            if !endpoint.input_modalities.contains(&analyzer_modality)
                || !analyzer_transport.accepts(reference.size())
            {
                return Err(ArtifactInputRouteError::AnalyzerInputUnsupported {
                    artifact: reference.id(),
                    analyzer: endpoint.slug,
                    modality: analyzer_modality,
                });
            }
            ordered.push(ArtifactInputDisposition::Analyze {
                input: input.clone(),
                analyzer_model_id: endpoint.model_id,
                capability: endpoint.capability,
            });
            if let Some(group) = analysis_batches.iter_mut().find(|group| {
                group.endpoint.model_id == endpoint.model_id
                    && group.endpoint.capability == endpoint.capability
            }) {
                group.inputs.push(input.clone());
            } else {
                analysis_batches.push(AssignedArtifactAnalyzer {
                    endpoint,
                    inputs: vec![input.clone()],
                });
            }
        }

        Ok(Self {
            ordered,
            analysis_batches,
        })
    }
}

/// Collect each durable artifact input once, preserving conversation order.
pub fn collect_artifact_inputs(
    messages: &[nenjo_models::ConversationMessage],
) -> Vec<ArtifactInput> {
    let analyses = messages
        .iter()
        .filter_map(|message| match message {
            nenjo_models::ConversationMessage::ArtifactAnalysis(analysis) => Some(analysis),
            nenjo_models::ConversationMessage::Chat(_)
            | nenjo_models::ConversationMessage::AssistantToolCalls { .. }
            | nenjo_models::ConversationMessage::ToolResults(_)
            | nenjo_models::ConversationMessage::RuntimeContext(_) => None,
        })
        .collect::<Vec<_>>();
    let mut inputs = Vec::new();
    for message in messages {
        match message {
            nenjo_models::ConversationMessage::Chat(message) => {
                for input in &message.artifacts {
                    if analyses.iter().any(|analysis| analysis.covers(input)) {
                        continue;
                    }
                    if !inputs.iter().any(|found: &ArtifactInput| {
                        found.artifact() == input.artifact()
                            && found.instruction() == input.instruction()
                    }) {
                        inputs.push(input.clone());
                    }
                }
            }
            nenjo_models::ConversationMessage::ToolResults(results) => {
                for reference in results.iter().flat_map(|result| {
                    result.output.parts().iter().filter_map(|part| match part {
                        nenjo_models::ToolOutputPart::Artifact(reference) => Some(reference),
                        nenjo_models::ToolOutputPart::Text(_) => None,
                    })
                }) {
                    let input = ArtifactInput::new(
                        reference.clone(),
                        nenjo_models::ArtifactInputSource::ToolResult,
                    );
                    if analyses.iter().any(|analysis| analysis.covers(&input)) {
                        continue;
                    }
                    if !inputs.iter().any(|found| {
                        found.artifact() == input.artifact()
                            && found.instruction() == input.instruction()
                    }) {
                        inputs.push(input);
                    }
                }
            }
            nenjo_models::ConversationMessage::AssistantToolCalls {
                text: _,
                tool_calls: _,
            } => {}
            nenjo_models::ConversationMessage::ArtifactAnalysis(_) => {}
            nenjo_models::ConversationMessage::RuntimeContext(_) => {}
        }
    }
    inputs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArtifactInputRequirement {
    modality: ModelModality,
    analysis_capability: ModelCapabilityId,
}

impl ArtifactInputRequirement {
    fn modality_for(self, transport: ArtifactInputTransport) -> ModelModality {
        match transport {
            ArtifactInputTransport::InlineText { .. } => ModelModality::Text,
            ArtifactInputTransport::Unsupported
            | ArtifactInputTransport::Inline { .. }
            | ArtifactInputTransport::FileUpload { .. } => self.modality,
        }
    }

    fn for_reference(reference: &ArtifactRef) -> Result<Self, ArtifactInputRouteError> {
        let essence = reference.media_type().essence_str();
        let requirement = if essence.starts_with("image/") {
            Self {
                modality: ModelModality::Image,
                analysis_capability: ModelCapabilityId::AnalyzeImage,
            }
        } else if essence.starts_with("video/") {
            Self {
                modality: ModelModality::Video,
                analysis_capability: ModelCapabilityId::AnalyzeVideo,
            }
        } else if essence.starts_with("audio/") {
            Self {
                modality: ModelModality::Audio,
                analysis_capability: ModelCapabilityId::TranscribeAudio,
            }
        } else if reference.media_type().is_utf8_text() || is_document_media_type(essence) {
            Self {
                modality: ModelModality::File,
                analysis_capability: ModelCapabilityId::AnalyzeDocument,
            }
        } else {
            return Err(ArtifactInputRouteError::UnsupportedMediaType {
                artifact: reference.id(),
                media_type: reference.media_type().clone(),
            });
        };
        Ok(requirement)
    }
}

fn is_document_media_type(essence: &str) -> bool {
    matches!(
        essence,
        "application/pdf"
            | "application/rtf"
            | "application/msword"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.ms-excel"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.ms-powerpoint"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::num::NonZeroU64;

    use nenjo_models::{
        ArtifactInputSource, ArtifactInstruction, ArtifactSize, ChatMessage, Sha256Digest,
    };
    use uuid::Uuid;

    use super::*;
    use crate::media::AssignmentSource;

    struct TestTransports {
        supported: HashMap<(String, String), ArtifactInputTransport>,
    }

    impl ArtifactTransportResolver for TestTransports {
        fn resolve_transport(
            &self,
            target: ArtifactTransportTarget<'_>,
            media_type: &MediaType,
        ) -> ArtifactInputTransport {
            self.supported
                .get(&(
                    target.model.to_string(),
                    media_type.essence_str().to_string(),
                ))
                .copied()
                .unwrap_or(ArtifactInputTransport::Unsupported)
        }
    }

    struct TestAnalyzers {
        endpoints: HashMap<ModelCapabilityId, ResolvedModelEndpoint>,
    }

    impl ArtifactAnalyzerResolver for TestAnalyzers {
        fn resolve_analyzer(
            &self,
            capability: ModelCapabilityId,
        ) -> Result<ResolvedModelEndpoint, ModelAssignmentResolveError> {
            self.endpoints
                .get(&capability)
                .cloned()
                .ok_or(ModelAssignmentResolveError::MissingAssignment { capability })
        }
    }

    fn input(media_type: &str, byte: u8) -> ArtifactInput {
        let reference = ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&format!("sha256:{}", format!("{byte:02x}").repeat(32))).unwrap(),
            MediaType::parse(media_type).unwrap(),
            ArtifactSize::new(128),
        );
        ArtifactInput::new(reference, ArtifactInputSource::UserAttachment)
    }

    fn primary(input_modalities: Vec<ModelModality>) -> PrimaryArtifactModel {
        PrimaryArtifactModel {
            slug: Slug::derive("primary"),
            provider: "test".to_string(),
            model: "primary-model".to_string(),
            base_url: None,
            input_modalities,
        }
    }

    fn analyzer(
        capability: ModelCapabilityId,
        model: &str,
        input_modalities: Vec<ModelModality>,
    ) -> ResolvedModelEndpoint {
        ResolvedModelEndpoint {
            model_id: Uuid::new_v4(),
            provider: "test".to_string(),
            model: model.to_string(),
            base_url: None,
            capability,
            source: AssignmentSource::Local,
            slug: Slug::derive(model),
            input_modalities,
        }
    }

    fn inline_transport() -> ArtifactInputTransport {
        ArtifactInputTransport::Inline {
            max_bytes: NonZeroU64::new(1024).unwrap(),
        }
    }

    fn inline_text_transport() -> ArtifactInputTransport {
        ArtifactInputTransport::InlineText {
            max_bytes: NonZeroU64::new(1024).unwrap(),
        }
    }

    #[test]
    fn collection_deduplicates_exact_requests_but_preserves_distinct_instructions() {
        let base = input("image/png", 42);
        let first = base
            .clone()
            .with_instruction(ArtifactInstruction::parse("read the labels").unwrap());
        let second = base
            .clone()
            .with_instruction(ArtifactInstruction::parse("describe the colors").unwrap());
        let messages = [
            nenjo_models::ConversationMessage::chat(
                ChatMessage::user("inspect").with_artifacts(vec![first.clone(), first.clone()]),
            ),
            nenjo_models::ConversationMessage::chat(
                ChatMessage::user("inspect again").with_artifacts(vec![second.clone()]),
            ),
        ];

        assert_eq!(collect_artifact_inputs(&messages), [first, second]);
    }

    #[test]
    fn direct_requires_primary_modality_and_provider_transport() {
        let image = input("image/png", 1);
        let transports = TestTransports {
            supported: HashMap::from([(
                ("primary-model".to_string(), "image/png".to_string()),
                inline_transport(),
            )]),
        };

        let route = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Image]),
            std::slice::from_ref(&image),
            &transports,
            &TestAnalyzers {
                endpoints: HashMap::new(),
            },
        )
        .unwrap();

        assert_eq!(route.ordered.len(), 1);
        assert!(matches!(
            &route.ordered[0],
            ArtifactInputDisposition::Direct(direct) if direct.input == image
        ));
        assert!(route.analysis_batches.is_empty());
    }

    #[test]
    fn inline_utf8_document_requires_text_instead_of_native_file_modality() {
        let markdown = input("text/markdown", 7);
        let transports = TestTransports {
            supported: HashMap::from([(
                ("primary-model".to_string(), "text/markdown".to_string()),
                inline_text_transport(),
            )]),
        };

        let route = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Text]),
            std::slice::from_ref(&markdown),
            &transports,
            &TestAnalyzers {
                endpoints: HashMap::new(),
            },
        )
        .unwrap();

        assert!(matches!(
            &route.ordered[0],
            ArtifactInputDisposition::Direct(direct) if direct.input == markdown
        ));
    }

    #[test]
    fn csv_alias_routes_directly_as_utf8_text() {
        let csv = input("application/csv", 8);
        let transports = TestTransports {
            supported: HashMap::from([(
                ("primary-model".to_string(), "application/csv".to_string()),
                inline_text_transport(),
            )]),
        };

        let route = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Text]),
            std::slice::from_ref(&csv),
            &transports,
            &TestAnalyzers {
                endpoints: HashMap::new(),
            },
        )
        .unwrap();

        assert!(matches!(
            &route.ordered[0],
            ArtifactInputDisposition::Direct(direct) if direct.input == csv
        ));
    }

    #[test]
    fn oversized_inline_text_reports_transport_limit_instead_of_missing_analyzer() {
        let csv = input("text/csv", 9);
        let error = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Text]),
            std::slice::from_ref(&csv),
            &TestTransports {
                supported: HashMap::from([(
                    ("primary-model".to_string(), "text/csv".to_string()),
                    ArtifactInputTransport::InlineText {
                        max_bytes: NonZeroU64::new(64).unwrap(),
                    },
                )]),
            },
            &TestAnalyzers {
                endpoints: HashMap::new(),
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            ArtifactInputRouteError::DirectInputTooLarge {
                artifact: csv.artifact().id(),
                media_type: MediaType::parse("text/csv").unwrap(),
                size_bytes: 128,
                max_bytes: 64,
                fallback_capability: ModelCapabilityId::AnalyzeDocument,
            }
        );
    }

    #[test]
    fn missing_primary_transport_uses_only_explicit_analyzer_assignment() {
        let image = input("image/png", 2);
        let endpoint = analyzer(
            ModelCapabilityId::AnalyzeImage,
            "image-analyzer",
            vec![ModelModality::Image],
        );
        let transports = TestTransports {
            supported: HashMap::from([(
                ("image-analyzer".to_string(), "image/png".to_string()),
                inline_transport(),
            )]),
        };

        let route = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Image]),
            std::slice::from_ref(&image),
            &transports,
            &TestAnalyzers {
                endpoints: HashMap::from([(ModelCapabilityId::AnalyzeImage, endpoint.clone())]),
            },
        )
        .unwrap();

        assert!(matches!(
            &route.ordered[0],
            ArtifactInputDisposition::Analyze { input, .. } if input == &image
        ));
        assert_eq!(route.analysis_batches.len(), 1);
        assert_eq!(route.analysis_batches[0].endpoint, endpoint);
        assert_eq!(route.analysis_batches[0].inputs, [image]);
    }

    #[test]
    fn batch_groups_comparable_inputs_by_assigned_analyzer() {
        let first = input("image/png", 3);
        let second = input("image/png", 4);
        let endpoint = analyzer(
            ModelCapabilityId::AnalyzeImage,
            "image-analyzer",
            vec![ModelModality::Image],
        );
        let route = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Text]),
            &[first.clone(), second.clone()],
            &TestTransports {
                supported: HashMap::from([(
                    ("image-analyzer".to_string(), "image/png".to_string()),
                    inline_transport(),
                )]),
            },
            &TestAnalyzers {
                endpoints: HashMap::from([(ModelCapabilityId::AnalyzeImage, endpoint)]),
            },
        )
        .unwrap();

        assert_eq!(route.ordered.len(), 2);
        assert_eq!(route.analysis_batches.len(), 1);
        assert_eq!(route.analysis_batches[0].inputs, [first, second]);
    }

    #[test]
    fn unsupported_path_reports_required_assignment_without_inventory_scan() {
        let image = input("image/png", 5);
        let error = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Text]),
            std::slice::from_ref(&image),
            &TestTransports {
                supported: HashMap::new(),
            },
            &TestAnalyzers {
                endpoints: HashMap::new(),
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            ArtifactInputRouteError::MissingAnalyzer {
                artifact: image.artifact().id(),
                capability: ModelCapabilityId::AnalyzeImage,
            }
        );
    }

    #[test]
    fn assigned_analyzer_must_support_both_modality_and_transport() {
        let document = input("application/pdf", 6);
        let endpoint = analyzer(
            ModelCapabilityId::AnalyzeDocument,
            "document-analyzer",
            vec![ModelModality::File],
        );
        let error = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Text]),
            std::slice::from_ref(&document),
            &TestTransports {
                supported: HashMap::new(),
            },
            &TestAnalyzers {
                endpoints: HashMap::from([(ModelCapabilityId::AnalyzeDocument, endpoint.clone())]),
            },
        )
        .unwrap_err();

        assert_eq!(
            error,
            ArtifactInputRouteError::AnalyzerInputUnsupported {
                artifact: document.artifact().id(),
                analyzer: endpoint.slug,
                modality: ModelModality::File,
            }
        );
    }

    #[test]
    fn audio_uses_the_explicit_transcription_assignment() {
        let audio = input("audio/mpeg", 9);
        let endpoint = analyzer(
            ModelCapabilityId::TranscribeAudio,
            "transcriber",
            vec![ModelModality::Audio],
        );
        let route = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Text]),
            std::slice::from_ref(&audio),
            &TestTransports {
                supported: HashMap::from([(
                    ("transcriber".to_string(), "audio/mpeg".to_string()),
                    inline_transport(),
                )]),
            },
            &TestAnalyzers {
                endpoints: HashMap::from([(ModelCapabilityId::TranscribeAudio, endpoint)]),
            },
        )
        .unwrap();

        assert_eq!(route.analysis_batches.len(), 1);
        assert_eq!(
            route.analysis_batches[0].endpoint.capability,
            ModelCapabilityId::TranscribeAudio
        );
    }

    #[test]
    fn mixed_batch_keeps_original_disposition_order() {
        let image = input("image/png", 10);
        let document = input("application/pdf", 11);
        let document_analyzer = analyzer(
            ModelCapabilityId::AnalyzeDocument,
            "document-analyzer",
            vec![ModelModality::File],
        );
        let transports = TestTransports {
            supported: HashMap::from([
                (
                    ("primary-model".to_string(), "image/png".to_string()),
                    inline_transport(),
                ),
                (
                    (
                        "document-analyzer".to_string(),
                        "application/pdf".to_string(),
                    ),
                    inline_transport(),
                ),
            ]),
        };
        let route = ArtifactInputRoute::resolve(
            &primary(vec![ModelModality::Image]),
            &[image.clone(), document.clone()],
            &transports,
            &TestAnalyzers {
                endpoints: HashMap::from([(ModelCapabilityId::AnalyzeDocument, document_analyzer)]),
            },
        )
        .unwrap();

        assert!(matches!(
            &route.ordered[0],
            ArtifactInputDisposition::Direct(direct) if direct.input == image
        ));
        assert!(matches!(
            &route.ordered[1],
            ArtifactInputDisposition::Analyze { input, .. } if input == &document
        ));
    }

    #[test]
    fn analysis_result_preserves_source_and_analyzer_provenance() {
        let first = input("image/png", 7);
        let second = input("image/png", 8);
        let endpoint = analyzer(
            ModelCapabilityId::AnalyzeImage,
            "image-analyzer",
            vec![ModelModality::Image],
        );
        let request = ArtifactAnalysisRequest {
            endpoint: endpoint.clone(),
            inputs: vec![
                MaterializedAnalysisInput::new(first.clone(), Arc::from(&b"first"[..])),
                MaterializedAnalysisInput::new(second.clone(), Arc::from(&b"second"[..])),
            ],
        };

        let result = ArtifactAnalysisResult::from_request(
            &request,
            "comparison",
            TokenUsage {
                input_tokens: 20,
                output_tokens: 5,
            },
            Duration::from_millis(40),
        );

        assert_eq!(
            result
                .source_inputs
                .iter()
                .map(ArtifactInput::artifact)
                .collect::<Vec<_>>(),
            [first.artifact(), second.artifact()]
        );
        assert_eq!(result.analyzer.model_id, endpoint.model_id);
        assert_eq!(result.analyzer.model_slug, endpoint.slug.to_string());
        assert_eq!(result.analyzer.capability, ModelCapabilityId::AnalyzeImage);
        assert_eq!(result.analyzer.assignment_source, AssignmentSource::Local);
        assert_eq!(result.usage.input_tokens, 20);
        assert_eq!(result.elapsed, Duration::from_millis(40));
    }
}
