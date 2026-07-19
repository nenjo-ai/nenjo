//! Explicit user response tool.

use anyhow::Result;
use serde::Deserialize;
use std::fmt;

use super::async_ops::AsyncOpManager;
use crate::tools::{Tool, ToolCategory, ToolOrigin, ToolResult};

pub(crate) const RESPOND_TO_USER_TOOL_NAME: &str = "respond_to_user";
pub(crate) const TERMINAL_RESPONSE_BLOCKED_BY_ASYNC_OPS: &str = "respond_to_user with a terminal status is unavailable while model-visible async operations are still running or waiting for input";
const INVALID_STATUS_ERROR: &str =
    "status must be one of in_progress, completed, blocked, needs_user_input";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RespondToUserStatus {
    InProgress,
    Completed,
    Blocked,
    NeedsUserInput,
}

impl RespondToUserStatus {
    pub(crate) fn parse(status: &str) -> Option<Self> {
        match status.trim() {
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "blocked" => Some(Self::Blocked),
            "needs_user_input" => Some(Self::NeedsUserInput),
            _ => None,
        }
    }

    pub(crate) fn from_tool_call(tool_call: &nenjo_models::ToolCall) -> Self {
        if tool_call.name != RESPOND_TO_USER_TOOL_NAME {
            return Self::Completed;
        }

        serde_json::from_str::<serde_json::Value>(&tool_call.arguments)
            .ok()
            .and_then(|args| {
                args.get("status")
                    .and_then(serde_json::Value::as_str)
                    .and_then(Self::parse)
            })
            .unwrap_or(Self::Completed)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::NeedsUserInput => "needs_user_input",
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Blocked | Self::NeedsUserInput)
    }
}

impl fmt::Display for RespondToUserStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) struct RespondToUserTool {
    async_ops: AsyncOpManager,
}

impl RespondToUserTool {
    pub(crate) fn new(async_ops: AsyncOpManager) -> Self {
        Self { async_ops }
    }
}

#[derive(Deserialize)]
struct RespondToUserArgs {
    message: String,
    #[serde(default = "default_status")]
    status: String,
}

fn default_status() -> String {
    "completed".into()
}

#[async_trait::async_trait]
impl Tool for RespondToUserTool {
    fn name(&self) -> &str {
        RESPOND_TO_USER_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Send a user-visible response. Use status=in_progress for progress updates while continuing work. Use status=completed only when the user's request is fully handled, status=blocked when you cannot continue, or status=needs_user_input when the user must answer before work can continue. Terminal statuses end the turn and are unavailable while model-visible async operations are still running."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "message": {
                    "type": "string",
                    "minLength": 1,
                    "description": "User-facing text to show in the chat. Keep in_progress updates brief; make terminal messages complete enough to stand alone."
                },
                "status": {
                    "type": "string",
                    "enum": ["in_progress", "completed", "blocked", "needs_user_input"],
                    "default": "completed",
                    "description": "Response state. in_progress keeps the turn open. completed, blocked, and needs_user_input end the turn."
                }
            },
            "required": ["message"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    fn is_terminal(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: RespondToUserArgs = serde_json::from_value(args)?;
        let Some(status) = RespondToUserStatus::parse(&parsed.status) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(INVALID_STATUS_ERROR.into()),
            });
        };
        if status.is_terminal() && self.async_ops.has_open_model_visible().await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(TERMINAL_RESPONSE_BLOCKED_BY_ASYNC_OPS.into()),
            });
        }

        let message = parsed.message.trim();
        if message.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("message is required".into()),
            });
        }

        Ok(ToolResult {
            success: true,
            output: message.to_string(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::async_ops::{AsyncOpId, AsyncOpKind, AsyncOpSignal, StartAsyncOp};
    use crate::tools::AsyncControls;

    #[tokio::test]
    async fn terminal_response_is_rejected_while_model_visible_async_op_is_open() {
        let async_ops = AsyncOpManager::new();
        let _started = async_ops
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_build_1"),
                    kind: AsyncOpKind::Ability,
                    label: "Build".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "Building".into(),
                    model_visible: true,
                    controls: AsyncControls::NONE,
                },
                None,
            )
            .await;
        let tool = RespondToUserTool::new(async_ops);

        let result = tool
            .execute(serde_json::json!({
                "message": "Done",
                "status": "completed",
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some(TERMINAL_RESPONSE_BLOCKED_BY_ASYNC_OPS)
        );
    }

    #[tokio::test]
    async fn in_progress_response_is_allowed_while_model_visible_async_op_is_open() {
        let async_ops = AsyncOpManager::new();
        let _started = async_ops
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_build_1"),
                    kind: AsyncOpKind::Ability,
                    label: "Build".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "Building".into(),
                    model_visible: true,
                    controls: AsyncControls::NONE,
                },
                None,
            )
            .await;
        let tool = RespondToUserTool::new(async_ops);

        let result = tool
            .execute(serde_json::json!({
                "message": "Still working",
                "status": "in_progress",
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "Still working");
    }

    #[tokio::test]
    async fn terminal_response_is_allowed_after_model_visible_async_op_finishes() {
        let async_ops = AsyncOpManager::new();
        let started = async_ops
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_build_1"),
                    kind: AsyncOpKind::Ability,
                    label: "Build".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "Building".into(),
                    model_visible: true,
                    controls: AsyncControls::NONE,
                },
                None,
            )
            .await;
        started
            .handle
            .complete(
                AsyncOpSignal::Completed {
                    summary: "Built".into(),
                    output: None,
                },
                None,
            )
            .await;
        let tool = RespondToUserTool::new(async_ops);

        let result = tool
            .execute(serde_json::json!({
                "message": "Done",
                "status": "completed",
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "Done");
    }
}
