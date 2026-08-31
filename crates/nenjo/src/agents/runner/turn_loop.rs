//! Core agent turn loop — LLM call → tool execution → repeat.
//!
//! This module contains the generic turn loop that drives agent execution.
//! It is independent of Nenjo platform concepts (NATS, streaming, bootstrap).
//! Callers build prompts and pass pre-built messages to [`run()`].

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use nenjo_models::ModelProvider;
use regex::Regex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::chat::ProviderResponseDelivery;
use super::compaction::{
    compact_messages_for_payload, compact_messages_with_summary, estimate_serialized_bytes,
    estimate_serialized_messages_bytes, truncate, truncate_old_tool_arguments, truncate_str,
};
use super::tool_calls::{normalize_tool_call_arguments, tool_for_call};
use super::types::{
    ToolCall, TurnEvent, TurnInputReceiver, TurnLoopConfig, TurnLoopError, TurnOutput,
};
use crate::agents::async_ops::{
    AsyncOpWaitFilter, AsyncOperationRuntime, scope_current_async_operation_runtime,
};
use crate::agents::instance::AgentInstance;
use crate::hooks::{
    ActiveHook, ActiveHookScope, HookBlock, HookEvent, HookRuntime, HookRuntimeEvent,
};
use crate::provider::{ArtifactInputPreparer, ProviderRuntime};
use crate::tools::{Tool, ToolCategory, ToolResult};
use nenjo_models::{
    ChatMessage, ChatRequest, ConversationMessage, ProviderStreamEvent, ProviderToolTrace,
    ToolOutput, ToolResultMessage,
};

/// How a turn is allowed to reach a successful terminal state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TurnCompletion {
    /// A non-empty assistant response may end the turn.
    Natural,
    /// Only a successful call to the named terminal tool may end the turn.
    RequireTool(&'static str),
}

impl TurnCompletion {
    fn required_tool(self) -> Option<&'static str> {
        match self {
            Self::Natural => None,
            Self::RequireTool(tool) => Some(tool),
        }
    }
}

fn provider_tool_metadata(trace: &ProviderToolTrace) -> serde_json::Value {
    serde_json::json!({
        "tool_origin": "provider",
        "provider_native": true,
        "provider": trace.provider.clone(),
        "citations": trace.citations.clone(),
    })
}

fn provider_tool_result(trace: &ProviderToolTrace) -> ToolResult {
    let output = trace
        .output
        .clone()
        .unwrap_or_else(|| serde_json::json!({ "status": "completed" }))
        .to_string();

    ToolResult {
        success: true,
        output: output.into(),
        error: None,
    }
}

fn emit_provider_tool_start(
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    batch_id: String,
    trace: &ProviderToolTrace,
) {
    let metadata = provider_tool_metadata(trace);
    emit_event(
        events_tx,
        TurnEvent::ToolCallStart {
            batch_id,
            parent_tool_name: None,
            calls: vec![ToolCall {
                tool_call_id: Some(trace.id.clone()),
                tool_name: trace.name.clone(),
                tool_args: trace.input.to_string(),
                text_preview: None,
                metadata: Some(metadata),
            }],
        },
    );
}

fn emit_provider_tool_end(
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    batch_id: String,
    trace: &ProviderToolTrace,
) {
    let metadata = provider_tool_metadata(trace);
    emit_event(
        events_tx,
        TurnEvent::ToolCallEnd {
            batch_id,
            parent_tool_name: None,
            tool_call_id: Some(trace.id.clone()),
            tool_name: trace.name.clone(),
            tool_args: trace.input.to_string(),
            result: provider_tool_result(trace),
            metadata: Some(metadata),
        },
    );
}

