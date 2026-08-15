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
        let analyses = messages
            .iter()
            .filter_map(|message| match message {
                ConversationMessage::ArtifactAnalysis(analysis) => Some(analysis),
                ConversationMessage::Chat(_)
                | ConversationMessage::AssistantToolCalls { .. }
                | ConversationMessage::ToolResults(_) => None,
            })
            .chain(new_analysis_messages.iter())
            .collect::<Vec<_>>();
        let mut request_messages = messages
            .iter()
            .map(|message| compile_message(message, &analyses))
            .collect::<Vec<_>>();
        request_messages.extend(
            new_analysis_messages
                .iter()
                .map(|analysis| ConversationMessage::user(analysis.model_context())),
        );
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
) -> ConversationMessage {
    match message {
        ConversationMessage::Chat(chat) => {
            let mut chat = chat.clone();
            chat.artifacts
                .retain(|input| !analyses.iter().any(|analysis| analysis.covers(input)));
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
                });
            }
            ConversationMessage::ToolResults(results)
        }
        ConversationMessage::ArtifactAnalysis(analysis) => {
            ConversationMessage::user(analysis.model_context())
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
}
