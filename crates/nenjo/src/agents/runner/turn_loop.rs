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
use nenjo_models::ModelProvider;
use regex::Regex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::compaction::{
    compact_messages_with_summary, truncate, truncate_old_tool_arguments, truncate_str,
};
use super::types::{ToolCall, TurnEvent, TurnLoopConfig, TurnOutput};
use crate::agents::instance::AgentInstance;
use crate::hooks::{
    ActiveHook, ActiveHookScope, HookBlock, HookEvent, HookRuntime, HookRuntimeEvent,
};
use crate::provider::ProviderRuntime;
use crate::tools::{Tool, ToolCategory, ToolResult};
use nenjo_models::{ChatMessage, ChatRequest};

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

tokio::task_local! {
    static CURRENT_HOOK_RUNTIME: Option<Arc<HookRuntime>>;
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

pub(crate) fn activate_current_hook_scope(scope: ActiveHookScope) -> bool {
    let Ok(Some(runtime)) = CURRENT_HOOK_RUNTIME.try_with(Clone::clone) else {
        return false;
    };
    runtime.activate_scope(scope);
    true
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
pub async fn run<P>(
    agent: &AgentInstance<P>,
    mut messages: Vec<ChatMessage>,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    pause_token: Option<super::types::PauseToken>,
) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let agent_name = agent.name();
    let model_provider = &*agent.model.model_provider;
    let model = &agent.model.model_name;
    let temperature = agent.model.temperature;
    let tools = &agent.runtime.tools;
    let tool_specs = agent.tool_specs();
    let tool_specs = tool_specs.as_slice();
    let hook_runtime = agent.runtime.hook_runtime.clone();
    let config = TurnLoopConfig {
        max_turns: agent.runtime.config.max_turns as u32,
        parallel_tools: agent.runtime.config.parallel_tools,
    };
    let max_turns = config.max_turns;

    let mut final_text = String::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_tool_calls: u32 = 0;
    let mut user_prompt_submit_hooks_seen = HashSet::new();

    let nested_usage = CURRENT_NESTED_TOKEN_USAGE
        .try_with(Clone::clone)
        .unwrap_or_else(|_| Arc::new(NestedTokenUsage::default()));
    let run_depth = nested_usage.run_depth.fetch_add(1, Ordering::Relaxed) + 1;
    let nested_input_baseline = nested_usage.input_tokens.load(Ordering::Relaxed);
    let nested_output_baseline = nested_usage.output_tokens.load(Ordering::Relaxed);

    let run_result = CURRENT_NESTED_TOKEN_USAGE
        .scope(nested_usage.clone(), async {
            CURRENT_HOOK_RUNTIME
                .scope(hook_runtime.clone(), async {
                    CURRENT_EVENTS_TX.scope(events_tx.clone(), async {
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
                let raw_window = model_provider
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
                    agent.runtime.config.context_compaction_trigger_percent,
                );
                // Compact conversation if token estimate still exceeds budget
                // after argument truncation.
                compact_messages_with_summary(
                    model_provider,
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

                if let Some(prompt) = latest_user_prompt(&messages) {
                    let prompt = prompt.to_string();
                    let outcome = run_user_prompt_submit_hooks(
                        agent_name,
                        hook_runtime.as_ref(),
                        &prompt,
                        &messages,
                        events_tx.as_ref(),
                        &mut user_prompt_submit_hooks_seen,
                    )
                    .await;
                    if let Some(block) = outcome.block {
                        final_text =
                            format!("Blocked by hook {}: {}", block.hook, block.reason);
                        remove_latest_user_prompt(&mut messages, &prompt);
                        break;
                    }
                    append_user_prompt_hook_contexts(
                        &mut messages,
                        events_tx.as_ref(),
                        outcome.additional_contexts,
                    );
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

                let model_request_id = Uuid::new_v4().to_string();
                emit_event(
                    events_tx.as_ref(),
                    TurnEvent::ModelRequestStarted {
                        request_id: model_request_id.clone(),
                        parent_call_id: None,
                        provider: None,
                        model: model.to_string(),
                    },
                );
                let mut response = model_provider.chat(request, model, temperature).await?;
                let original_tool_call_count = response.tool_calls.len();
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
                if let Some(text) = response.text.as_deref()
                    && !text.is_empty()
                {
                    emit_event(
                        events_tx.as_ref(),
                        TurnEvent::AssistantTextDelta {
                            request_id: model_request_id.clone(),
                            delta: text.to_string(),
                        },
                    );
                }
                emit_event(
                    events_tx.as_ref(),
                    TurnEvent::ModelRequestCompleted {
                        request_id: model_request_id.clone(),
                        parent_call_id: None,
                    },
                );

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
                    let tool_batch_id = Uuid::new_v4().to_string();

                    // Emit a single start event with all tool calls.
                    emit_event(
                        events_tx.as_ref(),
                        TurnEvent::ToolCallStart {
                            batch_id: tool_batch_id.clone(),
                            parent_tool_name: None,
                            calls: response
                                .tool_calls
                                .iter()
                                .map(|tc| ToolCall {
                                    tool_call_id: Some(tc.id.clone()),
                                    tool_name: tc.name.clone(),
                                    tool_args: tc.arguments.clone(),
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
                                let hook_runtime = hook_runtime.clone();
                                async move {
                                    let result = execute_tool(
                                        agent_name,
                                        tools,
                                        tc,
                                        &current_messages,
                                        hook_runtime,
                                    )
                                    .await;
                                    (tc, result)
                                }
                            });
                            futures_util::future::join_all(futs).await
                        } else {
                            let mut results = Vec::with_capacity(response.tool_calls.len());
                            for tc in &response.tool_calls {
                                let result =
                                    execute_tool(agent_name, tools, tc, &messages, hook_runtime.clone())
                                        .await;
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
                                batch_id: tool_batch_id.clone(),
                                parent_tool_name: None,
                                tool_call_id: Some(tool_call.id.clone()),
                                tool_name: tool_call.name.clone(),
                                tool_args: tool_call.arguments.clone(),
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
                        if let Some(block) = run_stop_hooks(
                            agent_name,
                            hook_runtime.as_ref(),
                            events_tx.as_ref(),
                            &messages,
                            &final_text,
                        )
                        .await
                        {
                            final_text.clear();
                            append_hook_block_continuation(
                                &mut messages,
                                events_tx.as_ref(),
                                block,
                            );
                            continue;
                        }
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
                    messages.push(ChatMessage::developer(
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
                if let Some(block) = run_stop_hooks(
                    agent_name,
                    hook_runtime.as_ref(),
                    events_tx.as_ref(),
                    &messages,
                    &final_text,
                )
                .await
                {
                    final_text.clear();
                    append_hook_block_continuation(&mut messages, events_tx.as_ref(), block);
                    continue;
                }
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
                task_id: None,
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
    hook_runtime: Option<Arc<HookRuntime>>,
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
    let events_tx = current_events_tx();

    let outcome = run_hooks_for_event(
        agent_name,
        hook_runtime.as_ref(),
        HookRuntimeEvent::PreToolUse {
            tool_name: &tool_call.name,
            tool_input: &args,
            tool_use_id: Some(&tool_call.id),
        },
        Some(&tool_call.name),
        events_tx.as_ref(),
    )
    .await;
    if let Some(block) = outcome.block {
        return ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Blocked by hook {}: {}", block.hook, block.reason)),
        };
    }

    // Execute
    let tool_args = args.clone();
    let execute = async {
        match tool.execute(tool_args).await {
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

    let result = if let Some(tx) = events_tx.clone() {
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
    };

    let tool_response = serde_json::json!({
        "success": result.success,
        "output": &result.output,
        "error": &result.error,
    });
    let outcome = run_hooks_for_event(
        agent_name,
        hook_runtime.as_ref(),
        HookRuntimeEvent::PostToolUse {
            tool_name: &tool_call.name,
            tool_input: &args,
            tool_response: &tool_response,
            tool_use_id: Some(&tool_call.id),
        },
        Some(&tool_call.name),
        events_tx.as_ref(),
    )
    .await;
    if let Some(block) = outcome.block {
        return ToolResult {
            success: false,
            output: result.output,
            error: Some(format!("Blocked by hook {}: {}", block.hook, block.reason)),
        };
    }

    result
}

async fn run_stop_hooks(
    agent_name: &str,
    hook_runtime: Option<&Arc<HookRuntime>>,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    messages: &[ChatMessage],
    final_text: &str,
) -> Option<HookBlock> {
    run_hooks_for_event(
        agent_name,
        hook_runtime,
        HookRuntimeEvent::Stop {
            messages,
            final_text,
        },
        None,
        events_tx,
    )
    .await
    .block
}

#[derive(Default)]
struct HookRunOutcome {
    block: Option<HookBlock>,
    additional_contexts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ActiveHookKey {
    hook_slug: crate::Slug,
    source_kind: String,
    source_name: String,
}

impl ActiveHookKey {
    fn from_active(active: &ActiveHook) -> Self {
        Self {
            hook_slug: active.hook.slug.clone(),
            source_kind: active.source.kind().to_string(),
            source_name: active.source.name().to_string(),
        }
    }
}

async fn run_user_prompt_submit_hooks(
    agent_name: &str,
    hook_runtime: Option<&Arc<HookRuntime>>,
    prompt: &str,
    messages: &[ChatMessage],
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    seen: &mut HashSet<ActiveHookKey>,
) -> HookRunOutcome {
    let Some(runtime) = hook_runtime.map(Arc::as_ref) else {
        return HookRunOutcome::default();
    };
    if runtime.is_empty() {
        return HookRunOutcome::default();
    }

    let active_hooks = runtime
        .matching_hooks(&HookEvent::UserPromptSubmit, None)
        .into_iter()
        .filter(|active| seen.insert(ActiveHookKey::from_active(active)))
        .collect();

    run_selected_hooks_for_event(
        agent_name,
        runtime,
        HookRuntimeEvent::UserPromptSubmit { prompt, messages },
        active_hooks,
        events_tx,
    )
    .await
}

async fn run_hooks_for_event(
    agent_name: &str,
    hook_runtime: Option<&Arc<HookRuntime>>,
    event: HookRuntimeEvent<'_>,
    subject: Option<&str>,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
) -> HookRunOutcome {
    let Some(runtime) = hook_runtime.map(Arc::as_ref) else {
        return HookRunOutcome::default();
    };
    if runtime.is_empty() {
        return HookRunOutcome::default();
    }

    let hook_event = hook_event_for_runtime_event(&event);
    let active_hooks = runtime.matching_hooks(&hook_event, subject);
    run_selected_hooks_for_event(agent_name, runtime, event, active_hooks, events_tx).await
}

async fn run_selected_hooks_for_event(
    agent_name: &str,
    runtime: &HookRuntime,
    event: HookRuntimeEvent<'_>,
    active_hooks: Vec<ActiveHook>,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
) -> HookRunOutcome {
    let mut outcome = HookRunOutcome::default();
    for active in active_hooks {
        let hook_label = active.hook.label().to_string();
        let hook_event = active.hook.event.as_str().to_string();
        let hook_type = active.hook.hook_type.clone();
        let source = active.source.kind().to_string();
        emit_event(
            events_tx,
            TurnEvent::HookStarted {
                hook: hook_label.clone(),
                hook_event: hook_event.clone(),
                hook_type: hook_type.clone(),
                source: source.clone(),
            },
        );
        debug!(
            agent = agent_name,
            hook = %hook_label,
            hook_event = %hook_event,
            source = %active_hook_source(&active),
            "Executing hook"
        );
        let execution = runtime.execute(&active, event.clone()).await;
        emit_event(
            events_tx,
            TurnEvent::HookCompleted {
                hook: hook_label.clone(),
                hook_event,
                hook_type,
                source,
                success: execution.success,
                blocked: execution.blocked,
                exit_code: execution.exit_code,
                output: truncate(&execution.stdout, 1_000),
                error: (!execution.stderr.trim().is_empty())
                    .then(|| truncate(&execution.stderr, 1_000)),
                reason: execution.reason.clone(),
            },
        );
        if let Some(additional_context) = execution
            .additional_context
            .clone()
            .filter(|context| !context.trim().is_empty())
        {
            outcome.additional_contexts.push(additional_context);
        }
        if execution.blocked {
            outcome.block = Some(HookBlock {
                hook: hook_label,
                reason: hook_block_reason(&execution),
                system_message: execution.system_message,
            });
            return outcome;
        }
    }
    outcome
}

fn hook_event_for_runtime_event(event: &HookRuntimeEvent<'_>) -> HookEvent {
    match event {
        HookRuntimeEvent::UserPromptSubmit { .. } => HookEvent::UserPromptSubmit,
        HookRuntimeEvent::PreToolUse { .. } => HookEvent::PreToolUse,
        HookRuntimeEvent::PostToolUse { .. } => HookEvent::PostToolUse,
        HookRuntimeEvent::Stop { .. } => HookEvent::Stop,
    }
}

fn latest_user_prompt(messages: &[ChatMessage]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| message.content.as_str())
}

fn remove_latest_user_prompt(messages: &mut Vec<ChatMessage>, prompt: &str) {
    if let Some(index) = messages
        .iter()
        .rposition(|message| message.role == "user" && message.content == prompt)
    {
        messages.remove(index);
    }
}

fn append_user_prompt_hook_contexts(
    messages: &mut Vec<ChatMessage>,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    contexts: Vec<String>,
) {
    let contexts: Vec<String> = contexts
        .into_iter()
        .map(|context| context.trim().to_string())
        .filter(|context| !context.is_empty())
        .collect();
    if contexts.is_empty() {
        return;
    }
    let message = ChatMessage::developer(format!(
        "Additional context from UserPromptSubmit hooks:\n\n{}",
        contexts.join("\n\n")
    ));
    messages.push(message.clone());
    emit_event(events_tx, TurnEvent::TranscriptMessage { message });
}

fn hook_block_reason(execution: &crate::hooks::HookExecution) -> String {
    execution
        .reason
        .as_ref()
        .filter(|reason| !reason.trim().is_empty())
        .cloned()
        .or_else(|| {
            (!execution.stderr.trim().is_empty()).then(|| truncate(&execution.stderr, 1_000))
        })
        .unwrap_or_else(|| "Hook blocked continuation without a reason.".to_string())
}

fn append_hook_block_continuation(
    messages: &mut Vec<ChatMessage>,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    block: HookBlock,
) {
    if let Some(system_message) = block
        .system_message
        .filter(|message| !message.trim().is_empty())
    {
        let message = ChatMessage::developer(system_message);
        messages.push(message.clone());
        emit_event(events_tx, TurnEvent::TranscriptMessage { message });
    }

    let message = ChatMessage::user(format!(
        "Hook `{}` blocked completion and requested continuation:\n{}",
        block.hook, block.reason
    ));
    messages.push(message.clone());
    emit_event(events_tx, TurnEvent::TranscriptMessage { message });
}

fn active_hook_source(active: &ActiveHook) -> String {
    format!("{}:{}", active.source.kind(), active.source.name())
}
