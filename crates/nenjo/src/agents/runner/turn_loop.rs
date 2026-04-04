//! Core agent turn loop — LLM call → tool execution → repeat.
//!
//! This module contains the generic turn loop that drives agent execution.
//! It is independent of Nenjo platform concepts (NATS, streaming, bootstrap).
//! Callers build prompts and pass pre-built messages to [`run()`].

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use nenjo_models::{ChatMessage, ChatRequest};
use nenjo_tools::Tool;

use super::types::{ToolCall, TurnEvent, TurnLoopConfig, TurnOutput};
use crate::agents::instance::AgentInstance;

/// Conservative fallback context window when the provider doesn't report one.
const DEFAULT_CONTEXT_WINDOW: usize = 100_000;

/// Run the agentic turn loop.
///
/// Takes pre-built messages (caller handles prompt construction) and loops:
/// call LLM → if tool calls, execute tools → emit events → repeat.
///
/// Returns [`TurnOutput`] with the final text, token counts, and full
/// conversation messages.
pub async fn run(
    agent: &AgentInstance,
    mut messages: Vec<ChatMessage>,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    pause_token: Option<super::types::PauseToken>,
) -> Result<TurnOutput> {
    let agent_name = &agent.name;
    let provider = &*agent.provider;
    let model = &agent.model;
    let temperature = agent.temperature;
    let tools = &agent.tools;
    let tool_specs = agent.tool_specs();
    let tool_specs = tool_specs.as_slice();
    let config = TurnLoopConfig {
        max_iterations: agent.agent_config.max_tool_iterations as u32,
        parallel_tools: agent.agent_config.parallel_tools,
    };
    let max_iterations = config.max_iterations;

    let mut final_text = String::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_tool_calls: u32 = 0;

    // Log tool specs being sent to the provider (once, before the loop)
    if !tool_specs.is_empty() {
        let tool_names: Vec<&str> = tool_specs.iter().map(|t| t.name.as_str()).collect();
        debug!(
            agent = agent_name,
            model,
            tool_count = tool_specs.len(),
            tools = ?tool_names,
            "Turn loop starting with tools"
        );
    } else {
        warn!(
            agent = agent_name,
            model, "Turn loop starting with NO tools"
        );
    }

    for iteration in 0..max_iterations {
        debug!(
            agent = agent_name,
            iteration,
            messages_count = messages.len(),
            "Turn loop iteration"
        );

        // Resolve the context budget: provider-reported context window with
        // an 80% safety margin. Falls back to 100K if the provider doesn't
        // know the model. Agent config can override.
        let raw_window = provider
            .context_window(model)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let context_budget = raw_window * 4 / 5;

        // Truncate tool arguments in older messages only when we're
        // approaching the context limit (≥80% of budget).  This keeps full
        // arguments available as long as there's headroom, and only starts
        // reclaiming space when pressure is real — preventing the model
        // from seeing (and mimicking) truncation markers prematurely.
        truncate_old_tool_arguments(&mut messages, context_budget);

        // Compact conversation if token estimate still exceeds budget
        // after argument truncation.
        compact_messages(&mut messages, context_budget);

        // Check pause token before each LLM call. If paused, block until
        // resumed. In-flight tool executions finish before we reach this point.
        if let Some(ref pt) = pause_token
            && pt.is_paused()
        {
            let _ = events_tx.as_ref().map(|tx| tx.send(TurnEvent::Paused));
            pt.wait_if_paused().await;
            let _ = events_tx.as_ref().map(|tx| tx.send(TurnEvent::Resumed));
        }

        // Call LLM
        let tools_ref = if tool_specs.is_empty() {
            None
        } else {
            Some(tool_specs)
        };

        let request = ChatRequest {
            messages: &messages,
            tools: tools_ref,
        };

        let response = provider.chat(request, model, temperature).await?;

        // Accumulate token usage
        total_input_tokens += response.usage.input_tokens;
        total_output_tokens += response.usage.output_tokens;

        // Log response summary to diagnose tool-calling issues
        debug!(
            agent = agent_name,
            model,
            iteration,
            has_tool_calls = response.has_tool_calls(),
            tool_call_count = response.tool_calls.len(),
            has_text = response.text.is_some(),
            text_preview = response
                .text
                .as_deref()
                .map(|t| truncate_str(t, 300))
                .unwrap_or("(none)"),
            input_tokens = response.usage.input_tokens,
            output_tokens = response.usage.output_tokens,
            "LLM response received"
        );

        // If the LLM requested tool calls, execute them
        if response.has_tool_calls() {
            // Record the assistant's tool call request as structured JSON so
            // that providers can reconstruct the native assistant-tool-call
            // message on the next iteration.  Both the OpenAI and Anthropic
            // convert_messages helpers look for a `"tool_calls"` key inside the
            // assistant content.
            //
            // Store full arguments here — truncation of older messages is
            // deferred to `truncate_old_tool_arguments()` which runs at the
            // start of each iteration.  This ensures the model always sees
            // its most recent tool calls intact and won't mimic truncation
            // markers as literal content.
            let tool_calls_json: Vec<serde_json::Value> = response
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "name": tc.name,
                        "arguments": tc.arguments,
                    })
                })
                .collect();

            let assistant_content = serde_json::json!({
                "content": response.text.as_deref().unwrap_or(""),
                "tool_calls": tool_calls_json,
            });

            debug!(
                agent = agent_name,
                model, "Tool call response: {assistant_content}"
            );
            messages.push(ChatMessage::assistant(assistant_content.to_string()));

            // Execute tool calls — parallel when the model returns multiple
            // calls in one response (it understands ordering dependencies),
            // sequential otherwise or when opted out via config.
            let run_parallel = config.parallel_tools && response.tool_calls.len() > 1;

            // Emit a single start event with all tool calls.
            let _ = events_tx.as_ref().map(|tx| {
                tx.send(TurnEvent::ToolCallStart {
                    calls: response
                        .tool_calls
                        .iter()
                        .map(|tc| ToolCall {
                            tool_name: tc.name.clone(),
                            tool_args: truncate(&tc.arguments, 120),
                        })
                        .collect(),
                })
            });

            let tool_results: Vec<(&nenjo_models::ToolCall, nenjo_tools::ToolResult)> =
                if run_parallel {
                    let futs = response.tool_calls.iter().map(|tc| async move {
                        let result = execute_tool(agent_name, tools, tc).await;
                        (tc, result)
                    });
                    futures_util::future::join_all(futs).await
                } else {
                    let mut results = Vec::with_capacity(response.tool_calls.len());
                    for tc in &response.tool_calls {
                        let result = execute_tool(agent_name, tools, tc).await;
                        results.push((tc, result));
                    }
                    results
                };

            total_tool_calls += tool_results.len() as u32;

            // Check if any executed tool is terminal (e.g. gate_verdict).
            // Terminal tools signal that the turn loop should stop immediately
            // without feeding the tool result back to the LLM.
            let has_terminal = tool_results.iter().any(|(tc, _)| {
                tools
                    .iter()
                    .find(|t| t.name() == tc.name)
                    .is_some_and(|t| t.is_terminal())
            });

            // Emit result events and build messages in order.
            for (tool_call, tool_result) in &tool_results {
                let _ = events_tx.as_ref().map(|tx| {
                    tx.send(TurnEvent::ToolCallEnd {
                        tool_name: tool_call.name.clone(),
                        result: tool_result.clone(),
                    })
                });

                // Log tool failures so auth issues (e.g. `gh` CLI) are
                // visible in worker logs instead of being silently swallowed.
                if !tool_result.success {
                    let raw_err = tool_result.error.as_deref().unwrap_or("(no error message)");
                    let err_first_line = raw_err.lines().next().unwrap_or(raw_err);
                    warn!(
                        agent = agent_name,
                        model,
                        tool = %tool_call.name,
                        error = err_first_line,
                        "Tool call failed"
                    );
                }

                // Skip pushing tool results when a terminal tool was called —
                // the structured arguments are already captured in the assistant
                // message and no further LLM interaction is needed.
                if has_terminal {
                    continue;
                }

                // Build tool result message with tool_call_id so providers
                // can match each result to its corresponding tool call.
                let raw_content = if tool_result.success {
                    tool_result.output.clone()
                } else {
                    format!(
                        "Error: {}",
                        tool_result.error.as_deref().unwrap_or(&tool_result.output)
                    )
                };

                let tool_content = serde_json::json!({
                    "tool_call_id": tool_call.id,
                    "content": raw_content,
                });
                messages.push(ChatMessage::tool(tool_content.to_string()));
            }

            // Terminal tool: stop the loop. The verdict is already recorded
            // in the assistant message's tool_calls for extraction.
            if has_terminal {
                debug!(
                    agent = agent_name,
                    model, "Terminal tool called, ending turn loop"
                );
                final_text = response.text.as_deref().unwrap_or("").to_string();
                break;
            }

            continue;
        }

        // No tool calls — check if we have a final text response.
        let text = response.text.unwrap_or_default();

        // Empty response (no text, no tool calls) — some models occasionally
        // return these.  Retry instead of treating as final answer.
        if text.trim().is_empty() {
            warn!(
                agent = agent_name,
                model, iteration, "LLM returned empty response (no text, no tool calls), retrying"
            );
            // Push an empty assistant message so the provider sees the turn,
            // then add a nudge so the model tries again.
            messages.push(ChatMessage::assistant(String::new()));
            messages.push(ChatMessage::user(
                "Your previous response was empty. Please respond to the user's request."
                    .to_string(),
            ));
            continue;
        }

        final_text = text;
        break;
    }

    if final_text.is_empty() && max_iterations > 0 {
        warn!(
            agent = agent_name,
            model, "Turn loop reached max iterations without final response"
        );
        final_text = messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "Max iterations reached without a final response.".into());
    }

    let output = TurnOutput {
        text: final_text,
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        tool_calls: total_tool_calls,
        messages,
    };

    let _ = events_tx.as_ref().map(|tx| {
        tx.send(TurnEvent::Done {
            output: output.clone(),
        })
    });

    Ok(output)
}

