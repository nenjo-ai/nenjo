//! Standard Responses input parts supported by vLLM's Responses endpoint.
//!
//! Video/audio Chat Completions extensions and document file inputs are not
//! advertised here: a server's Responses schema must support a modality too.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;

use crate::openai_multimodal::{
    ChatArtifactDialect, chat_artifact_transport, guarded_text_artifact,
};
use crate::{ArtifactInputTransport, ArtifactRef, PreparedArtifactInputs};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponsesInputContent {
    Text(String),
    Parts(Vec<ResponsesInputPart>),
}

impl ResponsesInputContent {
    /// Coalesce adjacent user turns without flattening media or moving instructions.
    pub(super) fn append(&mut self, next: Self) {
        match (self, next) {
            (Self::Text(left), Self::Text(right)) => {
                left.push_str("\n\n");
                left.push_str(&right);
            }
            (left, right) => {
                let previous = std::mem::replace(left, Self::Parts(Vec::new()));
                let mut parts = previous.into_parts();
                parts.push(ResponsesInputPart::InputText {
                    text: "\n\n".into(),
                });
                parts.extend(right.into_parts());
                *left = Self::Parts(parts);
            }
        }
    }

    fn into_parts(self) -> Vec<ResponsesInputPart> {
        match self {
            Self::Text(text) => vec![ResponsesInputPart::InputText { text }],
            Self::Parts(parts) => parts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponsesInputPart {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        detail: &'static str,
    },
}

/// Transport support is separate from the selected model's vision capability.
pub(crate) fn artifact_transport(media_type: &str) -> ArtifactInputTransport {
    let transport = chat_artifact_transport(ChatArtifactDialect::Vllm, media_type);
    if matches!(transport, ArtifactInputTransport::InlineText { .. })
        || matches!(
            media_type,
            "image/png" | "image/jpeg" | "image/webp" | "image/gif"
        )
    {
        transport
    } else {
        ArtifactInputTransport::Unsupported
    }
}

/// Encode bounded, digest-verified artifacts for attachments or function outputs.
pub(super) fn artifact_content<'a>(
    text: &str,
    artifacts: impl IntoIterator<Item = (&'a ArtifactRef, Option<&'a str>)>,
    prepared: Option<&PreparedArtifactInputs>,
) -> anyhow::Result<ResponsesInputContent> {
    let mut artifacts = artifacts.into_iter().peekable();
    if artifacts.peek().is_none() {
        return Ok(ResponsesInputContent::Text(text.to_owned()));
    }
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(ResponsesInputPart::InputText {
            text: text.to_owned(),
        });
    }
    for (reference, instruction) in artifacts {
        let media_type = reference.media_type().essence_str();
        let transport = artifact_transport(media_type);
        anyhow::ensure!(
            transport != ArtifactInputTransport::Unsupported,
            "artifact {} has unsupported Responses media type '{media_type}'",
            reference.id()
        );
        anyhow::ensure!(
            transport.accepts(reference.size()),
            "artifact {} exceeds the Responses inline input limit",
            reference.id()
        );
        let artifact = prepared
            .and_then(|inputs| inputs.get(reference))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "artifact {} was not materialized for this provider request",
                    reference.id()
                )
            })?;
        if let Some(instruction) = instruction {
            parts.push(ResponsesInputPart::InputText {
                text: instruction.to_owned(),
            });
        }
        if let Some(text) = artifact.utf8_text() {
            parts.push(ResponsesInputPart::InputText {
                text: guarded_text_artifact(reference, media_type, text),
            });
        } else {
            parts.push(ResponsesInputPart::InputImage {
                image_url: format!(
                    "data:{media_type};base64,{}",
                    STANDARD.encode(artifact.bytes())
                ),
                detail: "auto",
            });
        }
    }
    Ok(ResponsesInputContent::Parts(parts))
}
