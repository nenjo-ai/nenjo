use std::borrow::Cow;

use serde::Serialize;
use tracing::Level;

use crate::{ChatRole, ConversationMessage, RuntimeContextAuthority, RuntimeContextScope};

const PROVIDER_REQUEST_PARTS_TARGET: &str = "nenjo_models::provider_request::parts";
const PROVIDER_REQUEST_WIRE_TARGET: &str = "nenjo_models::provider_request::wire";

/// Logs a semantic message-by-message view and the exact JSON body passed to a
/// model provider on independently configurable diagnostic targets.
///
/// Serialization is deliberately gated behind the wire trace-level target so
/// large prompts and inline artifacts do not incur work when this diagnostic is
/// disabled. A logging failure must never prevent the provider request itself.
pub(crate) fn debug_provider_request<T>(
    provider: &str,
    model: &str,
    attempt: u32,
    messages: &[ConversationMessage],
    request: &T,
) where
    T: Serialize + ?Sized,
{
    let parts_enabled = tracing::enabled!(target: PROVIDER_REQUEST_PARTS_TARGET, Level::DEBUG);
    let wire_enabled = tracing::enabled!(target: PROVIDER_REQUEST_WIRE_TARGET, Level::TRACE);
    if !parts_enabled && !wire_enabled {
        return;
    }

    if parts_enabled {
        tracing::debug!(
            target: PROVIDER_REQUEST_PARTS_TARGET,
            provider = %provider,
            model = %model,
            attempt,
            part_count = messages.len(),
            "model provider request parts"
        );
        for (part_index, message) in messages.iter().enumerate() {
            let part = ProviderRequestPart::from(message);
            tracing::debug!(
                target: PROVIDER_REQUEST_PARTS_TARGET,
                provider = %provider,
                model = %model,
                attempt,
                part_index,
                part = part.kind.as_str(),
                artifact_count = part.artifact_count,
                content = %part.content,
                "model provider request part"
            );
        }
    }

    if wire_enabled {
        match serialize_provider_request(request) {
            Ok(request_json) => tracing::trace!(
                target: PROVIDER_REQUEST_WIRE_TARGET,
                provider = %provider,
                model = %model,
                attempt,
                request_json = %request_json,
                "model provider wire request"
            ),
            Err(error) => tracing::trace!(
                target: PROVIDER_REQUEST_WIRE_TARGET,
                provider = %provider,
                model = %model,
                attempt,
                error = %error,
                "failed to serialize model provider request for diagnostic logging"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderRequestPartKind {
    System,
    Developer,
    SessionControl,
    SessionData,
    TurnControl,
    TurnData,
    User,
    Assistant,
    AssistantToolCalls,
    ToolResults,
    ArtifactAnalysis,
}

impl ProviderRequestPartKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::SessionControl => "session_control",
            Self::SessionData => "session_data",
            Self::TurnControl => "turn_control",
            Self::TurnData => "turn_data",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::AssistantToolCalls => "assistant_tool_calls",
            Self::ToolResults => "tool_results",
            Self::ArtifactAnalysis => "artifact_analysis",
        }
    }
}

struct ProviderRequestPart<'a> {
    kind: ProviderRequestPartKind,
    content: Cow<'a, str>,
    artifact_count: usize,
}

