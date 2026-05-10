//! Core agent turn loop — LLM call → tool execution → repeat.
//!
//! This module contains the generic turn loop that drives agent execution.
//! It is independent of Nenjo platform concepts (NATS, streaming, bootstrap).
//! Callers build prompts and pass pre-built messages to [`run()`].

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use anyhow::Result;
use regex::Regex;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use super::compaction::{
    compact_messages_with_summary, truncate, truncate_old_tool_arguments, truncate_str,
};
use super::types::{ToolCall, TurnEvent, TurnLoopConfig, TurnOutput};
use crate::agents::instance::AgentInstance;
use crate::tools::{Tool, ToolCategory, ToolResult};
use nenjo_models::{ChatMessage, ChatRequest};

fn dedupe_tool_calls(tool_calls: Vec<nenjo_models::ToolCall>) -> Vec<nenjo_models::ToolCall> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(tool_calls.len());
    for tool_call in tool_calls {
        let key = (tool_call.name.clone(), tool_call.arguments.clone());
        if seen.insert(key) {
            deduped.push(tool_call);
        }
    }
    deduped
}

fn tool_for_call<'a>(
    tools: &'a [Arc<dyn Tool>],
    tool_call: &nenjo_models::ToolCall,
) -> Option<&'a Arc<dyn Tool>> {
    tools.iter().find(|t| {
        let name = t.name();
        name == tool_call.name
            || nenjo_models::sanitize_tool_name(name) == tool_call.name
            || nenjo_models::sanitize_tool_name_lenient(name) == tool_call.name
    })
}

fn emit_event(events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>, event: TurnEvent) {
    if let Some(tx) = events_tx {
        let _ = tx.send(event);
    }
}

tokio::task_local! {
    static CURRENT_EVENTS_TX: Option<mpsc::UnboundedSender<TurnEvent>>;
}

tokio::task_local! {
    static CURRENT_CHAT_HISTORY: Vec<ChatMessage>;
}

#[derive(Default)]
struct NestedTokenUsage {
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    run_depth: AtomicU32,
}

tokio::task_local! {
    static CURRENT_NESTED_TOKEN_USAGE: Arc<NestedTokenUsage>;
}

pub(crate) fn current_events_tx() -> Option<mpsc::UnboundedSender<TurnEvent>> {
    CURRENT_EVENTS_TX.try_with(Clone::clone).ok().flatten()
}

pub(crate) fn current_chat_history() -> Option<Vec<ChatMessage>> {
    CURRENT_CHAT_HISTORY.try_with(Clone::clone).ok()
}

pub(crate) fn record_nested_token_usage(input_tokens: u64, output_tokens: u64) {
    if input_tokens == 0 && output_tokens == 0 {
        return;
    }

    if let Ok(usage) = CURRENT_NESTED_TOKEN_USAGE.try_with(Clone::clone) {
        usage
            .input_tokens
            .fetch_add(input_tokens, Ordering::Relaxed);
        usage
            .output_tokens
            .fetch_add(output_tokens, Ordering::Relaxed);
    }
}

/// Conservative fallback context window when the provider doesn't report one.
const DEFAULT_CONTEXT_WINDOW: usize = 100_000;

