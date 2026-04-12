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

use super::compaction::{
    compact_messages_with_summary, truncate, truncate_old_tool_arguments, truncate_str,
};
use super::types::{ToolCall, TurnEvent, TurnLoopConfig, TurnOutput};
use crate::agents::instance::AgentInstance;

tokio::task_local! {
    static CURRENT_EVENTS_TX: Option<mpsc::UnboundedSender<TurnEvent>>;
}

tokio::task_local! {
    static CURRENT_CHAT_HISTORY: Vec<ChatMessage>;
}

pub(crate) fn current_events_tx() -> Option<mpsc::UnboundedSender<TurnEvent>> {
    CURRENT_EVENTS_TX.try_with(Clone::clone).ok().flatten()
}

pub(crate) fn current_chat_history() -> Option<Vec<ChatMessage>> {
    CURRENT_CHAT_HISTORY.try_with(Clone::clone).ok()
}

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

                let mut response = provider.chat(request, model, temperature).await?;

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
                        model, "Tool call response: {assistant_content}"
                    );
                    messages.push(ChatMessage::assistant(assistant_content.to_string()));

                    // Execute tool calls — parallel when the model returns multiple
                    // calls in one response (it understands ordering dependencies),
                    // sequential otherwise or when opted out via config.
                    let run_parallel = config.parallel_tools && response.tool_calls.len() > 1;
                    let tool_text_preview = response
                        .text
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string);

                    // Emit a single start event with all tool calls.
                    let _ = events_tx.as_ref().map(|tx| {
                        tx.send(TurnEvent::ToolCallStart {
                            parent_tool_name: None,
                            calls: response
                                .tool_calls
                                .iter()
                                .map(|tc| ToolCall {
                                    tool_name: tc.name.clone(),
                                    tool_args: truncate(&tc.arguments, 120),
                                    text_preview: tool_text_preview.clone(),
                                })
                                .collect(),
                        })
                    });

                    let tool_results: Vec<(&nenjo_models::ToolCall, nenjo_tools::ToolResult)> =
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
                                parent_tool_name: None,
                                tool_name: tool_call.name.clone(),
                                result: tool_result.clone(),
                            })
                        });

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
        })
        .await
}

/// Execute a single tool call against the tool registry.
async fn execute_tool(
    agent_name: &str,
    tools: &[Arc<dyn Tool>],
    tool_call: &nenjo_models::ToolCall,
    current_messages: &[ChatMessage],
) -> nenjo_tools::ToolResult {
    info!(
        agent = agent_name,
        tool = %tool_call.name,
        args = %truncate(&tool_call.arguments, 200),
        "Tool call"
    );

    // Find the tool — also match against sanitized names since strict providers
    // (DeepSeek, OpenAI) replace dots/slashes (e.g. "app.nenjo.platform/x" → "app_nenjo_platform_x").
    let tool = match tools.iter().find(|t| {
        let name = t.name();
        name == tool_call.name
            || nenjo_models::sanitize_tool_name(name) == tool_call.name
            || nenjo_models::sanitize_tool_name_lenient(name) == tool_call.name
    }) {
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
    let execute = async {
        match tool.execute(args).await {
            Ok(result) => result,
            Err(e) => nenjo_tools::ToolResult {
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
