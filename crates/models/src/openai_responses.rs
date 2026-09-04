//! Shared standard Responses API input, local-tool, and output shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::openai_chat::InstructionRolePolicy;
use crate::{ChatRequest, ChatRole, ConversationMessage, TokenUsage, ToolCall, ToolSpec};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponsesInputItem {
    Message {
        role: String,
        content: String,
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
        output: String,
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

    let mut input = Vec::with_capacity(request.messages.len());
    for message in request.messages {
        match message {
            ConversationMessage::AssistantToolCalls { text, tool_calls } => {
                if let Some(content) = text {
                    input.push(ResponsesInputItem::Message {
                        role: "assistant".to_string(),
                        content: content.clone(),
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
                input.extend(
                    results
                        .iter()
                        .map(|result| ResponsesInputItem::FunctionCallOutput {
                            kind: "function_call_output",
                            call_id: result.tool_call_id.clone(),
                            output: result.output.text_content(),
                        }),
                );
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
                    content: message.content.clone(),
                });
            }
            ConversationMessage::ArtifactAnalysis(analysis) => {
                input.push(ResponsesInputItem::Message {
                    role: "user".to_string(),
                    content: analysis.model_context(),
                });
            }
            ConversationMessage::RuntimeContext(context) => {
                let role = match role_policy {
                    InstructionRolePolicy::NativeDeveloper => context.preferred_role(),
                    InstructionRolePolicy::PortableUserFallback => context.fallback_role(),
                };
                input.push(ResponsesInputItem::Message {
                    role: role.to_string(),
                    content: context.content().to_string(),
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
                    previous_content.push_str("\n\n");
                    previous_content.push_str(&content);
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
}

pub(crate) fn response_text(response: &ResponsesResponse) -> Option<String> {
    if let Some(text) = nonempty(response.output_text.as_deref()) {
        return Some(text);
    }
    for output in &response.output {
        for content in &output.content {
            if content.kind.as_deref() == Some("output_text")
                && let Some(text) = nonempty(content.text.as_deref())
            {
                return Some(text);
            }
        }
    }
    response.output.iter().find_map(|output| {
        output
            .content
            .iter()
            .find_map(|content| nonempty(content.text.as_deref()))
    })
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
        .map(|usage| TokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        })
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