/// Execute a single tool call against the tool registry.
async fn execute_tool(
    agent_name: &str,
    tools: &[Arc<dyn Tool>],
    tool_call: &nenjo_models::ToolCall,
) -> nenjo_tools::ToolResult {
    info!(
        agent = agent_name,
        tool = %tool_call.name,
        args = %truncate(&tool_call.arguments, 200),
        "Tool call"
    );

    // Find the tool
    let tool = match tools.iter().find(|t| t.name() == tool_call.name) {
        Some(t) => t,
        None => {
            return nenjo_tools::ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown tool: {}", tool_call.name)),
            };
        }
    };

    // Parse arguments
    let args: serde_json::Value = match serde_json::from_str(&tool_call.arguments) {
        Ok(v) => v,
        Err(e) => {
            return nenjo_tools::ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to parse tool arguments: {e}")),
            };
        }
    };

    // Execute
    match tool.execute(args).await {
        Ok(result) => result,
        Err(e) => nenjo_tools::ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Tool execution error: {e}")),
        },
    }
}

/// Estimate total token count across all messages using the chars/4 heuristic.
fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(|m| m.content.len() / 4).sum()
}

/// Progressively compact conversation messages to stay within a token budget.
///
/// Strategy (preserves recent context, compacts old):
/// 1. If under budget, no-op.
/// 2. Phase 1: Truncate old tool-result content (oldest first, skip recent 6).
/// 3. Phase 2: Summarize old assistant tool-call arguments to just tool names.
/// 4. Phase 2.5: Truncate large plain-text assistant messages (artifact content).
/// 5. Phase 3: Drop oldest non-system messages until under budget (keep last 4).
fn compact_messages(messages: &mut Vec<ChatMessage>, max_tokens: usize) {
    if estimate_tokens(messages) <= max_tokens {
        return;
    }

    let len = messages.len();
    // Protect system (index 0) and the most recent messages.
    let protect_tail = 6.min(len.saturating_sub(1));
    let compactable_end = len - protect_tail;

    // Phase 1: Truncate old tool results.
    for i in 1..compactable_end {
        if messages[i].role != "tool" {
            continue;
        }
        if messages[i].content.len() <= 500 {
            continue;
        }
        // Try to preserve the tool_call_id while truncating the content.
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&messages[i].content) {
            if let Some(obj) = parsed.as_object_mut()
                && let Some(content) = obj.get("content").and_then(|v| v.as_str())
            {
                let preview = truncate(content, 200);
                obj.insert(
                    "content".to_string(),
                    serde_json::Value::String(format!(
                        "{preview}\n[compacted — {} chars total]",
                        content.len()
                    )),
                );
                messages[i].content = serde_json::to_string(obj).unwrap_or_default();
            }
        } else {
            // Plain text tool result
            let original_len = messages[i].content.len();
            messages[i].content = format!(
                "{}\n[compacted — {original_len} chars total]",
                truncate(&messages[i].content, 200)
            );
        }

        if estimate_tokens(messages) <= max_tokens {
            return;
        }
    }

    // Phase 2: Summarize old assistant tool-call arguments.
    // Keep tool_calls as a valid JSON array (with arguments truncated) so
    // provider convert_messages can still parse them and match tool results.
    for i in 1..compactable_end {
        if messages[i].role != "assistant" {
            continue;
        }
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&messages[i].content)
            && let Some(calls) = parsed.get("tool_calls").and_then(|v| v.as_array()).cloned()
        {
            if calls.is_empty() {
                continue;
            }
            let summarized_calls: Vec<serde_json::Value> = calls
                .into_iter()
                .map(|mut c| {
                    if let Some(obj) = c.as_object_mut() {
                        obj.insert("arguments".to_string(), serde_json::json!("{}"));
                    }
                    c
                })
                .collect();
            parsed["tool_calls"] = serde_json::Value::Array(summarized_calls);
            messages[i].content = parsed.to_string();

            if estimate_tokens(messages) <= max_tokens {
                return;
            }
        }
    }

    // Phase 2.5: Truncate large plain-text assistant messages (e.g. rendered
    // artifact content from domain sessions).
    for i in 1..compactable_end {
        if messages[i].role != "assistant" {
            continue;
        }
        if messages[i].content.starts_with('{') {
            continue; // JSON = tool-call message, handled by Phase 2
        }
        if messages[i].content.len() <= 600 {
            continue;
        }
        let original_len = messages[i].content.len();
        messages[i].content = format!(
            "{}\n[compacted — {original_len} chars total]",
            truncate(&messages[i].content, 300)
        );
        if estimate_tokens(messages) <= max_tokens {
            return;
        }
    }

    // Phase 3: Drop oldest non-system messages one at a time.
    // When removing an assistant tool-call message, also remove its
    // subsequent tool-result messages to avoid orphaned tool roles.
    let min_keep = 5; // system + at least 4 recent messages
    while messages.len() > min_keep && estimate_tokens(messages) > max_tokens {
        let removed = messages.remove(1); // remove oldest non-system
        // If we removed an assistant message with tool_calls, also remove
        // the following tool-result messages that belong to it.
        if removed.role == "assistant" {
            while messages.len() > min_keep && messages.get(1).is_some_and(|m| m.role == "tool") {
                messages.remove(1);
            }
        }
    }
}