async fn chat_with_provider_stream<P>(
    provider: &P,
    request: ChatRequest<'_>,
    model: &str,
    temperature: f64,
    request_id: &str,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    cancel: &CancellationToken,
) -> anyhow::Result<(
    nenjo_models::ChatResponse,
    HashSet<String>,
    HashSet<String>,
    bool,
)>
where
    P: ModelProvider + ?Sized,
{
    request.ensure_artifacts_prepared()?;
    let (stream_tx, mut stream_rx) = mpsc::channel(64);
    let response = provider.chat_stream(request, model, temperature, stream_tx);
    tokio::pin!(response);

    let mut provider_tool_batches = std::collections::HashMap::<String, String>::new();
    let mut started_provider_tools = HashSet::new();
    let mut completed_provider_tools = HashSet::new();
    let mut emitted_text_delta = false;
    let provider_request_started_at = Instant::now();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                anyhow::bail!("execution cancelled");
            }
            Some(event) = stream_rx.recv() => {
                match event {
                    ProviderStreamEvent::TextDelta(delta) => {
                        if !delta.is_empty() {
                            if !emitted_text_delta {
                                debug!(
                                    request_id,
                                    provider_to_first_delta_us = provider_request_started_at.elapsed().as_micros(),
                                    "Received first provider text delta"
                                );
                            }
                            emitted_text_delta = true;
                            emit_event(
                                events_tx,
                                TurnEvent::AssistantTextDelta {
                                    request_id: request_id.to_string(),
                                    delta,
                                },
                            );
                        }
                    }
                    ProviderStreamEvent::ReasoningDelta(delta) => {
                        if !delta.is_empty() {
                            emit_event(
                                events_tx,
                                TurnEvent::AssistantReasoningDelta {
                                    request_id: request_id.to_string(),
                                    delta,
                                },
                            );
                        }
                    }
                    ProviderStreamEvent::ProviderToolStarted(trace) => {
                        let batch_id = provider_tool_batches
                            .entry(trace.id.clone())
                            .or_insert_with(|| Uuid::new_v4().to_string())
                            .clone();
                        emit_provider_tool_start(events_tx, batch_id, &trace);
                        started_provider_tools.insert(trace.id);
                    }
                    ProviderStreamEvent::ProviderToolCompleted(trace) => {
                        let batch_id = provider_tool_batches
                            .entry(trace.id.clone())
                            .or_insert_with(|| Uuid::new_v4().to_string())
                            .clone();
                        emit_provider_tool_end(events_tx, batch_id, &trace);
                        started_provider_tools.insert(trace.id.clone());
                        completed_provider_tools.insert(trace.id);
                    }
                    ProviderStreamEvent::CapacityWaiting { limit } => emit_event(
                        events_tx,
                        TurnEvent::ModelCapacityWaiting {
                            request_id: request_id.to_string(),
                            limit,
                        },
                    ),
                    ProviderStreamEvent::CapacityAcquired => emit_event(
                        events_tx,
                        TurnEvent::ModelCapacityAcquired {
                            request_id: request_id.to_string(),
                        },
                    ),
                    ProviderStreamEvent::RetryScheduled {
                        provider,
                        model,
                        attempt,
                        max_attempts,
                        delay_ms,
                        code,
                        message,
                    } => emit_event(
                        events_tx,
                        TurnEvent::ProviderRetryScheduled {
                            request_id: request_id.to_string(),
                            provider,
                            model,
                            attempt,
                            max_attempts,
                            delay_ms,
                            code,
                            message,
                        },
                    ),
                }
            }
            result = &mut response => {
                let response = result?;
                while let Ok(event) = stream_rx.try_recv() {
                    match event {
                        ProviderStreamEvent::TextDelta(delta) => {
                            if !delta.is_empty() {
                                if !emitted_text_delta {
                                    debug!(
                                        request_id,
                                        provider_to_first_delta_us = provider_request_started_at.elapsed().as_micros(),
                                        "Received first provider text delta"
                                    );
                                }
                                emitted_text_delta = true;
                                emit_event(
                                    events_tx,
                                    TurnEvent::AssistantTextDelta {
                                        request_id: request_id.to_string(),
                                        delta,
                                    },
                                );
                            }
                        }
                        ProviderStreamEvent::ReasoningDelta(delta) => {
                            if !delta.is_empty() {
                                emit_event(
                                    events_tx,
                                    TurnEvent::AssistantReasoningDelta {
                                        request_id: request_id.to_string(),
                                        delta,
                                    },
                                );
                            }
                        }
                        ProviderStreamEvent::ProviderToolStarted(trace) => {
                            let batch_id = provider_tool_batches
                                .entry(trace.id.clone())
                                .or_insert_with(|| Uuid::new_v4().to_string())
                                .clone();
                            emit_provider_tool_start(events_tx, batch_id, &trace);
                            started_provider_tools.insert(trace.id);
                        }
                        ProviderStreamEvent::ProviderToolCompleted(trace) => {
                            let batch_id = provider_tool_batches
                                .entry(trace.id.clone())
                                .or_insert_with(|| Uuid::new_v4().to_string())
                                .clone();
                            emit_provider_tool_end(events_tx, batch_id, &trace);
                            started_provider_tools.insert(trace.id.clone());
                            completed_provider_tools.insert(trace.id);
                        }
                        ProviderStreamEvent::CapacityWaiting { limit } => emit_event(
                            events_tx,
                            TurnEvent::ModelCapacityWaiting {
                                request_id: request_id.to_string(),
                                limit,
                            },
                        ),
                        ProviderStreamEvent::CapacityAcquired => emit_event(
                            events_tx,
                            TurnEvent::ModelCapacityAcquired {
                                request_id: request_id.to_string(),
                            },
                        ),
                        ProviderStreamEvent::RetryScheduled {
                            provider,
                            model,
                            attempt,
                            max_attempts,
                            delay_ms,
                            code,
                            message,
                        } => emit_event(
                            events_tx,
                            TurnEvent::ProviderRetryScheduled {
                                request_id: request_id.to_string(),
                                provider,
                                model,
                                attempt,
                                max_attempts,
                                delay_ms,
                                code,
                                message,
                            },
                        ),
                    }
                }
                debug!(
                    request_id,
                    provider_request_duration_us = provider_request_started_at.elapsed().as_micros(),
                    streamed_text_delta = emitted_text_delta,
                    "Provider request finished"
                );
                return Ok((
                    response,
                    started_provider_tools,
                    completed_provider_tools,
                    emitted_text_delta,
                ));
            }
        }
    }
}

