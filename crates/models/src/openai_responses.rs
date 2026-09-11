//! Shared standard Responses API input, local-tool, and output shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod content;
pub(crate) mod diagnostics;
pub(crate) mod stream;

use crate::openai_chat::InstructionRolePolicy;
use crate::{ChatRequest, ChatRole, ConversationMessage, TokenUsage, ToolCall, ToolSpec};
pub(crate) use content::artifact_transport;
use content::{ResponsesInputContent, artifact_content};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponsesInputItem {
    Message {
        role: String,
        content: ResponsesInputContent,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        output: ResponsesInputContent,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ResponsesTool {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<Value>,
}

impl ResponsesTool {
    pub(crate) fn native(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            name: None,
            description: None,
            parameters: None,
        }
    }
}

pub(crate) fn convert_input(
    request: &ChatRequest<'_>,
    role_policy: InstructionRolePolicy,
) -> anyhow::Result<Vec<ResponsesInputItem>> {
    request.reject_artifact_inputs().map_err(|error| {
        anyhow::anyhow!(
            "Responses fallback cannot encode unresolved multimodal artifact references: {error}"
        )
    })?;

    convert_input_with_artifacts(request, role_policy)
}

/// Project complete local history, resolving media only from verified ephemeral inputs.
pub(crate) fn convert_input_with_artifacts(
    request: &ChatRequest<'_>,
    role_policy: InstructionRolePolicy,
) -> anyhow::Result<Vec<ResponsesInputItem>> {
    request.ensure_artifacts_prepared()?;

    let mut input = Vec::with_capacity(request.messages.len());
    for message in request.messages {
        match message {
            ConversationMessage::AssistantToolCalls { text, tool_calls } => {
                if let Some(content) = text {
                    input.push(ResponsesInputItem::Message {
                        role: "assistant".to_string(),
                        content: ResponsesInputContent::Text(content.clone()),
                    });
                }
                input.extend(
                    tool_calls
                        .iter()
                        .map(|call| ResponsesInputItem::FunctionCall {
                            kind: "function_call",
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        }),
                );
            }
            ConversationMessage::ToolResults(results) => {
                for result in results {
                    input.push(ResponsesInputItem::FunctionCallOutput {
                        kind: "function_call_output",
                        call_id: result.tool_call_id.clone(),
                        output: artifact_content(
                            &result.output.text_content(),
                            result.output.parts().iter().filter_map(|part| match part {
                                crate::ToolOutputPart::Artifact(reference) => {
                                    Some((reference, None))
                                }
                                crate::ToolOutputPart::Text(_) => None,
                            }),
                            request.prepared_artifacts,
                        )?,
                    });
                }
            }
            ConversationMessage::Chat(message) => {
                let role = match (role_policy, message.role) {
                    (InstructionRolePolicy::PortableUserFallback, ChatRole::Developer) => {
                        ChatRole::User
                    }
                    (InstructionRolePolicy::NativeDeveloper, role)
                    | (InstructionRolePolicy::PortableUserFallback, role) => role,
                };
                input.push(ResponsesInputItem::Message {
                    role: role.to_string(),
                    content: artifact_content(
                        &message.content,
                        message.artifacts.iter().map(|input| {
                            (
                                input.artifact(),
                                input.instruction().map(|value| value.as_str()),
                            )
                        }),
                        request.prepared_artifacts,
                    )?,
                });
            }
            ConversationMessage::ArtifactAnalysis(analysis) => {
                input.push(ResponsesInputItem::Message {
                    role: "user".to_string(),
                    content: ResponsesInputContent::Text(analysis.model_context()),
                });
            }
            ConversationMessage::RuntimeContext(context) => {
                let role = match role_policy {
                    InstructionRolePolicy::NativeDeveloper => context.preferred_role(),
                    InstructionRolePolicy::PortableUserFallback => context.fallback_role(),
                };
                input.push(ResponsesInputItem::Message {
                    role: role.to_string(),
                    content: ResponsesInputContent::Text(context.content().to_string()),
                });
            }
        }
    }
    if role_policy == InstructionRolePolicy::PortableUserFallback {
        coalesce_adjacent_user_messages(&mut input);
    }
    Ok(input)
}

fn coalesce_adjacent_user_messages(input: &mut Vec<ResponsesInputItem>) {
    let mut normalized = Vec::with_capacity(input.len());
    for item in input.drain(..) {
        match item {
            ResponsesInputItem::Message { role, content } if role == "user" => {
                if let Some(ResponsesInputItem::Message {
                    role: previous_role,
                    content: previous_content,
                }) = normalized.last_mut()
                    && previous_role == "user"
                {
                    previous_content.append(content);
                } else {
                    normalized.push(ResponsesInputItem::Message { role, content });
                }
            }
            other => normalized.push(other),
        }
    }
    *input = normalized;
}

pub(crate) fn convert_local_tools(
    tools: Option<&[ToolSpec]>,
) -> anyhow::Result<Vec<ResponsesTool>> {
    let converted = crate::openai_tools::convert_tools_checked(tools, crate::sanitize_tool_name)?;
    Ok(converted
        .unwrap_or_default()
        .into_iter()
        .map(|tool| ResponsesTool {
            kind: tool.kind,
            name: Some(tool.function.name),
            description: Some(tool.function.description),
            parameters: Some(tool.function.parameters),
        })
        .collect())
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResponsesResponse {
    #[serde(default)]
    pub(crate) output: Vec<ResponsesOutput>,
    #[serde(default)]
    pub(crate) output_text: Option<String>,
    #[serde(default)]
    pub(crate) usage: Option<ResponsesUsage>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResponsesOutput {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) call_id: Option<String>,
    #[serde(rename = "type", default)]
    pub(crate) kind: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) arguments: Option<Value>,
    #[serde(default)]
    pub(crate) content: Vec<ResponsesContent>,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ResponsesContent {
    #[serde(rename = "type", default)]
    pub(crate) kind: Option<String>,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) annotations: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResponsesUsage {
    #[serde(default, alias = "prompt_tokens")]
    pub(crate) input_tokens: u64,
    #[serde(default, alias = "completion_tokens")]
    pub(crate) output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<ResponsesInputTokensDetails>,
    #[serde(default)]
    output_tokens_details: Option<ResponsesOutputTokensDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesInputTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesOutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

impl ResponsesUsage {
    /// Preserve optional breakdowns without adding them to totals a second time.
    pub(crate) fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_input_tokens: self
                .input_tokens_details
                .as_ref()
                .and_then(|v| v.cached_tokens),
            reasoning_tokens: self
                .output_tokens_details
                .as_ref()
                .and_then(|v| v.reasoning_tokens),
        }
    }
}

pub(crate) fn response_text(response: &ResponsesResponse) -> Option<String> {
    if let Some(text) = nonempty(response.output_text.as_deref()) {
        return Some(text);
    }
    let text: String = response
        .output
        .iter()
        .filter(|output| output.kind.as_deref().is_none_or(|kind| kind == "message"))
        .flat_map(|output| &output.content)
        .filter(|content| matches!(content.kind.as_deref(), Some("output_text" | "message")))
        .filter_map(|content| content.text.as_deref())
        .collect();
    (!text.is_empty()).then_some(text)
}

pub(crate) fn response_tool_calls(response: &ResponsesResponse) -> Vec<ToolCall> {
    response
        .output
        .iter()
        .filter(|output| output.kind.as_deref() == Some("function_call"))
        .filter_map(|output| {
            let name = output.name.clone()?;
            let arguments = match output.arguments.as_ref() {
                Some(Value::String(arguments)) => arguments.clone(),
                Some(arguments) => arguments.to_string(),
                None => "{}".to_string(),
            };
            Some(ToolCall {
                id: output
                    .call_id
                    .clone()
                    .or_else(|| output.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name,
                arguments,
            })
        })
        .collect()
}

pub(crate) fn response_usage(response: &ResponsesResponse) -> TokenUsage {
    response
        .usage
        .as_ref()
        .map(ResponsesUsage::token_usage)
        .unwrap_or_default()
}

fn nonempty(text: Option<&str>) -> Option<String> {
    text.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuntimeContextMessage, ToolOutput, ToolResultMessage};

    #[test]
    fn full_history_is_ordered_and_portable_users_are_coalesced() {
        let messages = vec![
            ConversationMessage::system("system"),
            ConversationMessage::developer("developer"),
            ConversationMessage::runtime_context(RuntimeContextMessage::session_control(
                "session-context",
            )),
            ConversationMessage::runtime_context(RuntimeContextMessage::turn_data("turn-context")),
            ConversationMessage::user("first"),
            ConversationMessage::assistant_tool_calls(
                Some("checking".into()),
                vec![ToolCall {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    arguments: "{\"q\":\"α\"}".into(),
                }],
            ),
            ConversationMessage::tool_result(ToolResultMessage {
                tool_call_id: "call-1".into(),
                output: ToolOutput::text("result"),
            }),
            ConversationMessage::user("later"),
        ];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };
        let input = convert_input(&request, InstructionRolePolicy::PortableUserFallback).unwrap();
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            serde_json::json!([
                {"role":"system","content":"system"},
                {"role":"user","content":"developer\n\nsession-context\n\nturn-context\n\nfirst"},
                {"role":"assistant","content":"checking"},
                {"type":"function_call","call_id":"call-1","name":"lookup","arguments":"{\"q\":\"α\"}"},
                {"type":"function_call_output","call_id":"call-1","output":"result"},
                {"role":"user","content":"later"}
            ])
        );
    }

    #[test]
    fn parses_local_function_calls_and_usage() {
        let response: ResponsesResponse = serde_json::from_value(serde_json::json!({
            "output": [{
                "type": "function_call",
                "call_id": "call-9",
                "name": "inspect",
                "arguments": {"operation_id":"op-1"}
            }],
            "usage": {"input_tokens": 7, "output_tokens": 3}
        }))
        .unwrap();
        assert_eq!(response_tool_calls(&response)[0].id, "call-9");
        assert_eq!(response_tool_calls(&response)[0].name, "inspect");
        assert_eq!(response_usage(&response).input_tokens, 7);
    }
}
