//! Progress, parent input, and explicit completion tools available inside an ability.

use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agents::async_ops::AsyncOpChildHandle;
use crate::tools::{Tool, ToolCategory, ToolOrigin, ToolResult};

use super::{FINISH_ABILITY_TOOL_NAME, json_tool};

#[derive(Deserialize)]
struct ProgressArgs {
    summary: String,
    details: Option<String>,
}

#[derive(Deserialize)]
struct ParentInputArgs {
    question: String,
    context: Option<String>,
}

struct UpdateAbilityParentTool {
    handle: AsyncOpChildHandle,
}

struct AskAbilityParentTool {
    handle: AsyncOpChildHandle,
}

pub(super) struct FinishAbilityTool;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AbilityFinishStatus {
    Completed,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct AbilityFinish {
    pub(super) status: AbilityFinishStatus,
    pub(super) summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<serde_json::Value>,
}

#[async_trait::async_trait]
impl Tool for UpdateAbilityParentTool {
    fn name(&self) -> &str {
        "update_parent_agent"
    }

    fn description(&self) -> &str {
        "Send a compact ability progress update to the parent agent without waking it immediately."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "details": {"type": "string"}
            },
            "required": ["summary"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    /// Publish progress only while this operation is still active.
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: ProgressArgs = serde_json::from_value(args)?;
        if let Some(cancel) = self.handle.cancel_token()
            && cancel.is_cancelled()
        {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some("ability operation was stopped".into()),
            });
        }
        self.handle
            .progress(
                parsed.summary,
                parsed.details,
                crate::agents::runner::turn_loop::current_events_tx(),
            )
            .await;
        Ok(json_tool(serde_json::json!({ "status": "delivered" })))
    }
}

#[async_trait::async_trait]
impl Tool for AskAbilityParentTool {
    fn name(&self) -> &str {
        "ask_parent_agent"
    }

    fn description(&self) -> &str {
        "Ask the parent agent for input and wait until it responds with send_input."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {"type": "string"},
                "context": {"type": "string"}
            },
            "required": ["question"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    /// Suspend for parent input, returning a tool error if the operation ends first.
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: ParentInputArgs = serde_json::from_value(args)?;
        match self
            .handle
            .ask(
                parsed.question,
                parsed.context,
                crate::agents::runner::turn_loop::current_events_tx(),
            )
            .await
        {
            Some(message) => Ok(json_tool(serde_json::json!({ "message": message }))),
            None => Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some("parent did not provide input before the operation ended".into()),
            }),
        }
    }
}

#[async_trait::async_trait]
impl Tool for FinishAbilityTool {
    fn name(&self) -> &str {
        FINISH_ABILITY_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Finish this ability operation. Use completed only after all requested actions and verification have succeeded. Use failed when the ability cannot complete the request. Ordinary assistant prose does not finish an ability."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["completed", "failed"]
                },
                "summary": {
                    "type": "string",
                    "minLength": 1,
                    "description": "A concise account of the completed result or concrete failure."
                },
                "result": {
                    "description": "Optional structured result for the parent agent."
                }
            },
            "required": ["status", "summary"]
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

    /// Validate the terminal payload; `failed` is a valid invocation with a failed outcome.
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let mut finish: AbilityFinish = serde_json::from_value(args)?;
        finish.summary = finish.summary.trim().to_string();
        if finish.summary.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some("summary is required".into()),
            });
        }
        Ok(json_tool(serde_json::to_value(finish)?))
    }
}

/// Add child-only communication tools and the required terminal `finish` tool.
pub(super) fn ability_child_tools(handle: AsyncOpChildHandle) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(UpdateAbilityParentTool {
            handle: handle.clone(),
        }),
        Arc::new(AskAbilityParentTool { handle }),
        Arc::new(FinishAbilityTool),
    ]
}
