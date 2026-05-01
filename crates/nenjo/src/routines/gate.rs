//! Pass verdict tool — deterministic pass/fail signal for routine steps.
//!
//! Instead of parsing free-form LLM text for keywords like "pass" or "fail",
//! the step agent calls this tool with a structured verdict. This eliminates
//! ambiguity from natural language (e.g. "I'll pass this along" being
//! misinterpreted as a pass verdict).

use std::sync::Arc;

use anyhow::{Result, bail};
use nenjo_models::ChatMessage;
use nenjo_tools::{Tool, ToolCategory, ToolResult};
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

use super::RoutineEvent;
use crate::agents::runner::{AgentRunner, types::TurnOutput};
use crate::types::TaskType;

/// Tool name constant used for injection and extraction.
pub const PASS_VERDICT_TOOL_NAME: &str = "pass_verdict";
const PASS_VERDICT_RETRY_LIMIT: usize = 2;

// ---------------------------------------------------------------------------
// PassVerdictTool
// ---------------------------------------------------------------------------

/// A tool that routine step agents call to submit their pass/fail verdict.
///
/// The tool's `execute()` is a no-op — the real value is in the structured
/// arguments captured by the turn loop. After execution completes,
/// [`extract_pass_verdict`] reads the verdict from the conversation messages.
pub struct PassVerdictTool;

impl Default for PassVerdictTool {
    fn default() -> Self {
        Self
    }
}

