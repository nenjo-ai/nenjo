//! Host-side preparation of durable artifact references for one model request.

use nenjo_models::{
    ArtifactAnalysisMessage, ArtifactInput, ArtifactInputSource, ConversationMessage,
    PreparedArtifactInputs, TokenUsage,
};

/// Ephemeral provider-request inputs plus auxiliary analysis accounting.
#[derive(Debug, Clone)]
pub struct PreparedModelArtifacts {
    /// Ephemeral conversation compiled for the primary provider request.
    pub request_messages: Vec<ConversationMessage>,
    pub artifacts: PreparedArtifactInputs,
    /// Newly derived messages that must also enter durable conversation history.
    pub new_analysis_messages: Vec<ArtifactAnalysisMessage>,
    pub usage: TokenUsage,
}

impl PreparedModelArtifacts {
    /// Compile durable references and derived analysis into one ephemeral provider request.
    pub fn new(
        messages: &[ConversationMessage],
        artifacts: PreparedArtifactInputs,
        new_analysis_messages: Vec<ArtifactAnalysisMessage>,
        usage: TokenUsage,
    ) -> Self {
        Self::with_ephemeral_context(
            messages,
            artifacts,
            new_analysis_messages,
            usage,
            &[],
            Vec::new(),
        )
    }

    /// Compile a request with host-derived context that is intentionally not persisted.
    ///
    /// This is used for deterministic local derivatives such as PDF page text and
    /// rendered page images. Durable history retains the authoritative source artifact;
    /// each request rebuilds or reuses the derivative from the source digest.
    pub fn with_ephemeral_context(
        messages: &[ConversationMessage],
        artifacts: PreparedArtifactInputs,
        new_analysis_messages: Vec<ArtifactAnalysisMessage>,
        usage: TokenUsage,
        suppressed_inputs: &[ArtifactInput],
        additional_request_messages: Vec<ConversationMessage>,
    ) -> Self {
        let analyses = messages
            .iter()
            .filter_map(|message| match message {
                ConversationMessage::ArtifactAnalysis(analysis) => Some(analysis),
                ConversationMessage::Chat(_)
                | ConversationMessage::AssistantToolCalls { .. }
                | ConversationMessage::ToolResults(_)
                | ConversationMessage::RuntimeContext(_) => None,
            })
            .chain(new_analysis_messages.iter())
            .collect::<Vec<_>>();
        let mut request_messages = messages
            .iter()
            .map(|message| compile_message(message, &analyses, suppressed_inputs))
            .collect::<Vec<_>>();
        request_messages.extend(
            new_analysis_messages
                .iter()
                .map(|analysis| ConversationMessage::user(analysis.model_context())),
        );
        request_messages.extend(additional_request_messages);
        Self {
            request_messages,
            artifacts,
            new_analysis_messages,
            usage,
        }
    }
}

fn compile_message(
    message: &ConversationMessage,
    analyses: &[&ArtifactAnalysisMessage],
    suppressed_inputs: &[ArtifactInput],
) -> ConversationMessage {
    match message {
        ConversationMessage::Chat(chat) => {
            let mut chat = chat.clone();
            chat.artifacts.retain(|input| {
                !analyses.iter().any(|analysis| analysis.covers(input))
                    && !suppressed_inputs.iter().any(|suppressed| {
                        suppressed.artifact() == input.artifact()
                            && suppressed.instruction() == input.instruction()
                    })
            });
            ConversationMessage::Chat(chat)
        }
        ConversationMessage::AssistantToolCalls { text, tool_calls } => {
            ConversationMessage::AssistantToolCalls {
                text: text.clone(),
                tool_calls: tool_calls.clone(),
            }
        }
        ConversationMessage::ToolResults(results) => {
            let mut results = results.clone();
            for result in &mut results {
                result.output.retain_artifacts(|artifact| {
                    let input =
                        ArtifactInput::new(artifact.clone(), ArtifactInputSource::ToolResult);
                    !analyses.iter().any(|analysis| analysis.covers(&input))
                        && !suppressed_inputs.iter().any(|suppressed| {
                            suppressed.artifact() == input.artifact()
                                && suppressed.instruction() == input.instruction()
                        })
                });
            }
            ConversationMessage::ToolResults(results)
        }
        ConversationMessage::ArtifactAnalysis(analysis) => {
            ConversationMessage::user(analysis.model_context())
        }
        ConversationMessage::RuntimeContext(context) => {
            ConversationMessage::runtime_context(context.clone())
        }
    }
}

