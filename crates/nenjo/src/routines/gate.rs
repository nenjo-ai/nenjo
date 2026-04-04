//! Gate verdict tool — deterministic pass/fail signal for gate steps.
//!
//! Instead of parsing free-form LLM text for keywords like "pass" or "fail",
//! the gate agent calls this tool with a structured verdict. This eliminates
//! ambiguity from natural language (e.g. "I'll pass this along" being
//! misinterpreted as a pass verdict).

use std::sync::Arc;

use anyhow::Result;
use nenjo_models::ChatMessage;
use nenjo_tools::{Tool, ToolCategory, ToolResult};
use tracing::warn;

/// Tool name constant used for injection and extraction.
pub const GATE_VERDICT_TOOL_NAME: &str = "gate_verdict";

// ---------------------------------------------------------------------------
// GateVerdictTool
// ---------------------------------------------------------------------------

/// A tool that gate agents call to submit their pass/fail verdict.
///
/// The tool's `execute()` is a no-op — the real value is in the structured
/// arguments captured by the turn loop. After execution completes,
/// [`extract_gate_verdict`] reads the verdict from the conversation messages.
pub struct GateVerdictTool;

impl Default for GateVerdictTool {
    fn default() -> Self {
        Self
    }
}

impl GateVerdictTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for GateVerdictTool {
    fn name(&self) -> &str {
        GATE_VERDICT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Submit your final verdict for this gate evaluation. You MUST call this tool exactly once \
         as the last action in your evaluation. Use verdict \"pass\" if the output meets the \
         acceptance criteria, or \"fail\" if it does not."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "verdict": {
                    "type": "string",
                    "enum": ["pass", "fail"],
                    "description": "The gate verdict: \"pass\" if criteria are met, \"fail\" if not"
                },
                "reasoning": {
                    "type": "string",
                    "description": "Brief explanation of why the output passes or fails the acceptance criteria"
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
            output: format!("Gate verdict recorded: {verdict}. {reasoning}"),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Verdict extraction from conversation messages
// ---------------------------------------------------------------------------

/// Extract a gate verdict from the conversation messages by looking for a
/// `gate_verdict` tool call. Returns `None` if the tool was never called.
pub fn extract_gate_verdict(messages: &[ChatMessage]) -> Option<bool> {
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
            if name != GATE_VERDICT_TOOL_NAME {
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

// ---------------------------------------------------------------------------
// Fallback: parse verdict from free-form text (JSON then keywords)
// ---------------------------------------------------------------------------

/// Parse a gate verdict from LLM output text. Tries structured JSON first,
/// then falls back to keyword matching. Returns `None` if no signal is found.
fn parse_gate_verdict_from_text(output: &str) -> Option<bool> {
    // Try JSON: {"verdict": "pass"} or {"verdict": "fail"}
    for candidate in extract_json_candidates(output) {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&candidate)
            && let Some(verdict) = obj.get("verdict").and_then(|v| v.as_str())
        {
            return match verdict.to_lowercase().as_str() {
                "pass" => Some(true),
                "fail" => Some(false),
                _ => None,
            };
        }
    }
    None
}

/// Extract potential JSON strings from text — first from code fences, then bare `{...}`.
fn extract_json_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // 1. Look inside ```json ... ``` or ``` ... ``` fences
    let mut search = text;
    while let Some(fence_start) = search.find("```") {
        let after_fence = &search[fence_start + 3..];
        // Skip optional language tag (e.g., "json")
        let content_start = after_fence.find('\n').map(|i| i + 1).unwrap_or(0);
        if let Some(fence_end) = after_fence[content_start..].find("```") {
            let content = after_fence[content_start..content_start + fence_end].trim();
            if content.starts_with('{') {
                candidates.push(content.to_string());
            }
            search = &after_fence[content_start + fence_end + 3..];
        } else {
            break;
        }
    }

    // 2. Fallback: find bare JSON objects `{ ... }`
    let mut depth = 0i32;
    let mut obj_start = None;
    for (i, ch) in text.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    obj_start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = obj_start {
                        let s = &text[start..=i];
                        if !candidates.iter().any(|c| c == s.trim()) {
                            candidates.push(s.to_string());
                        }
                    }
                    obj_start = None;
                }
            }
            _ => {}
        }
    }

    candidates
}

/// Structured gate verdict with reasoning.
#[derive(Debug, Clone)]
pub struct GateVerdict {
    pub passed: bool,
    pub reasoning: Option<String>,
}

