//! Shared provider-native Chat Completions projection.
//!
//! OpenAI, OpenRouter, generic compatible endpoints, vLLM, and xAI chat all
//! use this conversion. Provider-specific request envelopes remain in their
//! adapters.

use serde::{Deserialize, Serialize};

use crate::openai_multimodal::{ChatArtifactDialect, ChatCompletionsContent, artifact_content};
use crate::{ChatRequest, ChatRole, ConversationMessage, ToolOutputPart};

/// Role behavior at an OpenAI-shaped provider boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstructionRolePolicy {
    NativeDeveloper,
    PortableUserFallback,
}

impl InstructionRolePolicy {
    pub(crate) const fn from_supports_developer_role(supported: bool) -> Self {
        if supported {
            Self::NativeDeveloper
        } else {
            Self::PortableUserFallback
        }
    }

    const fn chat_role(self, role: ChatRole) -> ChatRole {
        match (self, role) {
            (Self::PortableUserFallback, ChatRole::Developer) => ChatRole::User,
            (Self::NativeDeveloper, role) | (Self::PortableUserFallback, role) => role,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatCompletionsMessage {
    pub(crate) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<ChatCompletionsContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_calls: Option<Vec<ChatCompletionsToolCall>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionsToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    pub(crate) function: ChatCompletionsFunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatCompletionsFunctionCall {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

pub(crate) fn convert_messages(
    request: &ChatRequest<'_>,
    artifact_dialect: ChatArtifactDialect,
    role_policy: InstructionRolePolicy,
) -> anyhow::Result<Vec<ChatCompletionsMessage>> {
    let mut native = Vec::new();
    for message in request.messages {
        match message {
            ConversationMessage::AssistantToolCalls { text, tool_calls } => {
                native.push(ChatCompletionsMessage {
                    role: "assistant".to_string(),
                    content: text.clone().map(ChatCompletionsContent::text),
                    tool_call_id: None,
                    tool_calls: Some(
                        tool_calls
                            .iter()
                            .map(|call| ChatCompletionsToolCall {
                                id: Some(call.id.clone()),
                                kind: Some("function".to_string()),
                                function: ChatCompletionsFunctionCall {
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                },
                            })
                            .collect(),
                    ),
                });
            }
            ConversationMessage::ToolResults(results) => {
                for result in results {
                    native.push(ChatCompletionsMessage {
                        role: "tool".to_string(),
                        content: Some(ChatCompletionsContent::text(result.output.text_content())),
                        tool_call_id: Some(result.tool_call_id.clone()),
                        tool_calls: None,
                    });
                }
                let references = results.iter().flat_map(|result| {
                    result.output.parts().iter().filter_map(|part| match part {
                        ToolOutputPart::Artifact(reference) => Some((reference, None)),
                        ToolOutputPart::Text(_) => None,
                    })
                });
                let content = artifact_content(
                    "Inspect the attached artifact.",
                    references,
                    request.prepared_artifacts,
                    artifact_dialect,
                )?;
                if content.has_media() {
                    native.push(ChatCompletionsMessage {
                        role: "user".to_string(),
                        content: Some(content),
                        tool_call_id: None,
                        tool_calls: None,
                    });
                }
            }
            ConversationMessage::Chat(message) => native.push(ChatCompletionsMessage {
                role: role_policy.chat_role(message.role).to_string(),
                content: Some(artifact_content(
                    &message.content,
                    message.artifacts.iter().map(|input| {
                        (
                            input.artifact(),
                            input.instruction().map(|value| value.as_str()),
                        )
                    }),
                    request.prepared_artifacts,
                    artifact_dialect,
                )?),
                tool_call_id: None,
                tool_calls: None,
            }),
            ConversationMessage::ArtifactAnalysis(analysis) => {
                native.push(ChatCompletionsMessage {
                    role: "user".to_string(),
                    content: Some(ChatCompletionsContent::text(analysis.model_context())),
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            ConversationMessage::RuntimeContext(context) => {
                let role = match role_policy {
                    InstructionRolePolicy::NativeDeveloper => context.preferred_role(),
                    InstructionRolePolicy::PortableUserFallback => context.fallback_role(),
                };
                native.push(ChatCompletionsMessage {
                    role: role.to_string(),
                    content: Some(ChatCompletionsContent::text(context.content())),
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
        }
    }

    if role_policy == InstructionRolePolicy::PortableUserFallback {
        coalesce_adjacent_users(&mut native);
    }
    Ok(native)
}

fn coalesce_adjacent_users(messages: &mut Vec<ChatCompletionsMessage>) {
    let mut coalesced: Vec<ChatCompletionsMessage> = Vec::with_capacity(messages.len());
    for mut message in messages.drain(..) {
        let can_merge = message.role == "user"
            && message.tool_call_id.is_none()
            && message.tool_calls.is_none();
        if can_merge
            && let Some(previous) = coalesced.last_mut()
            && previous.role == "user"
            && previous.tool_call_id.is_none()
            && previous.tool_calls.is_none()
            && let (Some(left), Some(right)) = (previous.content.take(), message.content.take())
        {
            previous.content = Some(left.join_adjacent_user(right));
            continue;
        }
        coalesced.push(message);
    }
    *messages = coalesced;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuntimeContextMessage, ToolCall, ToolOutput, ToolResultMessage};

    fn request(messages: &[ConversationMessage]) -> ChatRequest<'_> {
        ChatRequest {
            messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        }
    }

    #[test]
    fn portable_roles_coalesce_only_adjacent_users() {
        let messages = vec![
            ConversationMessage::system("system"),
            ConversationMessage::developer("developer"),
            ConversationMessage::runtime_context(RuntimeContextMessage::session_control(
                "<session>α</session>",
            )),
            ConversationMessage::runtime_context(RuntimeContextMessage::turn_data("")),
            ConversationMessage::user("raw\n\n<context>literal</context>"),
            ConversationMessage::assistant("answer"),
            ConversationMessage::user("next"),
        ];

        let converted = convert_messages(
            &request(&messages),
            ChatArtifactDialect::OpenAi,
            InstructionRolePolicy::PortableUserFallback,
        )
        .unwrap();
        assert_eq!(
            converted
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            ["system", "user", "assistant", "user"]
        );
        assert_eq!(
            serde_json::to_value(&converted[1].content).unwrap(),
            serde_json::json!(
                "developer\n\n<session>α</session>\n\n\n\nraw\n\n<context>literal</context>"
            )
        );
    }

    #[test]
    fn native_developer_and_tool_boundaries_are_preserved() {
        let messages = vec![
            ConversationMessage::developer("developer"),
            ConversationMessage::runtime_context(RuntimeContextMessage::turn_control("clock")),
            ConversationMessage::user("question"),
            ConversationMessage::assistant_tool_calls(
                None,
                vec![ToolCall {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    arguments: "{}".into(),
                }],
            ),
            ConversationMessage::tool_result(ToolResultMessage {
                tool_call_id: "call-1".into(),
                output: ToolOutput::text("result"),
            }),
            ConversationMessage::user("later"),
        ];
        let native = convert_messages(
            &request(&messages),
            ChatArtifactDialect::OpenAi,
            InstructionRolePolicy::NativeDeveloper,
        )
        .unwrap();
        assert_eq!(
            native
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            [
                "developer",
                "developer",
                "user",
                "assistant",
                "tool",
                "user"
            ]
        );
    }
}