/// Truncate tool call arguments in older assistant messages while
/// preserving recent messages intact.
///
/// Only activates when the estimated token count reaches ≥80% of
/// `max_tokens`.  Below that threshold the full arguments are kept,
/// giving the model maximum context about its own prior actions.
///
/// This prevents the model from seeing truncation markers in its most
/// recent tool calls, which causes weaker models to mimic the markers
/// as literal content (e.g. writing `[2263 chars written]` as file content).
///
/// The last `PROTECT_TAIL` messages are never modified; only older
/// assistant messages outside this window have their tool arguments
/// truncated.
fn truncate_old_tool_arguments(messages: &mut [ChatMessage], max_tokens: usize) {
    // Only kick in when we're at ≥80% of the context budget.
    let threshold = max_tokens * 4 / 5;
    if estimate_tokens(messages) < threshold {
        return;
    }

    // Protect system (index 0) and the most recent messages.
    // 12 messages ≈ 4–6 recent turn-loop iterations (each iteration
    // adds 1 assistant + N tool result messages).
    const PROTECT_TAIL: usize = 12;

    let len = messages.len();
    let protect_tail = PROTECT_TAIL.min(len.saturating_sub(1));
    let compactable_end = len - protect_tail;

    for msg in messages[1..compactable_end].iter_mut() {
        if msg.role != "assistant" {
            continue;
        }
        let mut parsed = match serde_json::from_str::<serde_json::Value>(&msg.content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let calls = match parsed.get("tool_calls").and_then(|v| v.as_array()).cloned() {
            Some(c) => c,
            None => continue,
        };

        let mut changed = false;
        let mut new_calls = Vec::new();
        for call in &calls {
            let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args_str = call.get("arguments").and_then(|a| a.as_str()).unwrap_or("");

            let truncated = truncate_tool_arguments(name, args_str);
            if truncated != args_str {
                changed = true;
            }

            let mut new_call = call.clone();
            if let Some(obj) = new_call.as_object_mut() {
                obj.insert(
                    "arguments".to_string(),
                    serde_json::Value::String(truncated),
                );
            }
            new_calls.push(new_call);
        }

        if changed && let Some(obj) = parsed.as_object_mut() {
            obj.insert(
                "tool_calls".to_string(),
                serde_json::Value::Array(new_calls),
            );
            msg.content = serde_json::to_string(obj).unwrap_or_default();
        }
    }
}

/// Truncate tool call arguments stored in assistant messages to prevent
/// the conversation context from growing unboundedly.
///
/// Tools like `file_write` embed the entire file content in `arguments`,
/// which can be thousands of tokens. The model doesn't need to re-read
/// the full content it wrote — just the tool name and key metadata (path).
fn truncate_tool_arguments(tool_name: &str, arguments: &str) -> String {
    const MAX_ARG_LEN: usize = 500;

    // If arguments are already small, keep them as-is.
    if arguments.len() <= MAX_ARG_LEN {
        return arguments.to_string();
    }

    // Try to parse as JSON so we can surgically truncate large fields.
    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(obj) = parsed.as_object_mut()
    {
        match tool_name {
            "file_write" => {
                // Keep path, replace content with an unambiguous
                // system-level marker.  The «» delimiters and explicit
                // "previously written" phrasing prevent models from
                // reproducing the marker as literal file content.
                if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                    let len = content.len();
                    obj.insert(
                        "content".to_string(),
                        serde_json::Value::String(format!("«previously written — {len} chars»")),
                    );
                }
            }
            "file_edit" => {
                // Truncate old_string and new_string if large.
                for key in &["old_string", "new_string"] {
                    if let Some(val) = obj.get(*key).and_then(|v| v.as_str())
                        && val.len() > 200
                    {
                        let preview = truncate(val, 100);
                        obj.insert(
                            key.to_string(),
                            serde_json::Value::String(format!("«{} chars» {preview}", val.len())),
                        );
                    }
                }
            }
            "shell" => {
                // Keep command, truncate if very long.
                if let Some(cmd) = obj.get("command").and_then(|v| v.as_str())
                    && cmd.len() > 300
                {
                    obj.insert(
                        "command".to_string(),
                        serde_json::Value::String(truncate(cmd, 300)),
                    );
                }
            }
            _ => {
                // Generic: truncate any string value over 300 chars.
                let keys: Vec<String> = obj.keys().cloned().collect();
                for key in keys {
                    if let Some(val) = obj.get(&key).and_then(|v| v.as_str())
                        && val.len() > 300
                    {
                        obj.insert(
                            key,
                            serde_json::Value::String(format!("«{} chars omitted»", val.len())),
                        );
                    }
                }
            }
        }
        return serde_json::to_string(obj).unwrap_or_else(|_| truncate(arguments, MAX_ARG_LEN));
    }

    // Fallback: raw truncation.
    truncate(arguments, MAX_ARG_LEN)
}