/// Extract the reasoning string from a `gate_verdict` tool call in the
/// conversation messages. Returns `None` if the tool was never called.
pub fn extract_gate_reasoning(messages: &[ChatMessage]) -> Option<String> {
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
            if name != GATE_VERDICT_TOOL_NAME {
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

/// Resolve a gate verdict from conversation messages, with fallbacks.
///
/// Priority:
/// 1. `gate_verdict` tool call args (deterministic)
/// 2. JSON `{"verdict": "pass"|"fail"}` in the final text
/// 3. Default to `true` (pass) — a gate that produces no signal shouldn't block
///
/// Returns a [`GateVerdict`] with both the pass/fail result and the
/// agent's reasoning (if available from the tool call).
pub fn resolve_gate_verdict(messages: &[ChatMessage], final_text: &str) -> GateVerdict {
    // 1. Tool call — most reliable
    if let Some(passed) = extract_gate_verdict(messages) {
        let reasoning = extract_gate_reasoning(messages);
        return GateVerdict { passed, reasoning };
    }

    warn!("Gate agent did not call gate_verdict tool, falling back to text parsing");

    // 2. Structured JSON in text
    if let Some(passed) = parse_gate_verdict_from_text(final_text) {
        return GateVerdict {
            passed,
            reasoning: None,
        };
    }

    warn!("No structured verdict found in gate response, defaulting to pass");
    GateVerdict {
        passed: true,
        reasoning: None,
    }
}

// ---------------------------------------------------------------------------
// Convenience: create the tool as Arc<dyn Tool>
// ---------------------------------------------------------------------------

/// Create the gate verdict tool wrapped in an Arc for injection into agent tools.
pub fn gate_verdict_tool() -> Arc<dyn Tool> {
    Arc::new(GateVerdictTool::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- GateVerdictTool schema --

    #[test]
    fn tool_has_correct_name() {
        let tool = GateVerdictTool::new();
        assert_eq!(tool.name(), "gate_verdict");
    }

    #[test]
    fn tool_schema_has_required_fields() {
        let tool = GateVerdictTool::new();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("verdict")));
        assert!(required.contains(&serde_json::json!("reasoning")));
    }

    #[test]
    fn tool_schema_verdict_is_enum() {
        let tool = GateVerdictTool::new();
        let schema = tool.parameters_schema();
        let verdict_enum = schema["properties"]["verdict"]["enum"].as_array().unwrap();
        assert_eq!(verdict_enum.len(), 2);
        assert!(verdict_enum.contains(&serde_json::json!("pass")));
        assert!(verdict_enum.contains(&serde_json::json!("fail")));
    }

    // -- extract_gate_verdict from messages --

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
            "gate_verdict",
            r#"{"verdict": "pass", "reasoning": "All good"}"#,
        );
        assert_eq!(extract_gate_verdict(&[msg]), Some(true));
    }

    #[test]
    fn extract_verdict_fail_from_tool_call() {
        let msg = make_tool_call_message(
            "gate_verdict",
            r#"{"verdict": "fail", "reasoning": "Missing tests"}"#,
        );
        assert_eq!(extract_gate_verdict(&[msg]), Some(false));
    }

    #[test]
    fn extract_verdict_ignores_other_tools() {
        let msg = make_tool_call_message("shell", r#"{"command": "ls"}"#);
        assert_eq!(extract_gate_verdict(&[msg]), None);
    }

    #[test]
    fn extract_verdict_takes_last_call() {
        let msg1 = make_tool_call_message(
            "gate_verdict",
            r#"{"verdict": "fail", "reasoning": "First pass"}"#,
        );
        let msg2 = make_tool_call_message(
            "gate_verdict",
            r#"{"verdict": "pass", "reasoning": "Revised"}"#,
        );
        // Last message wins (reverse iteration)
        assert_eq!(extract_gate_verdict(&[msg1, msg2]), Some(true));
    }

    #[test]
    fn extract_verdict_none_when_no_messages() {
        assert_eq!(extract_gate_verdict(&[]), None);
    }

    // -- parse_gate_verdict_from_text (JSON fallback) --

    #[test]
    fn text_verdict_json_pass() {
        let text = r#"After review, here is my verdict: ```json
{"verdict": "pass"}
```"#;
        assert_eq!(parse_gate_verdict_from_text(text), Some(true));
    }

    #[test]
    fn text_verdict_json_fail() {
        let text = r#"The code has issues. {"verdict": "fail"}"#;
        assert_eq!(parse_gate_verdict_from_text(text), Some(false));
    }

    #[test]
    fn text_verdict_no_json_returns_none() {
        let text = "This looks great, I approve!";
        assert_eq!(parse_gate_verdict_from_text(text), None);
    }

    #[test]
    fn text_verdict_json_without_verdict_field() {
        let text = r#"{"status": "ok", "result": "pass"}"#;
        assert_eq!(parse_gate_verdict_from_text(text), None);
    }

    // -- extract_gate_reasoning --

    #[test]
    fn extract_reasoning_from_tool_call() {
        let msg = make_tool_call_message(
            "gate_verdict",
            r#"{"verdict": "fail", "reasoning": "Missing unit tests for auth module"}"#,
        );
        assert_eq!(
            extract_gate_reasoning(&[msg]),
            Some("Missing unit tests for auth module".to_string())
        );
    }

    #[test]
    fn extract_reasoning_none_when_no_tool_call() {
        assert_eq!(extract_gate_reasoning(&[]), None);
    }

    #[test]
    fn extract_reasoning_none_when_empty() {
        let msg = make_tool_call_message("gate_verdict", r#"{"verdict": "pass", "reasoning": ""}"#);
        assert_eq!(extract_gate_reasoning(&[msg]), None);
    }

    // -- resolve_gate_verdict (full fallback chain) --

    #[test]
    fn resolve_prefers_tool_call_over_text() {
        let msg =
            make_tool_call_message("gate_verdict", r#"{"verdict": "fail", "reasoning": "Bad"}"#);
        // Text says pass, tool says fail — tool wins
        let text = r#"{"verdict": "pass"}"#;
        let v = resolve_gate_verdict(&[msg], text);
        assert!(!v.passed);
        assert_eq!(v.reasoning.as_deref(), Some("Bad"));
    }

    #[test]
    fn resolve_falls_back_to_text_json() {
        let text = r#"My analysis: {"verdict": "fail"}"#;
        let v = resolve_gate_verdict(&[], text);
        assert!(!v.passed);
        assert!(v.reasoning.is_none());
    }

    #[test]
    fn resolve_defaults_to_pass() {
        let v = resolve_gate_verdict(&[], "Looks good to me!");
        assert!(v.passed);
    }
}