fn sanitize_tool_text_preview(text: &str) -> Option<String> {
    static XML_TAG_RE: OnceLock<Regex> = OnceLock::new();
    let xml_tag_re = XML_TAG_RE.get_or_init(|| {
        Regex::new(r"</?[A-Za-z][A-Za-z0-9:_-]*[^>]*>").expect("xml tag regex must be valid")
    });

    let without_tags = xml_tag_re.replace_all(text, " ");
    let collapsed = without_tags
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = collapsed.trim();

    if cleaned.is_empty() {
        None
    } else {
        Some(truncate_str(cleaned, 240).to_string())
    }
}

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
        max_turns: agent.agent_config.max_turns as u32,
        parallel_tools: agent.agent_config.parallel_tools,
    };
    let max_turns = config.max_turns;

    let mut final_text = String::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_tool_calls: u32 = 0;

    let nested_usage = CURRENT_NESTED_TOKEN_USAGE
        .try_with(Clone::clone)
        .unwrap_or_else(|_| Arc::new(NestedTokenUsage::default()));
    let run_depth = nested_usage.run_depth.fetch_add(1, Ordering::Relaxed) + 1;
    let nested_input_baseline = nested_usage.input_tokens.load(Ordering::Relaxed);
    let nested_output_baseline = nested_usage.output_tokens.load(Ordering::Relaxed);

    let run_result = CURRENT_NESTED_TOKEN_USAGE
        .scope(nested_usage.clone(), async {
            CURRENT_EVENTS_TX
                .scope(events_tx.clone(), async {
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
                if tracing::enabled!(tracing::Level::TRACE) {
                    let tool_names = tool_specs
                        .iter()
                        .map(|tool| tool.name.as_str())
                        .collect::<Vec<_>>()
                        .join("\n- ");
                    trace!(
                        agent = agent_name,
                        model,
                        tool_count = tool_specs.len(),
                        "\nTool belt sent to provider for {}:\n- {}",
                        agent_name,
                        tool_names,
                    );
                }
            } else {
                warn!(
                    agent = agent_name,
                    model, "Turn loop starting with NO tools"
                );
            }

            for iteration in 0..max_turns {
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
                // approaching the configured compaction threshold. This keeps full
                // arguments available as long as there's headroom, and only starts
                // reclaiming space when pressure is real — preventing the model
                // from seeing (and mimicking) truncation markers prematurely.
                truncate_old_tool_arguments(
                    &mut messages,
                    context_budget,
                    agent.agent_config.context_compaction_trigger_percent,
                );
                // Compact conversation if token estimate still exceeds budget
                // after argument truncation.
                compact_messages_with_summary(
                    provider,
                    model,
                    temperature,
                    &mut messages,
                    context_budget,
                    events_tx.as_ref(),
                )
                .await?;

                // Check pause token before each LLM call. If paused, block until
                // resumed. In-flight tool executions finish before we reach this point.
                if let Some(ref pt) = pause_token
                    && pt.is_paused()
                {
                    emit_event(events_tx.as_ref(), TurnEvent::Paused);
                    pt.wait_if_paused().await;
                    emit_event(events_tx.as_ref(), TurnEvent::Resumed);
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

                let mut response = provider.chat(request, model, temperature).await?;
                let original_tool_call_count = response.tool_calls.len();
                response.tool_calls = dedupe_tool_calls(response.tool_calls);
                if response.tool_calls.len() != original_tool_call_count {
                    warn!(
                        agent = agent_name,
                        model,
                        original_tool_call_count,
                        deduped_tool_call_count = response.tool_calls.len(),
                        "Deduped repeated tool calls from a single LLM response"
                    );
                }

                // Strip <think>…</think> blocks from reasoning models
                // (DeepSeek, MiniMax, etc.) before text enters messages or NATS.
                if let Some(ref text) = response.text {
                    let stripped = nenjo_models::strip_thinking(text);
                    response.text = if stripped.is_empty() {
                        None
                    } else {
                        Some(stripped)
                    };
                }

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
                        model,
                        tool_call_count = tool_calls_json.len(),
                        assistant_text_len = response.text.as_deref().map(str::len).unwrap_or(0),
                        assistant_text_preview = response
                            .text
                            .as_deref()
                            .map(|text| truncate_str(text, 300))
                            .unwrap_or("(none)"),
                        tool_calls = %serde_json::Value::Array(tool_calls_json.clone()),
                        "LLM requested tool calls"
                    );
                    let assistant_message = ChatMessage::assistant(assistant_content.to_string());
                    messages.push(assistant_message.clone());
                    emit_event(
                        events_tx.as_ref(),
                        TurnEvent::TranscriptMessage {
                            message: assistant_message,
                        },
                    );

                    // Execute tool calls — parallel when the model returns multiple
                    // calls in one response (it understands ordering dependencies),
                    // sequential otherwise or when opted out via config.
                    let has_write_like_tool = response.tool_calls.iter().any(|tc| {
                        tool_for_call(tools, tc)
                            .map(|tool| tool.category() != ToolCategory::Read)
                            .unwrap_or(true)
                    });
                    let run_parallel = config.parallel_tools
                        && response.tool_calls.len() > 1
                        && !has_write_like_tool;
                    if response.tool_calls.len() > 1 && has_write_like_tool {
                        debug!(
                            agent = agent_name,
                            model,
                            tool_call_count = response.tool_calls.len(),
                            "Serializing tool execution because the batch contains WRITE or READ/WRITE tools"
                        );
                    }
                    let tool_text_preview = response
                        .text
                        .as_deref()
                        .and_then(sanitize_tool_text_preview);

                    // Emit a single start event with all tool calls.
                    emit_event(
                        events_tx.as_ref(),
                        TurnEvent::ToolCallStart {
                            parent_tool_name: None,
                            calls: response
                                .tool_calls
                                .iter()
                                .map(|tc| ToolCall {
                                    tool_call_id: Some(tc.id.clone()),
                                    tool_name: tc.name.clone(),
                                    tool_args: truncate(&tc.arguments, 120),
                                    text_preview: tool_text_preview.clone(),
                                })
                                .collect(),
                        },
                    );

                    let tool_results: Vec<(&nenjo_models::ToolCall, ToolResult)> =
                        if run_parallel {
                            let message_snapshot = messages.clone();
                            let futs = response.tool_calls.iter().map(|tc| {
                                let current_messages = message_snapshot.clone();
                                async move {
                                    let result =
                                        execute_tool(agent_name, tools, tc, &current_messages)
                                            .await;
                                    (tc, result)
                                }
                            });
                            futures_util::future::join_all(futs).await
                        } else {
                            let mut results = Vec::with_capacity(response.tool_calls.len());
                            for tc in &response.tool_calls {
                                let result = execute_tool(agent_name, tools, tc, &messages).await;
                                results.push((tc, result));
                            }
                            results
                        };

                    total_tool_calls += tool_results.len() as u32;

                    // Check if any executed tool is terminal (e.g. pass_verdict).
                    // Terminal tools signal that the turn loop should stop immediately
                    // without feeding the tool result back to the LLM.
                    let has_terminal = tool_results.iter().any(|(tc, _)| {
                        tool_for_call(tools, tc)
                            .is_some_and(|t| t.is_terminal())
                    });

                    // Emit result events and build messages in order.
                    for (tool_call, tool_result) in &tool_results {
                        emit_event(
                            events_tx.as_ref(),
                            TurnEvent::ToolCallEnd {
                                parent_tool_name: None,
                                tool_call_id: Some(tool_call.id.clone()),
                                tool_name: tool_call.name.clone(),
                                tool_args: truncate(&tool_call.arguments, 120),
                                result: tool_result.clone(),
                            },
                        );

                        // Log tool failures so auth issues (e.g. `gh` CLI) are
                        // visible in worker logs instead of being silently swallowed.
                        if !tool_result.success {
                            let raw_err =
                                tool_result.error.as_deref().unwrap_or("(no error message)");
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

                        debug!(
                            agent = agent_name,
                            model,
                            tool = %tool_call.name,
                            tool_call_id = %tool_call.id,
                            success = tool_result.success,
                            response_len = raw_content.len(),
                            response_preview = %truncate(&raw_content, 500),
                            "Tool call response"
                        );

                        let tool_content = serde_json::json!({
                            "tool_call_id": tool_call.id,
                            "content": raw_content,
                        });
                        let tool_message = ChatMessage::tool(tool_content.to_string());
                        messages.push(tool_message.clone());
                        emit_event(
                            events_tx.as_ref(),
                            TurnEvent::TranscriptMessage {
                                message: tool_message,
                            },
                        );
                    }

                    // Terminal tool: stop the loop. The verdict is already recorded
                    // in the assistant message's tool_calls for extraction.
                    if has_terminal {
                        debug!(
                            agent = agent_name,
                            model, "Terminal tool called, ending turn loop"
                        );
                        let terminal_tool_text = tool_results
                            .iter()
                            .find(|(tc, _)| {
                                tool_for_call(tools, tc).is_some_and(|t| t.is_terminal())
                            })
                            .map(|(_, result)| {
                                if result.success {
                                    result.output.clone()
                                } else {
                                    result
                                        .error
                                        .clone()
                                        .unwrap_or_else(|| result.output.clone())
                                }
                            })
                            .unwrap_or_default();
                        final_text = response
                            .text
                            .as_deref()
                            .filter(|text| !text.trim().is_empty())
                            .map(ToOwned::to_owned)
                            .unwrap_or(terminal_tool_text);
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
                        model,
                        iteration,
                        "LLM returned empty response (no text, no tool calls), retrying"
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

                final_text = text.clone();
                let assistant_message = ChatMessage::assistant(text);
                messages.push(assistant_message.clone());
                emit_event(
                    events_tx.as_ref(),
                    TurnEvent::TranscriptMessage {
                        message: assistant_message,
                    },
                );
                break;
            }

            if final_text.is_empty() && max_turns > 0 {
                warn!(
                    agent = agent_name,
                    model,
                    max_turns,
                    "Turn loop reached max turns without final response"
                );
                final_text = messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.clone())
                    .unwrap_or_else(|| "Max iterations reached without a final response.".into());
            }

            if run_depth == 1 {
                total_input_tokens += nested_usage
                    .input_tokens
                    .load(Ordering::Relaxed)
                    .saturating_sub(nested_input_baseline);
                total_output_tokens += nested_usage
                    .output_tokens
                    .load(Ordering::Relaxed)
                    .saturating_sub(nested_output_baseline);
            }

            let output = TurnOutput {
                text: final_text,
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                tool_calls: total_tool_calls,
                messages,
            };

            emit_event(
                events_tx.as_ref(),
                TurnEvent::Done {
                    output: output.clone(),
                },
            );

            Ok(output)
                })
                .await
        })
        .await;

    nested_usage.run_depth.fetch_sub(1, Ordering::Relaxed);
    run_result
}