impl<'a> From<&'a ConversationMessage> for ProviderRequestPart<'a> {
    fn from(message: &'a ConversationMessage) -> Self {
        let (kind, content) = match message {
            ConversationMessage::Chat(chat) => {
                let kind = match chat.role {
                    ChatRole::System => ProviderRequestPartKind::System,
                    ChatRole::Developer => ProviderRequestPartKind::Developer,
                    ChatRole::User => ProviderRequestPartKind::User,
                    ChatRole::Assistant => ProviderRequestPartKind::Assistant,
                };
                (kind, Cow::Borrowed(chat.content.as_str()))
            }
            ConversationMessage::AssistantToolCalls { .. } => (
                ProviderRequestPartKind::AssistantToolCalls,
                pretty_message_json(message),
            ),
            ConversationMessage::ToolResults(_) => (
                ProviderRequestPartKind::ToolResults,
                pretty_message_json(message),
            ),
            ConversationMessage::ArtifactAnalysis(analysis) => (
                ProviderRequestPartKind::ArtifactAnalysis,
                Cow::Owned(analysis.model_context()),
            ),
            ConversationMessage::RuntimeContext(context) => {
                let kind = match (context.scope(), context.authority()) {
                    (RuntimeContextScope::Session, RuntimeContextAuthority::Control) => {
                        ProviderRequestPartKind::SessionControl
                    }
                    (RuntimeContextScope::Session, RuntimeContextAuthority::Data) => {
                        ProviderRequestPartKind::SessionData
                    }
                    (RuntimeContextScope::Turn, RuntimeContextAuthority::Control) => {
                        ProviderRequestPartKind::TurnControl
                    }
                    (RuntimeContextScope::Turn, RuntimeContextAuthority::Data) => {
                        ProviderRequestPartKind::TurnData
                    }
                };
                (kind, Cow::Borrowed(context.content()))
            }
        };
        Self {
            kind,
            content,
            artifact_count: message.unresolved_artifact_count(),
        }
    }
}

fn pretty_message_json(message: &ConversationMessage) -> Cow<'_, str> {
    Cow::Owned(
        serde_json::to_string_pretty(message)
            .unwrap_or_else(|error| format!("<failed to serialize message: {error}>")),
    )
}

fn serialize_provider_request<T>(request: &T) -> serde_json::Result<String>
where
    T: Serialize + ?Sized,
{
    serde_json::to_string(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct TestRequest<'a> {
        model: &'a str,
        messages: Vec<TestMessage<'a>>,
    }

    #[derive(Serialize)]
    struct TestMessage<'a> {
        role: &'a str,
        content: &'a str,
    }

    #[test]
    fn serializes_the_complete_request_without_redaction_or_truncation() {
        let request = TestRequest {
            model: "test-model",
            messages: vec![TestMessage {
                role: "user",
                content: "secret line 1\nline 2",
            }],
        };

        assert_eq!(
            serialize_provider_request(&request).unwrap(),
            r#"{"model":"test-model","messages":[{"role":"user","content":"secret line 1\nline 2"}]}"#
        );
    }

    #[test]
    fn classifies_runtime_context_parts_by_scope_and_authority() {
        let messages = [
            ConversationMessage::runtime_context(crate::RuntimeContextMessage::session_control(
                "session control",
            )),
            ConversationMessage::runtime_context(crate::RuntimeContextMessage::session_data(
                "session data",
            )),
            ConversationMessage::runtime_context(crate::RuntimeContextMessage::turn_control(
                "turn control",
            )),
            ConversationMessage::runtime_context(crate::RuntimeContextMessage::turn_data(
                "turn data",
            )),
        ];

        let kinds = messages
            .iter()
            .map(ProviderRequestPart::from)
            .map(|part| part.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ProviderRequestPartKind::SessionControl,
                ProviderRequestPartKind::SessionData,
                ProviderRequestPartKind::TurnControl,
                ProviderRequestPartKind::TurnData,
            ]
        );
    }

    #[test]
    fn classifies_authored_chat_parts_without_rewriting_content() {
        let messages = [
            ConversationMessage::system("system"),
            ConversationMessage::developer("developer"),
            ConversationMessage::user("user"),
            ConversationMessage::assistant("assistant"),
        ];

        let parts = messages
            .iter()
            .map(ProviderRequestPart::from)
            .collect::<Vec<_>>();
        assert_eq!(parts[0].kind, ProviderRequestPartKind::System);
        assert_eq!(parts[1].kind, ProviderRequestPartKind::Developer);
        assert_eq!(parts[2].kind, ProviderRequestPartKind::User);
        assert_eq!(parts[3].kind, ProviderRequestPartKind::Assistant);
        assert_eq!(parts[0].content, "system");
        assert_eq!(parts[1].content, "developer");
        assert_eq!(parts[2].content, "user");
        assert_eq!(parts[3].content, "assistant");
    }
}