struct ProviderChatExecution<'a> {
    request_id: &'a str,
    events_tx: Option<&'a mpsc::UnboundedSender<TurnEvent>>,
    cancel: &'a CancellationToken,
    delivery: ProviderResponseDelivery,
}

async fn chat_with_provider<P>(
    provider: &P,
    request: ChatRequest<'_>,
    model: &str,
    temperature: f64,
    execution: ProviderChatExecution<'_>,
) -> anyhow::Result<(
    nenjo_models::ChatResponse,
    HashSet<String>,
    HashSet<String>,
    bool,
)>
where
    P: ModelProvider + ?Sized,
{
    match execution.delivery {
        ProviderResponseDelivery::Buffered => {
            request.ensure_artifacts_prepared()?;
            let response = tokio::select! {
                _ = execution.cancel.cancelled() => anyhow::bail!("execution cancelled"),
                response = provider.chat(request, model, temperature) => response?,
            };
            Ok((response, HashSet::new(), HashSet::new(), false))
        }
        ProviderResponseDelivery::Streaming => {
            chat_with_provider_stream(
                provider,
                request,
                model,
                temperature,
                execution.request_id,
                execution.events_tx,
                execution.cancel,
            )
            .await
        }
    }
}

fn emit_event(events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>, event: TurnEvent) {
    if let Some(tx) = events_tx {
        let _ = tx.send(event);
    }
}

async fn model_visible_operation_continuation<P>(
    agent: &AgentInstance<P>,
    turn_input: Option<&TurnInputReceiver>,
) -> Result<Option<String>>
where
    P: ProviderRuntime,
{
    let wait = if let Some(turn_input) = turn_input {
        tokio::select! {
            wait = agent.runtime.async_ops.wait(30, AsyncOpWaitFilter::model_visible()) => wait,
            _ = turn_input.notified() => {
                return Ok(Some("A new user message arrived while async work was in progress. Read the queued user message before deciding what to do next.".into()));
            }
        }
    } else {
        agent
            .runtime
            .async_ops
            .wait(30, AsyncOpWaitFilter::model_visible())
            .await
    };
    let has_open = agent.runtime.async_ops.has_open_model_visible().await;
    if wait.updates.is_empty() && !has_open {
        return Ok(None);
    }

    let updates = serde_json::to_string(&wait.updates)?;
    let instruction = if has_open {
        "A model-visible async operation is still running or waiting for input. Do not give a final answer yet. Use the async operation tools if input is needed; otherwise continue waiting for completion before summarizing the result."
    } else {
        "A model-visible async operation produced terminal updates. Use these updates before giving the final answer."
    };

    Ok(Some(format!(
        "{instruction}\n\nAsync operation updates:\n{updates}"
    )))
}

async fn drain_queued_user_messages(
    messages: &mut Vec<ConversationMessage>,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    turn_input: Option<&TurnInputReceiver>,
) -> bool {
    let Some(turn_input) = turn_input else {
        return false;
    };
    let queued = turn_input.drain().await;
    let mut appended = false;
    for queued_message in queued {
        let content = queued_message.content.trim();
        if content.is_empty() && queued_message.artifacts.is_empty() {
            continue;
        }
        let message = ConversationMessage::chat(
            ChatMessage::user(content.to_string()).with_artifacts(queued_message.artifacts),
        );
        messages.push(message.clone());
        emit_event(events_tx, TurnEvent::TranscriptMessage { message });
        appended = true;
    }
    appended
}

tokio::task_local! {
    static CURRENT_EVENTS_TX: Option<mpsc::UnboundedSender<TurnEvent>>;
}

tokio::task_local! {
    static CURRENT_TURN_INPUT: Option<TurnInputReceiver>;
}