/// Execute a single tool call against the tool registry.
async fn execute_tool(
    agent_name: &str,
    tools: &[Arc<dyn Tool>],
    tool_call: &nenjo_models::ToolCall,
    current_messages: &[ChatMessage],
) -> ToolResult {
    info!(
        agent = agent_name,
        tool = %tool_call.name,
        args = %truncate(&tool_call.arguments, 200),
        "Executing tool call"
    );

    // Find the tool — also match against sanitized names since strict providers
    // (DeepSeek, OpenAI) replace dots/slashes (e.g. "app.nenjo.platform/x" → "app_nenjo_platform_x").
    let tool = match tool_for_call(tools, tool_call) {
        Some(t) => t,
        None => {
            return ToolResult {
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
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to parse tool arguments: {e}")),
            };
        }
    };

    // Execute
    let execute = async {
        match tool.execute(args).await {
            Ok(result) => result,
            Err(e) => ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Tool execution error: {e}")),
            },
        }
    };

    let current_history: Vec<ChatMessage> = current_messages
        .iter()
        .filter(|msg| msg.role != "system" && msg.role != "developer")
        .cloned()
        .collect();

    if let Some(tx) = current_events_tx().or_else(|| None) {
        CURRENT_EVENTS_TX
            .scope(
                Some(tx),
                CURRENT_CHAT_HISTORY.scope(current_history, execute),
            )
            .await
    } else {
        CURRENT_EVENTS_TX
            .scope(None, CURRENT_CHAT_HISTORY.scope(current_history, execute))
            .await
    }
}