/// Truncate a string at a character boundary, returning a `&str` slice.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    &s[..s.floor_char_boundary(max_bytes)]
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    if max_len <= 3 {
        return truncate_str(s, max_len).to_string();
    }
    format!("{}...", truncate_str(s, max_len.saturating_sub(3)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let max_len = 10;
        let result = truncate("hello world this is a long string", max_len);
        assert!(result.ends_with("..."));
        assert_eq!(result.len(), max_len);
    }

    #[test]
    fn tool_call_assistant_message_has_structured_json() {
        // Verify the JSON format that the turn loop would produce for an
        // assistant tool call message — providers parse this to reconstruct
        // native tool call messages.
        let tool_calls = vec![serde_json::json!({
            "id": "call_123",
            "name": "delegate_to",
            "arguments": r#"{"agent_name":"Dev","task":"fix bug"}"#,
        })];
        let assistant_content = serde_json::json!({
            "content": "I'll delegate this.",
            "tool_calls": tool_calls,
        });
        let msg = ChatMessage::assistant(assistant_content.to_string());

        // OpenAI/Anthropic providers parse from the content field
        let parsed: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(parsed["content"], "I'll delegate this.");
        assert!(parsed["tool_calls"].is_array());
        assert_eq!(parsed["tool_calls"][0]["id"], "call_123");
        assert_eq!(parsed["tool_calls"][0]["name"], "delegate_to");
    }

    #[test]
    fn tool_result_message_has_tool_call_id() {
        // Verify the JSON format for tool result messages.
        let tool_content = serde_json::json!({
            "tool_call_id": "call_123",
            "content": "Task completed successfully",
        });
        let msg = ChatMessage::tool(tool_content.to_string());

        let parsed: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(parsed["tool_call_id"], "call_123");
        assert_eq!(parsed["content"], "Task completed successfully");
    }

    #[test]
    fn truncate_tool_arguments_small_passthrough() {
        let args = r#"{"path":"src/main.rs"}"#;
        assert_eq!(truncate_tool_arguments("file_read", args), args);
    }

    #[test]
    fn truncate_tool_arguments_file_write_replaces_content() {
        let big_content = "x".repeat(2000);
        let args = serde_json::json!({
            "path": "src/main.rs",
            "content": big_content,
        });
        let result = truncate_tool_arguments("file_write", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        // Path preserved
        assert_eq!(parsed["path"], "src/main.rs");
        // Content replaced with unambiguous marker
        let content = parsed["content"].as_str().unwrap();
        assert!(
            content.contains("previously written") && content.contains("2000 chars"),
            "Expected unambiguous marker, got: {content}"
        );
        assert!(result.len() < 200);
    }

    #[test]
    fn truncate_tool_arguments_file_edit_truncates_large_strings() {
        let big_old = "a".repeat(500);
        let big_new = "b".repeat(500);
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "old_string": big_old,
            "new_string": big_new,
        });
        let result = truncate_tool_arguments("file_edit", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["path"], "src/lib.rs");
        assert!(parsed["old_string"].as_str().unwrap().contains("500 chars"));
        assert!(parsed["new_string"].as_str().unwrap().contains("500 chars"));
    }

    #[test]
    fn truncate_tool_arguments_generic_caps_large_values() {
        let big_val = "z".repeat(1000);
        let args = serde_json::json!({
            "query": big_val,
        });
        let result = truncate_tool_arguments("content_search", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let query = parsed["query"].as_str().unwrap();
        assert!(
            query.contains("1000 chars") && query.contains("omitted"),
            "Expected unambiguous marker, got: {query}"
        );
    }

    // ── Context window management tests ──────────────────────────────

    #[test]
    fn estimate_tokens_basic() {
        let msgs = vec![
            ChatMessage::system("a]".repeat(200).as_str()), // 400 chars => ~100 tokens
            ChatMessage::user("b".repeat(400).as_str()),    // 400 chars => ~100 tokens
        ];
        let est = estimate_tokens(&msgs);
        assert_eq!(est, 200);
    }

    #[test]
    fn compact_messages_noop_when_under_budget() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
        ];
        let before = msgs.clone();
        compact_messages(&mut msgs, 100_000);
        assert_eq!(msgs.len(), before.len());
        assert_eq!(msgs[0].content, before[0].content);
    }

    /// Helper: build a conversation with several large tool results.
    fn build_large_conversation() -> Vec<ChatMessage> {
        let big_result = "x".repeat(4000); // ~1000 tokens each
        let tool_result = |id: &str, content: &str| -> ChatMessage {
            let json = serde_json::json!({
                "tool_call_id": id,
                "content": content,
            });
            ChatMessage::tool(json.to_string())
        };
        let assistant_tool_call = |id: &str, name: &str| -> ChatMessage {
            let json = serde_json::json!({
                "content": "Let me use a tool.",
                "tool_calls": [{
                    "id": id,
                    "name": name,
                    "arguments": r#"{"path":"src/main.rs"}"#,
                }],
            });
            ChatMessage::assistant(json.to_string())
        };

        vec![
            ChatMessage::system("system prompt"),
            // Old turns (will be compacted)
            ChatMessage::user("do task 1"),
            assistant_tool_call("c1", "file_read"),
            tool_result("c1", &big_result),
            assistant_tool_call("c2", "file_write"),
            tool_result("c2", &big_result),
            assistant_tool_call("c3", "shell"),
            tool_result("c3", &big_result),
            ChatMessage::assistant("done with old work"),
            // Recent turns (should be preserved)
            ChatMessage::user("do task 2"),
            assistant_tool_call("c4", "file_read"),
            tool_result("c4", &big_result),
            ChatMessage::assistant("here is the result"),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("you're welcome"),
        ]
    }

    #[test]
    fn compact_messages_phase1_truncates_old_tool_results() {
        let mut msgs = build_large_conversation();
        let original_len = msgs.len();
        // Set a budget that's too small for the full conversation
        // but large enough that truncating tool results should suffice.
        // Each big result is ~1000 tokens, we have 4 of them = ~4000 tokens.
        // With budget of ~3000 tokens, phase 1 should truncate older ones.
        let tokens_before = estimate_tokens(&msgs);

        // Budget: about 60% of current tokens — phase 1 should handle it.
        let budget = tokens_before * 3 / 5;
        compact_messages(&mut msgs, budget);

        // Message count preserved (phase 1 doesn't remove messages)
        assert_eq!(msgs.len(), original_len);

        // Old tool results (indices 3, 5, 7) should be compacted
        assert!(msgs[3].content.contains("compacted"));
        assert!(msgs[5].content.contains("compacted"));

        // Recent tool result (index 11) should NOT be compacted
        assert!(!msgs[11].content.contains("compacted"));
    }

    #[test]
    fn compact_messages_phase2_summarizes_assistant_tool_calls() {
        // Build a conversation where assistant tool-call messages are large
        // but tool results are small, so phase 1 doesn't help much and
        // phase 2 must kick in.
        let small_result = |id: &str| -> ChatMessage {
            let json = serde_json::json!({
                "tool_call_id": id,
                "content": "ok",
            });
            ChatMessage::tool(json.to_string())
        };
        // Assistant messages with big arguments — these are the heavy ones.
        let big_assistant = |id: &str, name: &str| -> ChatMessage {
            let big_args = "a".repeat(3000);
            let json = serde_json::json!({
                "content": "Let me use a tool.",
                "tool_calls": [{
                    "id": id,
                    "name": name,
                    "arguments": big_args,
                }],
            });
            ChatMessage::assistant(json.to_string())
        };

        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("task"),
            big_assistant("c1", "file_write"),
            small_result("c1"),
            big_assistant("c2", "shell"),
            small_result("c2"),
            big_assistant("c3", "file_read"),
            small_result("c3"),
            ChatMessage::assistant("old summary"),
            // Recent (protected)
            ChatMessage::user("next task"),
            big_assistant("c4", "file_read"),
            small_result("c4"),
            ChatMessage::assistant("recent result"),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("welcome"),
        ];

        // Budget tight enough that phase 1 (small results) won't help,
        // but phase 2 (summarize assistant tool calls) should.
        let tokens_before = estimate_tokens(&msgs);
        let budget = tokens_before * 2 / 5;
        compact_messages(&mut msgs, budget);

        // Check that at least one old assistant message had its arguments truncated
        // while keeping tool_calls as a valid parseable array.
        let has_summarized = msgs.iter().any(|m| {
            if m.role != "assistant" {
                return false;
            }
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&m.content)
                && let Some(calls) = parsed.get("tool_calls").and_then(|v| v.as_array())
            {
                return calls
                    .iter()
                    .any(|c| c.get("arguments").and_then(|a| a.as_str()) == Some("{}"));
            }
            false
        });
        assert!(
            has_summarized,
            "Phase 2 should truncate arguments in old assistant tool calls"
        );

        // System preserved
        assert_eq!(msgs[0].role, "system");
    }

    #[test]
    fn compact_messages_phase3_drops_oldest() {
        let mut msgs = build_large_conversation();
        // Extremely tight budget: force into phase 3
        compact_messages(&mut msgs, 50);

        // System message always preserved
        assert_eq!(msgs[0].role, "system");
        // At least min_keep (5) messages remain
        assert!(msgs.len() >= 5);
        // Last message preserved
        assert_eq!(msgs.last().unwrap().content, "you're welcome");
    }

    #[test]
    fn compact_messages_preserves_system_and_recent() {
        let mut msgs = build_large_conversation();
        let last_content = msgs.last().unwrap().content.clone();
        let system_content = msgs[0].content.clone();

        compact_messages(&mut msgs, 100); // very tight

        assert_eq!(msgs[0].content, system_content);
        assert_eq!(msgs.last().unwrap().content, last_content);
    }

    #[test]
    fn compact_messages_phase2_5_truncates_large_assistant_text() {
        // Build a conversation where assistant messages contain large plain
        // text (e.g. rendered artifact content from domain sessions), but tool
        // results are small so phase 1 won't help, and the assistant messages
        // don't parse as JSON tool calls so phase 2 won't help either.
        let big_text = "This is a large artifact document. ".repeat(200); // ~6800 chars
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("create a PRD"),
            ChatMessage::assistant(&big_text), // large plain-text assistant msg
            ChatMessage::user("update section 2"),
            ChatMessage::assistant(&big_text), // another large one
            // Recent (protected tail = 6, so these are safe)
            ChatMessage::user("looks good"),
            ChatMessage::assistant("Great, glad you like it."),
            ChatMessage::user("any more changes?"),
            ChatMessage::assistant("No, we're done."),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("You're welcome!"),
        ];

        let tokens_before = estimate_tokens(&msgs);
        // Budget: enough that phases 1 & 2 alone won't fix it (they target
        // tool results / JSON tool calls, neither of which exist here).
        let budget = tokens_before * 2 / 5;
        compact_messages(&mut msgs, budget);

        // The old large assistant messages (indices 2, 4) should be compacted
        assert!(
            msgs[2].content.contains("compacted"),
            "Phase 2.5 should truncate large plain-text assistant message at index 2"
        );
        assert!(
            msgs[4].content.contains("compacted"),
            "Phase 2.5 should truncate large plain-text assistant message at index 4"
        );

        // Recent assistant messages should NOT be compacted
        let last = msgs.last().unwrap();
        assert!(
            !last.content.contains("compacted"),
            "Recent assistant messages should be preserved"
        );
    }

    // ── Deferred tool argument truncation tests ─────────────────────

    /// Helper: build a conversation with large file_write arguments that
    /// pushes token count high enough to trigger the 80% threshold.
    fn build_conversation_near_limit(_budget: usize) -> Vec<ChatMessage> {
        // Each file_write argument is ~2000 chars ≈ 500 tokens.
        // We want total tokens to be ≥80% of budget.
        let big_content = "x".repeat(2000);
        let assistant_write = |id: &str, content: &str| -> ChatMessage {
            let args = serde_json::json!({
                "path": format!("src/{id}.rs"),
                "content": content,
            });
            let json = serde_json::json!({
                "content": "",
                "tool_calls": [{
                    "id": id,
                    "name": "file_write",
                    "arguments": args.to_string(),
                }],
            });
            ChatMessage::assistant(json.to_string())
        };
        let tool_result = |id: &str| -> ChatMessage {
            let json = serde_json::json!({
                "tool_call_id": id,
                "content": "ok",
            });
            ChatMessage::tool(json.to_string())
        };

        let mut msgs = vec![ChatMessage::system("system prompt")];

        // Add enough old turns to approach the budget
        for i in 0..20 {
            let id = format!("old_{i}");
            msgs.push(assistant_write(&id, &big_content));
            msgs.push(tool_result(&id));
        }

        // Add recent turns (these should be protected)
        for i in 0..6 {
            let id = format!("recent_{i}");
            msgs.push(assistant_write(&id, &big_content));
            msgs.push(tool_result(&id));
        }

        msgs
    }

    #[test]
    fn truncate_old_tool_args_noop_when_under_threshold() {
        let big_content = "x".repeat(2000);
        let args = serde_json::json!({
            "path": "src/main.rs",
            "content": big_content,
        });
        let json = serde_json::json!({
            "content": "",
            "tool_calls": [{
                "id": "c1",
                "name": "file_write",
                "arguments": args.to_string(),
            }],
        });
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant(json.to_string()),
            ChatMessage::tool(r#"{"tool_call_id":"c1","content":"ok"}"#),
            ChatMessage::user("next"),
        ];
        let before = msgs[1].content.clone();

        // Huge budget — well under 80% threshold
        truncate_old_tool_arguments(&mut msgs, 1_000_000);

        // Nothing should have changed
        assert_eq!(msgs[1].content, before);
    }

    #[test]
    fn truncate_old_tool_args_truncates_old_preserves_recent() {
        // Use a budget that makes the conversation exceed the 80% threshold
        let mut msgs = build_conversation_near_limit(5000);
        let tokens = estimate_tokens(&msgs);
        // Set budget so we're above the 80% threshold
        let budget = tokens; // tokens == budget means we're at 100%, well above 80%

        // Snapshot the last 12 messages (recent, should be protected)
        let len = msgs.len();
        let recent_snapshot: Vec<String> =
            msgs[len - 12..].iter().map(|m| m.content.clone()).collect();

        truncate_old_tool_arguments(&mut msgs, budget);

        // Recent messages (last 12) should be unchanged
        let recent_after: Vec<String> =
            msgs[len - 12..].iter().map(|m| m.content.clone()).collect();
        assert_eq!(recent_snapshot, recent_after);

        // Old assistant messages (outside protected tail) should be truncated
        let old_assistant = &msgs[1]; // first assistant message, definitely old
        assert!(
            old_assistant.content.contains("previously written"),
            "Old file_write should have been truncated with marker, got: {}",
            &old_assistant.content[..200.min(old_assistant.content.len())]
        );
    }

    #[test]
    fn truncate_old_tool_args_markers_are_unambiguous() {
        // Verify the markers can't be confused with real content
        let big_content = "function hello() { return 'world'; }".repeat(100);
        let args = serde_json::json!({
            "path": "src/main.rs",
            "content": big_content,
        });
        let result = truncate_tool_arguments("file_write", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let marker = parsed["content"].as_str().unwrap();

        // Marker should use « » delimiters
        assert!(
            marker.starts_with('«'),
            "Marker should start with «: {marker}"
        );
        assert!(marker.ends_with('»'), "Marker should end with »: {marker}");
        // Marker should NOT look like code or normal text
        assert!(
            !marker.contains("function") && !marker.contains("return"),
            "Marker should not contain original content"
        );
    }
}