tokio::task_local! {
    static CURRENT_CHAT_HISTORY: Vec<ConversationMessage>;
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

pub(crate) fn current_turn_input() -> Option<TurnInputReceiver> {
    CURRENT_TURN_INPUT.try_with(Clone::clone).ok().flatten()
}

fn cancelled_tool_result() -> ToolResult {
    ToolResult {
        success: false,
        output: String::new().into(),
        error: Some("execution cancelled".into()),
    }
}

pub(crate) fn current_chat_history() -> Option<Vec<ConversationMessage>> {
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

/// Conservative fallback context window when no configured or provider value exists.
const DEFAULT_CONTEXT_WINDOW: usize = 100_000;
const CONTEXT_WINDOW_SAFETY_NUMERATOR: usize = 4;
const CONTEXT_WINDOW_SAFETY_DENOMINATOR: usize = 5;

fn compaction_context_budget(
    configured_context_window: Option<u64>,
    provider_context_window: Option<usize>,
) -> usize {
    let raw_window = configured_context_window
        .and_then(|window| usize::try_from(window).ok())
        .filter(|window| *window > 0)
        .or(provider_context_window)
        .filter(|window| *window > 0)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);

    raw_window
        .saturating_mul(CONTEXT_WINDOW_SAFETY_NUMERATOR)
        .saturating_div(CONTEXT_WINDOW_SAFETY_DENOMINATOR)
        .max(1)
}

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
    mut messages: Vec<ConversationMessage>,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    pause_token: Option<super::types::PauseToken>,
    turn_input: Option<TurnInputReceiver>,
    completion: TurnCompletion,
    response_delivery: ProviderResponseDelivery,
) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let agent_name = agent.name();
    let model_provider = &*agent.model.model_provider;
    let model = &agent.model.model_name;
    let temperature = agent.model.temperature;
    let tools = &agent.runtime.tools;
    if let Some(required_tool) = completion.required_tool()
        && !tools
            .iter()
            .any(|tool| tool.name() == required_tool && tool.is_terminal())
    {
        return Err(TurnLoopError::RequiredCompletionToolUnavailable {
            tool: required_tool,
        }
        .into());
    }
    let cancel = agent.runtime.execution_cancel.clone();
    let visible_tool_specs = agent.visible_tool_specs().await;
    let initial_local_tool_specs = agent.visible_local_tool_specs().await;
    let visible_tool_specs = visible_tool_specs.as_slice();
    let initial_local_tool_specs = initial_local_tool_specs.as_slice();
    let hook_runtime = agent.runtime.hook_runtime.clone();
    let config = TurnLoopConfig {
        max_turns: agent.runtime.config.max_turns,
        parallel_tools: agent.runtime.config.parallel_tools,
    };
    let max_turns = config.max_turns;

    let mut final_text = String::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_tool_calls: u32 = 0;
    let mut user_prompt_submit_hooks_seen = HashSet::new();

    enum TurnLoopExit {
        Completed,
        MaxTurnsReached,
    }

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
                        CURRENT_TURN_INPUT.scope(turn_input.clone(), async {
            // Log the visible tool belt once before the loop.
            if !visible_tool_specs.is_empty() {
                let tool_names: Vec<&str> =
                    visible_tool_specs.iter().map(|t| t.name.as_str()).collect();
                debug!(
                    agent = agent_name,
                    model,
                    tool_count = visible_tool_specs.len(),
                    local_tool_count = initial_local_tool_specs.len(),
                    native_tool_count = agent.model_manifest.native_tools.len(),
                    tools = ?tool_names,
                    "Turn loop starting with tools"
                );
            } else {
                warn!(
                    agent = agent_name,
                    model, "Turn loop starting with NO tools"
                );
            }

            let mut loop_exit = TurnLoopExit::MaxTurnsReached;
            for iteration in 0..max_turns {
                if cancel.is_cancelled() {
                    agent.runtime.async_ops.stop(
                        Vec::new(),
                        None,
                        Some("execution cancelled".into()),
                        events_tx.clone(),
                    ).await;
                    anyhow::bail!("execution cancelled");
                }
                debug!(
                    agent = agent_name,
                    iteration,
                    messages_count = messages.len(),
                    "Turn loop iteration"
                );

                if agent.runtime.config.compact_context {
                    // Prefer the catalog-derived model context window. It reflects the
                    // exact configured provider model; provider heuristics remain a
                    // fallback for legacy/manual model configurations.
                    let context_budget = compaction_context_budget(
                        agent.model_manifest.context_window,
                        model_provider.context_window(model),
                    );

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
                }

                // Check pause token before each LLM call. If paused, block until
                // resumed. In-flight tool executions finish before we reach this point.
                if let Some(ref pt) = pause_token
                    && pt.is_paused()
                {
                    emit_event(events_tx.as_ref(), TurnEvent::Paused);
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            agent.runtime.async_ops.stop(
                                Vec::new(),
                                None,
                                Some("execution cancelled".into()),
                                events_tx.clone(),
                            ).await;
                            anyhow::bail!("execution cancelled");
                        }
                        _ = pt.wait_if_paused() => {}
                    }
                    emit_event(events_tx.as_ref(), TurnEvent::Resumed);
                }

                drain_queued_user_messages(&mut messages, events_tx.as_ref(), turn_input.as_ref())
                    .await;

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
                        loop_exit = TurnLoopExit::Completed;
                        break;
                    }
                    append_user_prompt_hook_contexts(
                        &mut messages,
                        events_tx.as_ref(),
                        outcome.additional_contexts,
                    );
                }

                // Call LLM
                let local_tool_specs = agent
                    .visible_local_tool_specs()
                    .await;
                let tools_ref = if local_tool_specs.is_empty() {
                    None
                } else {
                    Some(local_tool_specs.as_slice())
                };
                let tool_payload_bytes = tools_ref.map(estimate_serialized_bytes).unwrap_or(0);
                let message_payload_budget = agent
                    .runtime
                    .config
                    .max_model_request_payload_bytes
                    .saturating_sub(tool_payload_bytes)
                    .saturating_sub(1024);
                let payload_bytes_before = estimate_serialized_messages_bytes(&messages);
                if compact_messages_for_payload(&mut messages, message_payload_budget) {
                    warn!(
                        agent = agent_name,
                        model,
                        payload_bytes_before,
                        payload_bytes_after = estimate_serialized_messages_bytes(&messages),
                        message_payload_budget,
                        tool_payload_bytes,
                        "Compacted messages to fit model request payload budget"
                    );
                }

                let artifact_input_preparer = agent
                    .runtime
                    .provider_runtime
                    .as_ref()
                    .and_then(ProviderRuntime::artifact_input_preparer);
                let mut prepared_model_artifacts = match artifact_input_preparer {
                    Some(preparer) if messages.iter().any(ConversationMessage::has_artifact_references) => {
                        let prepared = preparer
                            .prepare(&messages, &agent.manifest, &agent.model_manifest)
                            .await
                            .context("failed to prepare artifact inputs for the model request")?;
                        record_nested_token_usage(
                            prepared.usage.input_tokens,
                            prepared.usage.output_tokens,
                        );
                        Some(prepared)
                    }
                    Some(_) | None => None,
                };
                if let Some(prepared) = &mut prepared_model_artifacts
                    && compact_messages_for_payload(
                        &mut prepared.request_messages,
                        message_payload_budget,
                    )
                {
                    warn!(
                        agent = agent_name,
                        model,
                        payload_bytes_after = estimate_serialized_messages_bytes(
                            &prepared.request_messages
                        ),
                        message_payload_budget,
                        "Compacted prepared artifact context to fit model request payload budget"
                    );
                }
                if let Some(prepared) = &prepared_model_artifacts {
                    for analysis in &prepared.new_analysis_messages {
                        let message = ConversationMessage::artifact_analysis(analysis.clone());
                        messages.push(message.clone());
                        emit_event(
                            events_tx.as_ref(),
                            TurnEvent::TranscriptMessage { message },
                        );
                    }
                }
                let request_messages = prepared_model_artifacts
                    .as_ref()
                    .map_or(messages.as_slice(), |prepared| {
                        prepared.request_messages.as_slice()
                    });
                let request = ChatRequest {
                    messages: request_messages,
                    tools: tools_ref,
                    native_tools: Some(&agent.model_manifest.native_tools),
                    prepared_artifacts: prepared_model_artifacts
                        .as_ref()
                        .map(|prepared| &prepared.artifacts),
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
                let (
                    mut response,
                    streamed_provider_tool_started_ids,
                    streamed_provider_tool_completed_ids,
                    streamed_text_delta,
                ) =
                    chat_with_provider(
                        model_provider,
                        request,
                        model,
                        temperature,
                        ProviderChatExecution {
                            request_id: &model_request_id,
                            events_tx: events_tx.as_ref(),
                            cancel: &cancel,
                            delivery: response_delivery,
                        },
                    )
                    .await?;
                for tool_call in &mut response.tool_calls {
                    let normalized_fields = normalize_tool_call_arguments(tools, tool_call);
                    if normalized_fields > 0 {
                        warn!(
                            agent = agent_name,
                            model,
                            tool = %tool_call.name,
                            normalized_fields,
                            "Normalized JSON-encoded structured tool arguments"
                        );
                    }
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

                for provider_tool in response
                    .provider_tool_calls
                    .iter()
                    .filter(|trace| !streamed_provider_tool_completed_ids.contains(&trace.id))
                {
                    let batch_id = Uuid::new_v4().to_string();
                    if !streamed_provider_tool_started_ids.contains(&provider_tool.id) {
                        emit_provider_tool_start(
                            events_tx.as_ref(),
                            batch_id.clone(),
                            provider_tool,
                        );
                    }
                    emit_provider_tool_end(events_tx.as_ref(), batch_id, provider_tool);
                }
                total_tool_calls += response.provider_tool_calls.len() as u32;

                if let Some(text) = response.text.as_deref()
                    && !text.is_empty()
                    && !streamed_text_delta
                    && response_delivery == ProviderResponseDelivery::Streaming
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

                // Log structural response metadata only. Model text and tool
                // arguments may contain user or decrypted artifact content.
                debug!(
                    agent = agent_name,
                    model,
                    iteration,
                    has_tool_calls = response.has_tool_calls(),
                    tool_call_count = response.tool_calls.len(),
                    has_text = response.text.is_some(),
                    text_len = response.text.as_deref().map(str::len).unwrap_or(0),
                    input_tokens = response.usage.input_tokens,
                    output_tokens = response.usage.output_tokens,
                    "LLM response received"
                );

                // If the LLM requested tool calls, execute them
                if response.has_tool_calls() {
                    debug!(
                        agent = agent_name,
                        model,
                        tool_call_count = response.tool_calls.len(),
                        assistant_text_len = response.text.as_deref().map(str::len).unwrap_or(0),
                        tool_names = ?response
                            .tool_calls
                            .iter()
                            .map(|call| call.name.as_str())
                            .collect::<Vec<_>>(),
                        "LLM requested tool calls"
                    );
                    let assistant_message = ConversationMessage::assistant_tool_calls(
                        response.text.clone(),
                        response.tool_calls.clone(),
                    );
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
                                    metadata: None,
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
                                let cancel = cancel.clone();
                                async move {
                                    let result = execute_tool(
                                        agent_name,
                                        tools,
                                        tc,
                                        &current_messages,
                                        hook_runtime,
                                        AsyncOperationRuntime::new(
                                            agent.runtime.async_ops.clone(),
                                        ),
                                        cancel,
                                    )
                                    .await;
                                    (tc, result)
                                }
                            });
                            futures_util::future::join_all(futs).await
                        } else {
                            let mut results = Vec::with_capacity(response.tool_calls.len());
                            for tc in &response.tool_calls {
                                let result = execute_tool(
                                    agent_name,
                                    tools,
                                    tc,
                                    &messages,
                                    hook_runtime.clone(),
                                    AsyncOperationRuntime::new(agent.runtime.async_ops.clone()),
                                    cancel.clone(),
                                )
                                .await;
                                results.push((tc, result));
                            }
                            results
                        };

                    total_tool_calls += tool_results.len() as u32;

                    // Check if any executed tool is terminal.
                    // Terminal tools signal that the turn loop should stop immediately
                    // without feeding the tool result back to the LLM.
                    let terminal_result = tool_results.iter().find(|(tc, result)| {
                        result.success
                            && tool_for_call(tools, tc).is_some_and(|tool| {
                                tool.is_terminal()
                                    && completion
                                        .required_tool()
                                        .is_none_or(|required| tool.name() == required)
                            })
                    });
                    let has_terminal_attempt = tool_results.iter().any(|(tc, _)| {
                        tool_for_call(tools, tc)
                            .is_some_and(|t| t.is_terminal())
                    });

                    let mut result_messages = Vec::with_capacity(tool_results.len());
                    // Emit result events and build one semantic result batch in order.
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
                                metadata: None,
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

                        // Build tool result message with tool_call_id so providers
                        // can match each result to its corresponding tool call. Keep
                        // successful terminal results too: if a stop hook requests
                        // continuation, the next provider request must still have a
                        // structurally complete tool-call exchange.
                        let output = if tool_result.success {
                            tool_result.output.clone()
                        } else {
                            let error = tool_result
                                .error
                                .clone()
                                .unwrap_or_else(|| tool_result.output.text_content());
                            ToolOutput::text(format!(
                                "Error: {}",
                                error
                            ))
                        };
                        let raw_content = output.text_content();

                        debug!(
                            agent = agent_name,
                            model,
                            tool = %tool_call.name,
                            tool_call_id = %tool_call.id,
                            success = tool_result.success,
                            response_len = raw_content.len(),
                            "Tool call response"
                        );

                        result_messages.push(ToolResultMessage::new(tool_call.id.clone(), output));

                    }
                    let tool_results_message = ConversationMessage::ToolResults(result_messages);
                    messages.push(tool_results_message.clone());
                    emit_event(
                        events_tx.as_ref(),
                        TurnEvent::TranscriptMessage {
                            message: tool_results_message,
                        },
                    );

                    // Terminal tool: stop the loop. The verdict is already recorded
                    // in the assistant message's tool_calls for extraction.
                    if let Some((_, terminal_tool_result)) = terminal_result {
                        debug!(
                            agent = agent_name,
                            model, "Terminal tool called, ending turn loop"
                        );
                        let terminal_tool_text = terminal_tool_result.output.text_content();
                        final_text = if completion.required_tool().is_some() {
                            terminal_tool_text
                        } else {
                            response
                                .text
                                .as_deref()
                                .filter(|text| !text.trim().is_empty())
                                .map(ToOwned::to_owned)
                                .unwrap_or(terminal_tool_text)
                        };
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
                        loop_exit = TurnLoopExit::Completed;
                        break;
                    }

                    if has_terminal_attempt {
                        continue;
                    }

                    continue;
                }

                // No tool calls — check if we have a final text response.
                let text = response.text.unwrap_or_default();

                if !response.finish_reason.permits_natural_completion() {
                    return Err(TurnLoopError::ProviderIncomplete {
                        reason: format!("{:?}", response.finish_reason),
                    }
                    .into());
                }

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
                    messages.push(ConversationMessage::assistant(String::new()));
                    messages.push(ConversationMessage::developer(
                        "Your previous response was empty. Please respond to the user's request."
                            .to_string(),
                    ));
                    continue;
                }

                final_text = text.clone();
                if let Some(message) =
                    model_visible_operation_continuation(agent, turn_input.as_ref()).await?
                {
                    debug!(
                        agent = agent_name,
                        model,
                        iteration,
                        "Deferring final response until model-visible async operations settle"
                    );
                    final_text.clear();
                    messages.push(ConversationMessage::developer(message));
                    continue;
                }
                if let Some(required_tool) = completion.required_tool() {
                    messages.push(ConversationMessage::developer(format!(
                        "Your previous response was not delivered because this runtime requires the {required_tool} tool to end the turn. Continue the work, then call {required_tool} only when the requested work is fully handled.\n\nUndelivered response draft:\n{text}"
                    )));
                    continue;
                }
                let assistant_message = ConversationMessage::assistant(text);
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
                loop_exit = TurnLoopExit::Completed;
                break;
            }

            if matches!(loop_exit, TurnLoopExit::MaxTurnsReached) {
                let error = TurnLoopError::MaxTurnsReached { max_turns };
                warn!(
                    agent = agent_name,
                    model,
                    max_turns,
                    error = %error,
                    "Turn loop failed after reaching max turns"
                );
                agent
                    .runtime
                    .async_ops
                    .stop(
                        Vec::new(),
                        None,
                        Some(error.to_string()),
                        events_tx.clone(),
                    )
                    .await;
                return Err(error.into());
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
                        }).await
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
    current_messages: &[ConversationMessage],
    hook_runtime: Option<Arc<HookRuntime>>,
    async_operations: AsyncOperationRuntime,
    cancel: CancellationToken,
) -> ToolResult {
    info!(
        agent = agent_name,
        tool = %tool_call.name,
        args_len = tool_call.arguments.len(),
        "Executing tool call"
    );

    // Find the tool — also match against sanitized names since strict providers
    // (DeepSeek, OpenAI) replace dots/slashes (e.g. "app.nenjo.platform/x" → "app_nenjo_platform_x").
    let tool = match tool_for_call(tools, tool_call) {
        Some(t) => t,
        None => {
            return ToolResult {
                success: false,
                output: String::new().into(),
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
                output: String::new().into(),
                error: Some(format!("Failed to parse tool arguments: {e}")),
            };
        }
    };
    let events_tx = current_events_tx();

    let pre_tool_hooks = run_hooks_for_event(
        agent_name,
        hook_runtime.as_ref(),
        HookRuntimeEvent::PreToolUse {
            tool_name: &tool_call.name,
            tool_input: &args,
            tool_use_id: Some(&tool_call.id),
        },
        Some(&tool_call.name),
        events_tx.as_ref(),
    );
    let outcome = tokio::select! {
        _ = cancel.cancelled() => return cancelled_tool_result(),
        outcome = pre_tool_hooks => outcome,
    };
    if let Some(block) = outcome.block {
        return ToolResult {
            success: false,
            output: String::new().into(),
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
                output: String::new().into(),
                error: Some(format!("Tool execution error: {e}")),
            },
        }
    };

    let current_history: Vec<ConversationMessage> = current_messages
        .iter()
        .filter(|message| {
            !message.as_chat().is_some_and(|chat| {
                matches!(
                    chat.role,
                    nenjo_models::ChatRole::System | nenjo_models::ChatRole::Developer
                )
            })
        })
        .cloned()
        .collect();

    let scoped_execute = async {
        if let Some(tx) = events_tx.clone() {
            CURRENT_EVENTS_TX
                .scope(
                    Some(tx),
                    CURRENT_CHAT_HISTORY.scope(
                        current_history,
                        scope_current_async_operation_runtime(async_operations, execute),
                    ),
                )
                .await
        } else {
            CURRENT_EVENTS_TX
                .scope(
                    None,
                    CURRENT_CHAT_HISTORY.scope(
                        current_history,
                        scope_current_async_operation_runtime(async_operations, execute),
                    ),
                )
                .await
        }
    };
    let result = tokio::select! {
        _ = cancel.cancelled() => return cancelled_tool_result(),
        result = scoped_execute => result,
    };

    let tool_response = serde_json::json!({
        "success": result.success,
        // Keep the established hook projection text-compatible while exposing
        // ordered semantic parts to hooks that opt into artifact-aware output.
        "output": result.output.text_content(),
        "output_parts": &result.output,
        "error": &result.error,
    });
    let post_tool_hooks = run_hooks_for_event(
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
    );
    let outcome = tokio::select! {
        _ = cancel.cancelled() => return cancelled_tool_result(),
        outcome = post_tool_hooks => outcome,
    };
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
    messages: &[ConversationMessage],
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
    messages: &[ConversationMessage],
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