impl PassVerdictTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for PassVerdictTool {
    fn name(&self) -> &str {
        PASS_VERDICT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Submit the required final verdict for this routine execution. You MUST call this tool exactly once \
         as your final action after you have completed the work. Use verdict \"pass\" when the step output \
         should allow execution to continue, or \"fail\" when the step should fail or route down a failure path. \
         Always include concise reasoning that explains the decision."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "verdict": {
                    "type": "string",
                    "enum": ["pass", "fail"],
                    "description": "Final verdict for this routine step: \"pass\" to continue, \"fail\" to stop or follow failure routing."
                },
                "reasoning": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Short explanation of why the completed work should pass or fail."
                }
            },
            "required": ["verdict", "reasoning"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn is_terminal(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let verdict = args
            .get("verdict")
            .and_then(|v| v.as_str())
            .unwrap_or("pass");
        let reasoning = args.get("reasoning").and_then(|v| v.as_str()).unwrap_or("");

        Ok(ToolResult {
            success: true,
            output: format!("Pass verdict recorded: {verdict}. {reasoning}"),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Verdict extraction from conversation messages
// ---------------------------------------------------------------------------

/// Extract a pass verdict from the conversation messages by looking for a
/// `pass_verdict` tool call. Returns `None` if the tool was never called.
pub fn extract_pass_verdict(messages: &[ChatMessage]) -> Option<bool> {
    // Walk messages in reverse — the last call wins if the agent called it
    // multiple times (shouldn't happen, but be defensive).
    for msg in messages.iter().rev() {
        if msg.role != "assistant" {
            continue;
        }

        // Assistant messages with tool calls are stored as JSON:
        // {"content": "...", "tool_calls": [{"id": "...", "name": "...", "arguments": "..."}]}
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) else {
            continue;
        };

        let Some(tool_calls) = parsed.get("tool_calls").and_then(|v| v.as_array()) else {
            continue;
        };

        for tc in tool_calls {
            let name = tc.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name != PASS_VERDICT_TOOL_NAME {
                continue;
            }

            let args_str = tc.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let args: serde_json::Value =
                serde_json::from_str(args_str).unwrap_or(serde_json::Value::Null);

            if let Some(verdict) = args.get("verdict").and_then(|v| v.as_str()) {
                return match verdict {
                    "pass" => Some(true),
                    "fail" => Some(false),
                    _ => None,
                };
            }
        }
    }

    None
}

/// Structured pass verdict with reasoning.
#[derive(Debug, Clone)]
pub struct PassVerdict {
    pub passed: bool,
    pub reasoning: Option<String>,
}

/// Extract the reasoning string from a `pass_verdict` tool call in the
/// conversation messages. Returns `None` if the tool was never called.
pub fn extract_pass_reasoning(messages: &[ChatMessage]) -> Option<String> {
    for msg in messages.iter().rev() {
        if msg.role != "assistant" {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) else {
            continue;
        };
        let Some(tool_calls) = parsed.get("tool_calls").and_then(|v| v.as_array()) else {
            continue;
        };
        for tc in tool_calls {
            let name = tc.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name != PASS_VERDICT_TOOL_NAME {
                continue;
            }
            let args_str = tc.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let args: serde_json::Value =
                serde_json::from_str(args_str).unwrap_or(serde_json::Value::Null);
            if let Some(reasoning) = args.get("reasoning").and_then(|v| v.as_str())
                && !reasoning.is_empty()
            {
                return Some(reasoning.to_string());
            }
        }
    }
    None
}

/// Resolve a pass verdict from conversation messages.
///
/// The runtime requires the `pass_verdict` tool to be called before a
/// routine step can complete.
pub fn resolve_pass_verdict(messages: &[ChatMessage]) -> Result<PassVerdict> {
    if let Some(passed) = extract_pass_verdict(messages) {
        let reasoning = extract_pass_reasoning(messages);
        return Ok(PassVerdict { passed, reasoning });
    }

    bail!("Agent did not call required pass_verdict tool")
}

// ---------------------------------------------------------------------------
// Convenience: create the tool as Arc<dyn Tool>
// ---------------------------------------------------------------------------

/// Create the pass verdict tool wrapped in an Arc for injection into agent tools.
pub fn pass_verdict_tool() -> Arc<dyn Tool> {
    Arc::new(PassVerdictTool::new())
}

fn verdict_retry_prompt(previous_text: &str) -> String {
    if previous_text.trim().is_empty() {
        format!(
            "You did not call `{}`. Call `{}` exactly once now as your final action. \
             Do not continue working. Use `verdict` of `pass` or `fail` and include concise `reasoning`.",
            PASS_VERDICT_TOOL_NAME, PASS_VERDICT_TOOL_NAME
        )
    } else {
        format!(
            "Your previous response did not call `{}`. Based on the work you already completed, \
             call `{}` exactly once now as your final action. Do not redo the task. Do not provide \
             more free-form analysis unless it is needed inside `reasoning`.\n\nPrevious response:\n{}",
            PASS_VERDICT_TOOL_NAME, PASS_VERDICT_TOOL_NAME, previous_text
        )
    }
}

fn chat_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .filter(|m| m.role != "system" && m.role != "developer")
        .cloned()
        .collect()
}

async fn stream_turn_output(
    runner: &AgentRunner,
    task: TaskType,
    step_id: Uuid,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<TurnOutput> {
    let mut handle = runner.task_stream(task).await?;
    while let Some(event) = handle.recv().await {
        let _ = events_tx.send(RoutineEvent::AgentEvent { step_id, event });
    }
    handle.output().await
}

pub async fn execute_with_pass_verdict(
    runner: &AgentRunner,
    task: TaskType,
    project_id: Uuid,
    step_id: Uuid,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<TurnOutput> {
    let mut attempts = 0usize;
    let mut pending_task = task;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_tool_calls = 0u32;

    loop {
        let output = stream_turn_output(runner, pending_task, step_id, events_tx).await?;
        total_input_tokens += output.input_tokens;
        total_output_tokens += output.output_tokens;
        total_tool_calls += output.tool_calls;

        if extract_pass_verdict(&output.messages).is_some() {
            return Ok(TurnOutput {
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                tool_calls: total_tool_calls,
                ..output
            });
        }

        attempts += 1;
        if attempts > PASS_VERDICT_RETRY_LIMIT {
            bail!(
                "Agent did not call required {} tool after {} corrective attempt(s)",
                PASS_VERDICT_TOOL_NAME,
                PASS_VERDICT_RETRY_LIMIT
            );
        }

        warn!(
            attempt = attempts,
            "Agent omitted pass_verdict tool call, retrying with explicit instruction"
        );

        pending_task = TaskType::Chat {
            user_message: verdict_retry_prompt(&output.text),
            history: chat_history(&output.messages),
            project_id,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- PassVerdictTool schema --

    #[test]
    fn tool_has_correct_name() {
        let tool = PassVerdictTool::new();
        assert_eq!(tool.name(), "pass_verdict");
    }

    #[test]
    fn tool_schema_has_required_fields() {
        let tool = PassVerdictTool::new();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("verdict")));
        assert!(required.contains(&serde_json::json!("reasoning")));
    }

    #[test]
    fn tool_schema_verdict_is_enum() {
        let tool = PassVerdictTool::new();
        let schema = tool.parameters_schema();
        let verdict_enum = schema["properties"]["verdict"]["enum"].as_array().unwrap();
        assert_eq!(verdict_enum.len(), 2);
        assert!(verdict_enum.contains(&serde_json::json!("pass")));
        assert!(verdict_enum.contains(&serde_json::json!("fail")));
    }

    // -- extract_pass_verdict from messages --

    fn make_tool_call_message(tool_name: &str, args_json: &str) -> ChatMessage {
        let content = serde_json::json!({
            "content": "",
            "tool_calls": [{
                "id": "call_1",
                "name": tool_name,
                "arguments": args_json,
            }]
        });
        ChatMessage::assistant(content.to_string())
    }

    #[test]
    fn extract_verdict_pass_from_tool_call() {
        let msg = make_tool_call_message(
            "pass_verdict",
            r#"{"verdict": "pass", "reasoning": "All good"}"#,
        );
        assert_eq!(extract_pass_verdict(&[msg]), Some(true));
    }

    #[test]
    fn extract_verdict_fail_from_tool_call() {
        let msg = make_tool_call_message(
            "pass_verdict",
            r#"{"verdict": "fail", "reasoning": "Missing tests"}"#,
        );
        assert_eq!(extract_pass_verdict(&[msg]), Some(false));
    }

    #[test]
    fn extract_verdict_ignores_other_tools() {
        let msg = make_tool_call_message("shell", r#"{"command": "ls"}"#);
        assert_eq!(extract_pass_verdict(&[msg]), None);
    }

    #[test]
    fn extract_verdict_takes_last_call() {
        let msg1 = make_tool_call_message(
            "pass_verdict",
            r#"{"verdict": "fail", "reasoning": "First pass"}"#,
        );
        let msg2 = make_tool_call_message(
            "pass_verdict",
            r#"{"verdict": "pass", "reasoning": "Revised"}"#,
        );
        // Last message wins (reverse iteration)
        assert_eq!(extract_pass_verdict(&[msg1, msg2]), Some(true));
    }

    #[test]
    fn extract_verdict_none_when_no_messages() {
        assert_eq!(extract_pass_verdict(&[]), None);
    }

    // -- extract_pass_reasoning --

    #[test]
    fn extract_reasoning_from_tool_call() {
        let msg = make_tool_call_message(
            "pass_verdict",
            r#"{"verdict": "fail", "reasoning": "Missing unit tests for auth module"}"#,
        );
        assert_eq!(
            extract_pass_reasoning(&[msg]),
            Some("Missing unit tests for auth module".to_string())
        );
    }

    #[test]
    fn extract_reasoning_none_when_no_tool_call() {
        assert_eq!(extract_pass_reasoning(&[]), None);
    }

    #[test]
    fn extract_reasoning_none_when_empty() {
        let msg = make_tool_call_message("pass_verdict", r#"{"verdict": "pass", "reasoning": ""}"#);
        assert_eq!(extract_pass_reasoning(&[msg]), None);
    }

    // -- resolve_pass_verdict --

    #[test]
    fn resolve_requires_tool_call() {
        let msg =
            make_tool_call_message("pass_verdict", r#"{"verdict": "fail", "reasoning": "Bad"}"#);
        let v = resolve_pass_verdict(&[msg]).unwrap();
        assert!(!v.passed);
        assert_eq!(v.reasoning.as_deref(), Some("Bad"));
    }

    #[test]
    fn resolve_errors_when_tool_call_missing() {
        let err = resolve_pass_verdict(&[]).unwrap_err();
        assert!(
            err.to_string()
                .contains("Agent did not call required pass_verdict tool")
        );
    }
}