/// Open host/runtime seam for materialization, routing, and auxiliary analysis.
#[async_trait::async_trait]
pub trait ArtifactInputPreparer: Send + Sync {
    async fn prepare(
        &self,
        messages: &[ConversationMessage],
        agent: &crate::manifest::AgentManifest,
        model: &crate::manifest::ModelManifest,
    ) -> anyhow::Result<PreparedModelArtifacts>;
}

/// Marker used by providers that do not install artifact input preparation.
#[derive(Debug, Clone, Copy, Default)]
#[doc(hidden)]
pub struct NoArtifactInputPreparer;

#[async_trait::async_trait]
impl ArtifactInputPreparer for NoArtifactInputPreparer {
    async fn prepare(
        &self,
        _messages: &[ConversationMessage],
        _agent: &crate::manifest::AgentManifest,
        _model: &crate::manifest::ModelManifest,
    ) -> anyhow::Result<PreparedModelArtifacts> {
        anyhow::bail!("artifact input preparation is not configured")
    }
}

#[cfg(test)]
mod tests {
    use nenjo_models::{
        ArtifactAnalysisAssignmentSource, ArtifactAnalyzerProvenance, ArtifactId, ArtifactInput,
        ArtifactInputSource, ArtifactRef, ArtifactSize, ChatMessage, MediaType, ModelCapabilityId,
        Sha256Digest, ToolResultMessage,
    };
    use uuid::Uuid;

    use super::*;

    fn reference() -> ArtifactRef {
        ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&format!("sha256:{}", "d".repeat(64))).unwrap(),
            MediaType::parse("image/png").unwrap(),
            ArtifactSize::new(42),
        )
    }

    #[test]
    fn analyzed_references_are_removed_only_from_ephemeral_messages() {
        let reference = reference();
        let messages = vec![
            ConversationMessage::chat(ChatMessage::user("inspect").with_artifacts(vec![
                ArtifactInput::new(reference.clone(), ArtifactInputSource::UserAttachment),
            ])),
            ConversationMessage::tool_result(
                ToolResultMessage::text("call", "metadata").with_artifact(reference.clone()),
            ),
        ];
        let analysis = ArtifactAnalysisMessage {
            text: "derived text".into(),
            source_inputs: vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )],
            analyzer: ArtifactAnalyzerProvenance {
                model_id: Uuid::new_v4(),
                model_slug: "analyzer".into(),
                capability: ModelCapabilityId::AnalyzeImage,
                assignment_source: ArtifactAnalysisAssignmentSource::Local,
            },
        };

        let prepared = PreparedModelArtifacts::new(
            &messages,
            PreparedArtifactInputs::default(),
            vec![analysis],
            TokenUsage::default(),
        );

        assert!(
            messages
                .iter()
                .any(ConversationMessage::has_artifact_references)
        );
        assert!(
            prepared
                .request_messages
                .iter()
                .all(|message| !message.has_artifact_references())
        );
        assert!(prepared.request_messages.iter().any(|message| {
            message
                .as_chat()
                .is_some_and(|chat| chat.content.contains("derived text"))
        }));
    }

    #[test]
    fn local_derivatives_replace_only_the_ephemeral_source_input() {
        let source = reference();
        let input = ArtifactInput::new(source.clone(), ArtifactInputSource::UserAttachment);
        let durable = vec![ConversationMessage::chat(
            ChatMessage::user("read it").with_artifacts(vec![input.clone()]),
        )];
        let local_context = ConversationMessage::user("bounded local derivative");

        let prepared = PreparedModelArtifacts::with_ephemeral_context(
            &durable,
            PreparedArtifactInputs::default(),
            Vec::new(),
            TokenUsage::default(),
            &[input],
            vec![local_context],
        );

        assert!(durable[0].has_artifact_references());
        assert!(
            prepared
                .request_messages
                .iter()
                .all(|message| !message.has_artifact_references())
        );
        assert!(prepared.request_messages.iter().any(|message| {
            message
                .as_chat()
                .is_some_and(|chat| chat.content == "bounded local derivative")
        }));
    }
}