fn latest_user_prompt(messages: &[ConversationMessage]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find_map(|message| {
            message
                .as_chat()
                .filter(|chat| chat.role == nenjo_models::ChatRole::User)
        })
        .map(|message| message.content.as_str())
}

fn remove_latest_user_prompt(messages: &mut Vec<ConversationMessage>, prompt: &str) {
    if let Some(index) = messages.iter().rposition(|message| {
        message
            .as_chat()
            .is_some_and(|chat| chat.role == nenjo_models::ChatRole::User && chat.content == prompt)
    }) {
        messages.remove(index);
    }
}

fn append_user_prompt_hook_contexts(
    messages: &mut Vec<ConversationMessage>,
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
    let message = ConversationMessage::developer(format!(
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
    messages: &mut Vec<ConversationMessage>,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
    block: HookBlock,
) {
    if let Some(system_message) = block
        .system_message
        .filter(|message| !message.trim().is_empty())
    {
        let message = ConversationMessage::developer(system_message);
        messages.push(message.clone());
        emit_event(events_tx, TurnEvent::TranscriptMessage { message });
    }

    let message = ConversationMessage::user(format!(
        "Hook `{}` blocked completion and requested continuation:\n{}",
        block.hook, block.reason
    ));
    messages.push(message.clone());
    emit_event(events_tx, TurnEvent::TranscriptMessage { message });
}

fn active_hook_source(active: &ActiveHook) -> String {
    format!("{}:{}", active.source.kind(), active.source.name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct DeliveryProbe {
        buffered_calls: AtomicUsize,
        streaming_calls: AtomicUsize,
    }

    impl DeliveryProbe {
        fn new() -> Self {
            Self {
                buffered_calls: AtomicUsize::new(0),
                streaming_calls: AtomicUsize::new(0),
            }
        }

        fn response() -> nenjo_models::ChatResponse {
            nenjo_models::ChatResponse {
                text: Some("complete".to_string()),
                tool_calls: Vec::new(),
                provider_tool_calls: Vec::new(),
                usage: nenjo_models::TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::Stop,
            }
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for DeliveryProbe {
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<nenjo_models::ChatResponse> {
            self.buffered_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Self::response())
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
            events: mpsc::Sender<ProviderStreamEvent>,
        ) -> anyhow::Result<nenjo_models::ChatResponse> {
            self.streaming_calls.fetch_add(1, Ordering::SeqCst);
            events
                .send(ProviderStreamEvent::TextDelta("partial".to_string()))
                .await?;
            Ok(Self::response())
        }
    }

    #[test]
    fn compaction_budget_prefers_configured_context_window() {
        assert_eq!(
            compaction_context_budget(Some(200_000), Some(1_000_000)),
            160_000
        );
        assert_eq!(compaction_context_budget(None, Some(128_000)), 102_400);
        assert_eq!(compaction_context_budget(None, None), 80_000);
    }

    #[tokio::test]
    async fn provider_delivery_selects_one_exhaustive_transport() {
        let provider = DeliveryProbe::new();
        let messages = vec![ConversationMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };
        let cancel = CancellationToken::new();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        let (_, _, _, emitted_delta) = chat_with_provider(
            &provider,
            request,
            "model",
            0.0,
            ProviderChatExecution {
                request_id: "buffered-request",
                events_tx: Some(&events_tx),
                cancel: &cancel,
                delivery: ProviderResponseDelivery::Buffered,
            },
        )
        .await
        .expect("buffered response");

        assert!(!emitted_delta);
        assert_eq!(provider.buffered_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.streaming_calls.load(Ordering::SeqCst), 0);
        assert!(events_rx.try_recv().is_err());

        let (_, _, _, emitted_delta) = chat_with_provider(
            &provider,
            request,
            "model",
            0.0,
            ProviderChatExecution {
                request_id: "streaming-request",
                events_tx: Some(&events_tx),
                cancel: &cancel,
                delivery: ProviderResponseDelivery::Streaming,
            },
        )
        .await
        .expect("streaming response");

        assert!(emitted_delta);
        assert_eq!(provider.buffered_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.streaming_calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            events_rx.try_recv(),
            Ok(TurnEvent::AssistantTextDelta { delta, .. }) if delta == "partial"
        ));
    }
}
