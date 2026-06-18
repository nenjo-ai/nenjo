//! Event bridging — converts runtime events to NATS response types.

use nenjo::manifest::Manifest;
use nenjo_events::{AsyncOperationTranscriptEvent, Response, StepAgent, StreamEvent};
use serde::Serialize;
use tracing::{debug, trace};
use uuid::Uuid;

use nenjo_harness::preview::{PREVIEW_MAX_CHARS, summarize_preview, truncate_preview};

fn merge_tool_payload(
    mut payload: serde_json::Value,
    metadata: Option<&serde_json::Value>,
) -> serde_json::Value {
    let Some(metadata) = metadata.and_then(serde_json::Value::as_object) else {
        return payload;
    };
    let Some(payload_object) = payload.as_object_mut() else {
        return payload;
    };

    for (key, value) in metadata {
        payload_object.insert(key.clone(), value.clone());
    }

    payload
}

// ---------------------------------------------------------------------------
// Typed step-event data payloads
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct StepCompletedData {
    pub step_slug: String,
    pub step_run_id: Uuid,
    pub passed: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Serialize)]
pub struct StepFailedData {
    pub step_slug: String,
    pub step_run_id: Uuid,
    pub error: &'static str,
}

#[derive(Serialize)]
pub struct CronCycleStartedData {
    pub cycle: u32,
}

#[derive(Serialize)]
pub struct CronCycleCompletedData {
    pub cycle: u32,
    pub passed: bool,
}

/// Convert a TurnEvent to StreamEvents for the frontend.
pub fn turn_event_to_stream_events(
    event: &nenjo::TurnEvent,
    agent_name: &str,
    run_id: &str,
    session_id: Uuid,
) -> Vec<StreamEvent> {
    let stream_events = match event {
        nenjo::TurnEvent::ModelRequestStarted {
            request_id,
            parent_call_id,
            provider,
            model,
        } => vec![StreamEvent::ModelRequestStarted {
            run_id: run_id.to_string(),
            request_id: request_id.clone(),
            parent_call_id: parent_call_id.clone(),
            provider: provider.clone(),
            model: Some(model.clone()),
        }],
        nenjo::TurnEvent::AssistantTextDelta { request_id, delta } => {
            vec![StreamEvent::AssistantTextDelta {
                run_id: run_id.to_string(),
                request_id: request_id.clone(),
                payload: Some(serde_json::json!({
                    "delta": delta,
                })),
                encrypted_payload: None,
            }]
        }
        nenjo::TurnEvent::ModelRequestCompleted {
            request_id,
            parent_call_id,
        } => {
            vec![StreamEvent::ModelRequestCompleted {
                run_id: run_id.to_string(),
                request_id: request_id.clone(),
                parent_call_id: parent_call_id.clone(),
            }]
        }
        nenjo::TurnEvent::AbilityStarted { .. } => Vec::new(),
        nenjo::TurnEvent::ToolCallStart {
            batch_id,
            parent_tool_name,
            calls,
        } => calls
            .iter()
            .map(|call| StreamEvent::ToolCallStarted {
                run_id: run_id.to_string(),
                batch_id: batch_id.clone(),
                call_id: call
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| format!("{}:{}", call.tool_name, call.tool_args.len())),
                parent_call_id: parent_tool_name.clone(),
                tool_name: call.tool_name.clone(),
                payload: Some(merge_tool_payload(
                    serde_json::json!({
                        "tool_name": call.tool_name,
                        "tool_args": call.tool_args,
                        "text_preview": call.text_preview,
                    }),
                    call.metadata.as_ref(),
                )),
                encrypted_payload: None,
            })
            .collect(),
        nenjo::TurnEvent::ToolCallEnd {
            batch_id,
            parent_tool_name,
            tool_call_id,
            tool_name,
            tool_args,
            result,
            metadata,
        } => vec![StreamEvent::ToolCallCompleted {
            run_id: run_id.to_string(),
            batch_id: batch_id.clone(),
            call_id: tool_call_id
                .clone()
                .unwrap_or_else(|| format!("{}:{}", tool_name, tool_args.len())),
            parent_call_id: parent_tool_name.clone(),
            success: result.success,
            payload: Some(merge_tool_payload(
                serde_json::json!({
                    "tool_name": tool_name,
                    "tool_args": tool_args,
                    "output_preview": truncate_preview(&result.output, PREVIEW_MAX_CHARS),
                    "error_preview": result.error.as_deref().and_then(summarize_preview),
                }),
                metadata.as_ref(),
            )),
            encrypted_payload: None,
        }],
        nenjo::TurnEvent::HookStarted {
            hook,
            hook_event,
            hook_type,
            source,
        } => vec![StreamEvent::HookStarted {
            agent: agent_name.to_string(),
            hook: hook.clone(),
            hook_event: hook_event.clone(),
            hook_type: hook_type.clone(),
            source: source.clone(),
            payload: None,
            encrypted_payload: None,
        }],
        nenjo::TurnEvent::HookActivated { .. } => Vec::new(),
        nenjo::TurnEvent::HookCompleted {
            hook,
            hook_event,
            hook_type,
            source,
            success,
            blocked,
            exit_code,
            output,
            error,
            reason,
        } => vec![StreamEvent::HookCompleted {
            agent: agent_name.to_string(),
            hook: hook.clone(),
            hook_event: hook_event.clone(),
            hook_type: hook_type.clone(),
            source: source.clone(),
            success: *success,
            blocked: *blocked,
            payload: Some(serde_json::json!({
                "exit_code": exit_code,
                "output_preview": summarize_preview(output),
                "error_preview": error.as_deref().and_then(summarize_preview),
                "reason": reason,
            })),
            encrypted_payload: None,
        }],
        nenjo::TurnEvent::AbilityCompleted { .. } => Vec::new(),
        nenjo::TurnEvent::SubAgentEvent { .. } => Vec::new(),
        nenjo::TurnEvent::SubAgentTranscript { .. } => Vec::new(),
        nenjo::TurnEvent::AsyncOperationEvent {
            operation_id,
            kind,
            label,
            parent_operation_id,
            parent_tool_name,
            status,
            signal,
            summary,
            payload,
            model_visible,
        } => vec![StreamEvent::AsyncOperationEvent {
            operation_id: operation_id.clone(),
            kind: kind.clone(),
            label: label.clone(),
            status: status.clone(),
            signal: signal.clone(),
            model_visible: *model_visible,
            parent_operation_id: parent_operation_id.clone(),
            parent_tool_name: parent_tool_name.clone(),
            summary: summary.clone(),
            payload: payload.clone(),
            encrypted_payload: None,
        }],
        nenjo::TurnEvent::AsyncOperationTranscript {
            operation_id,
            kind,
            label,
            event,
        } => vec![StreamEvent::AsyncOperationTranscript {
            operation_id: operation_id.clone(),
            kind: kind.clone(),
            label: label.clone(),
            event: AsyncOperationTranscriptEvent {
                kind: event.kind().to_string(),
                summary: event.summary().to_string(),
                tool: event.tool_name().map(ToOwned::to_owned),
                success: event.success(),
            },
            payload: None,
            encrypted_payload: None,
        }],
        nenjo::TurnEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => vec![StreamEvent::MessageCompacted {
            messages_before: *messages_before,
            messages_after: *messages_after,
        }],
        nenjo::TurnEvent::TranscriptMessage { .. } => Vec::new(),
        nenjo::TurnEvent::Paused => vec![StreamEvent::Paused],
        nenjo::TurnEvent::Resumed => vec![StreamEvent::Resumed],
        nenjo::TurnEvent::Done { output } => vec![
            StreamEvent::RunCompleted {
                run_id: run_id.to_string(),
                session_id: session_id.to_string(),
            },
            StreamEvent::Done {
                payload: Some(serde_json::Value::String(output.text.clone())),
                encrypted_payload: None,
                total_input_tokens: output.input_tokens,
                total_output_tokens: output.output_tokens,
                project: None,
                agent: None,
                session_id: None,
            },
        ],
    };

    for stream_event in &stream_events {
        trace!(
            turn_event = %summarize_turn_event(event),
            stream_event = %summarize_stream_event(stream_event),
            agent = agent_name,
            "Bridged turn event to pre-codec stream event"
        );
    }
    if stream_events.is_empty() {
        debug!(
            turn_event = %summarize_turn_event(event),
            agent = agent_name,
            "Turn event did not produce a stream event"
        );
    }

    stream_events
}

pub fn summarize_turn_event(event: &nenjo::TurnEvent) -> String {
    match event {
        nenjo::TurnEvent::ModelRequestStarted {
            request_id,
            parent_call_id,
            model,
            ..
        } => format!(
            "model_request_started(request={request_id}, parent={}, model={model})",
            parent_call_id.as_deref().unwrap_or("-")
        ),
        nenjo::TurnEvent::AssistantTextDelta { request_id, delta } => {
            format!(
                "assistant_text_delta(request={request_id}, len={})",
                delta.len()
            )
        }
        nenjo::TurnEvent::ModelRequestCompleted {
            request_id,
            parent_call_id,
        } => format!(
            "model_request_completed(request={request_id}, parent={})",
            parent_call_id.as_deref().unwrap_or("-")
        ),
        nenjo::TurnEvent::AbilityStarted {
            call_id,
            ability_tool_name,
            ability_name,
            task_input,
            caller_history,
        } => format!(
            "ability_started(call={call_id}, tool={ability_tool_name}, ability={ability_name}, task_len={}, caller_messages={})",
            task_input.len(),
            caller_history.len()
        ),
        nenjo::TurnEvent::ToolCallStart {
            batch_id,
            parent_tool_name,
            calls,
        } => format!(
            "tool_call_start(batch={batch_id}, parent={}, tools=[{}], count={})",
            parent_tool_name.as_deref().unwrap_or("-"),
            calls
                .iter()
                .map(|call| call.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            calls.len()
        ),
        nenjo::TurnEvent::ToolCallEnd {
            batch_id,
            parent_tool_name,
            tool_name,
            tool_args,
            result,
            ..
        } => format!(
            "tool_call_end(batch={batch_id}, parent={}, tool={tool_name}, args_len={}, success={}, output_len={}, error={})",
            parent_tool_name.as_deref().unwrap_or("-"),
            tool_args.len(),
            result.success,
            result.output.len(),
            result
                .error
                .as_deref()
                .map(|err| truncate_preview(err, 80))
                .unwrap_or_else(|| "-".to_string())
        ),
        nenjo::TurnEvent::AbilityCompleted {
            call_id,
            ability_tool_name,
            ability_name,
            success,
            final_output,
        } => format!(
            "ability_completed(call={call_id}, tool={ability_tool_name}, ability={ability_name}, success={success}, output_len={})",
            final_output.len()
        ),
        nenjo::TurnEvent::HookStarted {
            hook,
            hook_event,
            hook_type,
            source,
        } => format!(
            "hook_started(hook={hook}, event={hook_event}, type={hook_type}, source={source})"
        ),
        nenjo::TurnEvent::HookActivated {
            hook,
            hook_event,
            hook_type,
            source,
        } => format!(
            "hook_signal(hook={hook}, event={hook_event}, type={hook_type}, source={source})"
        ),
        nenjo::TurnEvent::HookCompleted {
            hook,
            hook_event,
            hook_type,
            source,
            success,
            blocked,
            exit_code,
            ..
        } => format!(
            "hook_completed(hook={hook}, event={hook_event}, type={hook_type}, source={source}, success={success}, blocked={blocked}, exit_code={})",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        nenjo::TurnEvent::SubAgentEvent {
            slug,
            agent_name,
            kind,
            summary,
            ..
        } => format!(
            "sub_agent_event(slug={slug}, agent={agent_name}, kind={kind}, summary_len={})",
            summary.len()
        ),
        nenjo::TurnEvent::SubAgentTranscript {
            slug,
            agent_name,
            event,
        } => format!(
            "sub_agent_transcript(slug={slug}, agent={agent_name}, kind={}, summary_len={})",
            event.kind(),
            event.summary().len()
        ),
        nenjo::TurnEvent::AsyncOperationEvent {
            operation_id,
            kind,
            status,
            signal,
            ..
        } => format!(
            "async_operation_event(id={operation_id}, kind={kind}, signal={signal}, status={status})"
        ),
        nenjo::TurnEvent::AsyncOperationTranscript {
            operation_id,
            kind,
            event,
            ..
        } => format!(
            "async_operation_transcript(id={operation_id}, kind={kind}, event={}, summary_len={})",
            event.kind(),
            event.summary().len()
        ),
        nenjo::TurnEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => format!("message_compacted({messages_before}->{messages_after})"),
        nenjo::TurnEvent::TranscriptMessage { message } => format!(
            "transcript_message(role={}, content_len={})",
            message.role,
            message.content.len()
        ),
        nenjo::TurnEvent::Paused => "paused".to_string(),
        nenjo::TurnEvent::Resumed => "resumed".to_string(),
        nenjo::TurnEvent::Done { output } => format!(
            "done(text_len={}, input_tokens={}, output_tokens={}, tool_calls={}, messages={})",
            output.text.len(),
            output.input_tokens,
            output.output_tokens,
            output.tool_calls,
            output.messages.len()
        ),
    }
}

pub fn summarize_stream_event(event: &StreamEvent) -> String {
    match event {
        StreamEvent::RunStarted {
            run_id, session_id, ..
        } => format!("run_started(run={run_id}, session={session_id})"),
        StreamEvent::RunCompleted { run_id, .. } => format!("run_completed(run={run_id})"),
        StreamEvent::RunFailed { run_id, .. } => format!("run_failed(run={run_id})"),
        StreamEvent::RunCancelled { run_id, .. } => format!("run_cancelled(run={run_id})"),
        StreamEvent::ModelRequestStarted {
            run_id,
            request_id,
            model,
            ..
        } => format!(
            "model_request_started(run={run_id}, request={request_id}, model={})",
            model.as_deref().unwrap_or("-")
        ),
        StreamEvent::AssistantTextDelta {
            run_id,
            request_id,
            payload,
            encrypted_payload,
        } => format!(
            "assistant_text_delta(run={run_id}, request={request_id}, payload={}, encrypted={})",
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        StreamEvent::ModelRequestCompleted {
            run_id, request_id, ..
        } => {
            format!("model_request_completed(run={run_id}, request={request_id})")
        }
        StreamEvent::ToolCallStarted {
            run_id,
            batch_id,
            call_id,
            tool_name,
            ..
        } => format!(
            "tool_call_started(run={run_id}, batch={batch_id}, call={call_id}, tool={tool_name})"
        ),
        StreamEvent::ToolOutputDelta {
            run_id,
            call_id,
            stream,
            payload,
            encrypted_payload,
        } => format!(
            "tool_output_delta(run={run_id}, call={call_id}, stream={stream}, payload={}, encrypted={})",
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        StreamEvent::ToolCallCompleted {
            run_id,
            batch_id,
            call_id,
            parent_call_id,
            success,
            ..
        } => format!(
            "tool_call_completed(run={run_id}, batch={batch_id}, call={call_id}, parent={}, success={success})",
            parent_call_id.as_deref().unwrap_or("-")
        ),
        StreamEvent::HookStarted {
            agent,
            hook,
            hook_event,
            hook_type,
            source,
            ..
        } => format!(
            "hook_started(agent={agent}, hook={hook}, event={hook_event}, type={hook_type}, source={source})"
        ),
        StreamEvent::HookCompleted {
            agent,
            hook,
            hook_event,
            hook_type,
            source,
            success,
            blocked,
            ..
        } => format!(
            "hook_completed(agent={agent}, hook={hook}, event={hook_event}, type={hook_type}, source={source}, success={success}, blocked={blocked})"
        ),
        StreamEvent::AsyncOperationEvent {
            operation_id,
            kind,
            signal,
            status,
            ..
        } => format!(
            "async_operation_event(id={operation_id}, kind={kind}, signal={signal}, status={status})"
        ),
        StreamEvent::AsyncOperationTranscript {
            operation_id,
            kind,
            event,
            ..
        } => format!(
            "async_operation_transcript(id={operation_id}, kind={kind}, event={})",
            event.kind
        ),
        StreamEvent::Error { message, .. } => {
            format!(
                "error(message={:?}, len={})",
                truncate_preview(message, 80),
                message.len()
            )
        }
        StreamEvent::Done {
            payload,
            encrypted_payload,
            total_input_tokens,
            total_output_tokens,
            project,
            agent,
            session_id,
        } => format!(
            "done(payload={}, encrypted={}, input_tokens={}, output_tokens={}, project={}, agent={}, session_id={})",
            if payload.is_some() { "yes" } else { "no" },
            encrypted_payload.is_some(),
            total_input_tokens,
            total_output_tokens,
            project.as_deref().unwrap_or("-"),
            agent.as_deref().unwrap_or("-"),
            session_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        StreamEvent::DomainEntered {
            session_id,
            domain_name,
        } => format!("domain_entered(name={domain_name}, session={session_id})"),
        StreamEvent::DomainExited {
            session_id,
            artifact_id,
            document_id,
        } => format!(
            "domain_exited(session={}, artifact_id={}, document_id={})",
            session_id,
            artifact_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            document_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        StreamEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => format!("message_compacted({messages_before}->{messages_after})"),
        StreamEvent::Paused => "paused".to_string(),
        StreamEvent::Resumed => "resumed".to_string(),
    }
}

/// Resolve `StepAgent` from an optional agent ID using the manifest.
fn resolve_agent(manifest: &Manifest, agent_id: Option<Uuid>) -> Option<StepAgent> {
    agent_id.map(|aid| {
        let a = manifest
            .agents
            .iter()
            .find(|a| crate::resource_resolver::stable_resource_id("agent", &a.slug) == aid);
        StepAgent {
            agent: a
                .map(|a| a.slug.to_string())
                .unwrap_or_else(|| aid.to_string()),
            agent_name: a.map(|a| a.name.clone()),
            agent_color: a.and_then(|a| a.color.clone()),
        }
    })
}

#[derive(Debug, Clone)]
pub struct RoutineStepRef {
    pub step_slug: String,
    pub step_run_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct TaskTurnEventContext {
    pub execution_run_id: Uuid,
    pub task_id: Option<Uuid>,
    pub agent: Option<StepAgent>,
    pub routine_step: Option<RoutineStepRef>,
    pub agent_duration_ms: Option<u64>,
    pub emit_done: bool,
    pub summarize_outputs: bool,
}

fn task_data(
    routine_step: Option<RoutineStepRef>,
    mut data: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    if let Some(routine_step) = routine_step {
        data.insert(
            "step_slug".to_string(),
            serde_json::Value::String(routine_step.step_slug),
        );
        data.insert(
            "step_run_id".to_string(),
            serde_json::Value::String(routine_step.step_run_id.to_string()),
        );
    }
    serde_json::Value::Object(data)
}

fn task_output_preview(output: &str, summarize: bool) -> String {
    if summarize {
        summarize_preview(output).unwrap_or_else(|| output.to_string())
    } else {
        output.to_string()
    }
}

pub fn turn_event_to_task_step_response(
    event: &nenjo::TurnEvent,
    context: &TaskTurnEventContext,
) -> Option<Response> {
    let execution_run_id = context.execution_run_id.to_string();
    let task_id = context.task_id.map(|id| id.to_string());
    let agent = context.agent.clone();

    match event {
        nenjo::TurnEvent::ToolCallStart {
            batch_id,
            parent_tool_name,
            calls,
        } => Some(Response::TaskStepEvent {
            execution_run_id,
            task_id,
            event_type: "step_started".to_string(),
            step_name: calls
                .first()
                .map(|c| c.tool_name.clone())
                .unwrap_or_else(|| "tool_call".to_string()),
            step_type: "tool".to_string(),
            duration_ms: None,
            data: task_data(
                context.routine_step.clone(),
                serde_json::Map::from_iter([
                    (
                        "batch_id".to_string(),
                        serde_json::Value::String(batch_id.clone()),
                    ),
                    (
                        "parent_tool_name".to_string(),
                        serde_json::to_value(parent_tool_name).unwrap_or_default(),
                    ),
                    (
                        "parent_call_id".to_string(),
                        serde_json::to_value(parent_tool_name).unwrap_or_default(),
                    ),
                    (
                        "tool_call_ids".to_string(),
                        serde_json::to_value(
                            calls
                                .iter()
                                .map(|c| c.tool_call_id.clone())
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                    ),
                    (
                        "tool_names".to_string(),
                        serde_json::to_value(
                            calls
                                .iter()
                                .map(|c| c.tool_name.clone())
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                    ),
                    (
                        "tool_args".to_string(),
                        serde_json::to_value(
                            calls
                                .iter()
                                .map(|c| c.tool_args.clone())
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                    ),
                    (
                        "tool_metadata".to_string(),
                        serde_json::to_value(
                            calls.iter().map(|c| c.metadata.clone()).collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                    ),
                ]),
            ),
            payload: calls.first().and_then(|c| {
                let payload = c
                    .text_preview
                    .as_ref()
                    .map(|preview| serde_json::json!({ "text_preview": preview }))
                    .unwrap_or_else(|| serde_json::json!({}));
                let merged = merge_tool_payload(payload, c.metadata.as_ref());
                (!merged.as_object().is_some_and(serde_json::Map::is_empty)).then_some(merged)
            }),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::ToolCallEnd {
            batch_id,
            parent_tool_name,
            tool_call_id,
            tool_name,
            tool_args,
            result,
            metadata,
        } => Some(Response::TaskStepEvent {
            execution_run_id,
            task_id,
            event_type: if result.success {
                "step_completed".to_string()
            } else {
                "step_failed".to_string()
            },
            step_name: tool_name.clone(),
            step_type: "tool".to_string(),
            duration_ms: None,
            data: task_data(
                context.routine_step.clone(),
                serde_json::Map::from_iter([
                    (
                        "batch_id".to_string(),
                        serde_json::Value::String(batch_id.clone()),
                    ),
                    (
                        "parent_tool_name".to_string(),
                        serde_json::to_value(parent_tool_name).unwrap_or_default(),
                    ),
                    (
                        "parent_call_id".to_string(),
                        serde_json::to_value(parent_tool_name).unwrap_or_default(),
                    ),
                    (
                        "tool_call_id".to_string(),
                        serde_json::to_value(tool_call_id).unwrap_or_default(),
                    ),
                    (
                        "tool_args".to_string(),
                        serde_json::Value::String(tool_args.clone()),
                    ),
                    (
                        "success".to_string(),
                        serde_json::Value::Bool(result.success),
                    ),
                    (
                        "error".to_string(),
                        if result.error.is_some() {
                            serde_json::Value::String("Tool execution failed".to_string())
                        } else {
                            serde_json::Value::Null
                        },
                    ),
                    (
                        "tool_metadata".to_string(),
                        metadata.clone().unwrap_or(serde_json::Value::Null),
                    ),
                ]),
            ),
            payload: Some(merge_tool_payload(
                serde_json::json!({
                    "output_preview": task_output_preview(&result.output, context.summarize_outputs),
                    "error": result.error,
                }),
                metadata.as_ref(),
            )),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::AbilityStarted {
            call_id,
            ability_name,
            task_input,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id,
            task_id,
            event_type: "step_started".to_string(),
            step_name: ability_name.clone(),
            step_type: "ability".to_string(),
            duration_ms: None,
            data: task_data(
                context.routine_step.clone(),
                serde_json::Map::from_iter([(
                    "call_id".to_string(),
                    serde_json::Value::String(call_id.clone()),
                )]),
            ),
            payload: Some(serde_json::json!({
                "task_preview": task_input,
            })),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::AbilityCompleted {
            call_id,
            ability_name,
            success,
            final_output,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id,
            task_id,
            event_type: if *success {
                "step_completed".to_string()
            } else {
                "step_failed".to_string()
            },
            step_name: ability_name.clone(),
            step_type: "ability".to_string(),
            duration_ms: None,
            data: task_data(
                context.routine_step.clone(),
                serde_json::Map::from_iter([
                    ("success".to_string(), serde_json::Value::Bool(*success)),
                    (
                        "call_id".to_string(),
                        serde_json::Value::String(call_id.clone()),
                    ),
                ]),
            ),
            payload: Some(serde_json::json!({
                "output_preview": final_output,
            })),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::SubAgentEvent { .. } => None,
        nenjo::TurnEvent::AsyncOperationEvent {
            operation_id,
            kind,
            label,
            status,
            signal,
            summary,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id,
            task_id,
            event_type: format!("async_operation_{signal}"),
            step_name: label.clone(),
            step_type: kind.clone(),
            duration_ms: None,
            data: task_data(
                context.routine_step.clone(),
                serde_json::Map::from_iter([
                    (
                        "operation_id".to_string(),
                        serde_json::Value::String(operation_id.clone()),
                    ),
                    (
                        "status".to_string(),
                        serde_json::Value::String(status.clone()),
                    ),
                    (
                        "summary".to_string(),
                        serde_json::to_value(summary).unwrap_or_default(),
                    ),
                ]),
            ),
            payload: None,
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::HookActivated { .. }
        | nenjo::TurnEvent::HookStarted { .. }
        | nenjo::TurnEvent::HookCompleted { .. }
        | nenjo::TurnEvent::ModelRequestStarted { .. }
        | nenjo::TurnEvent::AssistantTextDelta { .. }
        | nenjo::TurnEvent::ModelRequestCompleted { .. } => None,
        nenjo::TurnEvent::Done { output } if context.emit_done => Some(Response::TaskStepEvent {
            execution_run_id,
            task_id,
            event_type: "step_completed".to_string(),
            step_name: "agent_response".to_string(),
            step_type: "agent".to_string(),
            duration_ms: context.agent_duration_ms,
            data: serde_json::json!({
                "input_tokens": output.input_tokens,
                "output_tokens": output.output_tokens,
            }),
            payload: Some(serde_json::json!({
                "output_preview": output.text,
            })),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::Done { .. } => None,
        nenjo::TurnEvent::SubAgentTranscript { .. } => None,
        nenjo::TurnEvent::AsyncOperationTranscript { .. } => None,
        nenjo::TurnEvent::TranscriptMessage { .. } => None,
        nenjo::TurnEvent::MessageCompacted { .. } => None,
        nenjo::TurnEvent::Paused | nenjo::TurnEvent::Resumed => None,
    }
}

/// Convert a RoutineEvent to a NATS Response.
pub fn routine_event_to_response(
    event: &nenjo::RoutineEvent,
    execution_run_id: Uuid,
    task_id: Option<Uuid>,
    current_agent_id: Option<Uuid>,
    manifest: &Manifest,
) -> Option<Response> {
    let eid = execution_run_id.to_string();
    let tid = task_id.map(|id| id.to_string());

    match event {
        nenjo::RoutineEvent::StepStarted {
            step_slug,
            step_run_id,
            step_name,
            step_type,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "step_started".to_string(),
            step_name: step_name.clone(),
            step_type: step_type.clone(),
            duration_ms: None,
            data: serde_json::json!({ "step_slug": step_slug, "step_run_id": step_run_id }),
            payload: None,
            encrypted_payload: None,
            agent: None,
        }),
        nenjo::RoutineEvent::StepCompleted {
            step_slug,
            step_run_id,
            result,
            duration_ms,
            ..
        } => {
            let data = StepCompletedData {
                step_slug: step_slug.to_string(),
                step_run_id: *step_run_id,
                passed: result.passed,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
            };

            Some(Response::TaskStepEvent {
                execution_run_id: eid,
                task_id: tid,
                event_type: "step_completed".to_string(),
                step_name: result.step_name.clone(),
                step_type: String::new(),
                duration_ms: Some(*duration_ms),
                data: serde_json::to_value(data).unwrap_or_default(),
                payload: Some(serde_json::json!({
                    "output_preview": result.output,
                    "verdict": result
                        .data
                        .get("verdict")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    "reasoning": result
                        .data
                        .get("reasoning")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                })),
                encrypted_payload: None,
                agent: resolve_agent(manifest, current_agent_id),
            })
        }
        nenjo::RoutineEvent::StepFailed {
            step_slug,
            step_run_id,
            error,
            duration_ms,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "step_failed".to_string(),
            step_name: String::new(),
            step_type: String::new(),
            duration_ms: Some(*duration_ms),
            data: serde_json::to_value(StepFailedData {
                step_slug: step_slug.to_string(),
                step_run_id: *step_run_id,
                error: "Step failed",
            })
            .unwrap_or_default(),
            payload: Some(serde_json::json!({ "error": error })),
            encrypted_payload: None,
            agent: None,
        }),
        nenjo::RoutineEvent::AgentEvent {
            step_slug,
            step_run_id,
            event,
        } => routine_agent_event_to_response(
            event,
            execution_run_id,
            task_id,
            step_slug.to_string(),
            *step_run_id,
            current_agent_id,
            manifest,
        ),
        nenjo::RoutineEvent::Done { .. } => None,
        nenjo::RoutineEvent::CronCycleStarted { cycle } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "cron_cycle_started".to_string(),
            step_name: format!("cycle-{cycle}"),
            step_type: "cron".to_string(),
            duration_ms: None,
            data: serde_json::to_value(CronCycleStartedData { cycle: *cycle }).unwrap_or_default(),
            payload: None,
            encrypted_payload: None,
            agent: None,
        }),
        nenjo::RoutineEvent::CronCycleCompleted { cycle, result, .. } => {
            Some(Response::TaskStepEvent {
                execution_run_id: eid,
                task_id: tid,
                event_type: "cron_cycle_completed".to_string(),
                step_name: format!("cycle-{cycle}"),
                step_type: "cron".to_string(),
                duration_ms: None,
                data: serde_json::to_value(CronCycleCompletedData {
                    cycle: *cycle,
                    passed: result.passed,
                })
                .unwrap_or_default(),
                payload: None,
                encrypted_payload: None,
                agent: None,
            })
        }
    }
}

fn routine_agent_event_to_response(
    event: &nenjo::TurnEvent,
    execution_run_id: Uuid,
    task_id: Option<Uuid>,
    routine_step_slug: String,
    routine_step_run_id: Uuid,
    current_agent_id: Option<Uuid>,
    manifest: &Manifest,
) -> Option<Response> {
    turn_event_to_task_step_response(
        event,
        &TaskTurnEventContext {
            execution_run_id,
            task_id,
            agent: resolve_agent(manifest, current_agent_id),
            routine_step: Some(RoutineStepRef {
                step_slug: routine_step_slug,
                step_run_id: routine_step_run_id,
            }),
            agent_duration_ms: None,
            emit_done: false,
            summarize_outputs: true,
        },
    )
}

/// Get project slug from manifest, falling back to UUID string.
pub fn project_slug(manifest: &Manifest, project_id: Uuid) -> String {
    if project_id.is_nil() {
        return String::new();
    }
    match manifest
        .projects
        .iter()
        .find(|p| crate::resource_resolver::stable_resource_id("project", &p.slug) == project_id)
    {
        Some(p) => p.slug.to_string(),
        None => project_id.to_string(),
    }
}

/// Get agent name from manifest, falling back to UUID string.
pub fn agent_name(manifest: &Manifest, agent_id: Uuid) -> String {
    manifest
        .agents
        .iter()
        .find(|a| crate::resource_resolver::stable_resource_id("agent", &a.slug) == agent_id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| agent_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_agent_lifecycle_is_not_sent_as_stream_event() {
        let event = nenjo::TurnEvent::SubAgentEvent {
            slug: "review".to_string(),
            agent_name: "specialist".to_string(),
            kind: "completed".to_string(),
            summary: "done".to_string(),
            model_visible: false,
        };

        assert!(turn_event_to_stream_events(&event, "leader", "run-1", Uuid::new_v4()).is_empty());
    }

    #[test]
    fn sub_agent_lifecycle_is_not_sent_as_task_step_response() {
        let event = nenjo::TurnEvent::SubAgentEvent {
            slug: "review".to_string(),
            agent_name: "specialist".to_string(),
            kind: "completed".to_string(),
            summary: "done".to_string(),
            model_visible: false,
        };

        let response = turn_event_to_task_step_response(
            &event,
            &TaskTurnEventContext {
                execution_run_id: Uuid::new_v4(),
                task_id: Some(Uuid::new_v4()),
                agent: None,
                routine_step: None,
                agent_duration_ms: None,
                emit_done: true,
                summarize_outputs: false,
            },
        );

        assert!(response.is_none());
    }

    #[test]
    fn stream_tool_output_preview_keeps_json_body() {
        let output = serde_json::json!({
            "models": [
                { "id": "model-a", "name": "Model A" },
                { "id": "model-b", "name": "Model B" }
            ]
        })
        .to_string();

        let events = turn_event_to_stream_events(
            &nenjo::TurnEvent::ToolCallEnd {
                batch_id: "batch-1".to_string(),
                parent_tool_name: None,
                tool_call_id: Some("call-1".to_string()),
                tool_name: "list_models".to_string(),
                tool_args: "{}".to_string(),
                result: nenjo::ToolResult {
                    success: true,
                    output: output.clone(),
                    error: None,
                },
                metadata: None,
            },
            "agent",
            "run-1",
            Uuid::new_v4(),
        );

        match events.as_slice() {
            [
                StreamEvent::ToolCallCompleted {
                    batch_id, payload, ..
                },
            ] => {
                assert_eq!(batch_id, "batch-1");
                assert_eq!(
                    payload
                        .as_ref()
                        .and_then(|payload| payload.get("output_preview"))
                        .and_then(serde_json::Value::as_str),
                    Some(output.as_str()),
                );
            }
            other => panic!("unexpected stream events: {other:?}"),
        }
    }

    #[test]
    fn provider_tool_metadata_is_included_in_stream_tool_events() {
        let metadata = serde_json::json!({
            "tool_origin": "provider",
            "provider_native": true,
            "provider": "xai",
        });

        let started = turn_event_to_stream_events(
            &nenjo::TurnEvent::ToolCallStart {
                batch_id: "batch-1".to_string(),
                parent_tool_name: None,
                calls: vec![nenjo::agents::ToolCall {
                    tool_call_id: Some("call-1".to_string()),
                    tool_name: "web_search".to_string(),
                    tool_args: "{}".to_string(),
                    text_preview: Some("provider web search".to_string()),
                    metadata: Some(metadata.clone()),
                }],
            },
            "agent",
            "run-1",
            Uuid::new_v4(),
        );

        match started.as_slice() {
            [StreamEvent::ToolCallStarted { payload, .. }] => {
                let payload = payload.as_ref().expect("payload");
                assert_eq!(payload["tool_origin"], "provider");
                assert_eq!(payload["provider_native"], true);
                assert_eq!(payload["provider"], "xai");
                assert_eq!(payload["tool_name"], "web_search");
            }
            other => panic!("unexpected stream events: {other:?}"),
        }

        let completed = turn_event_to_stream_events(
            &nenjo::TurnEvent::ToolCallEnd {
                batch_id: "batch-1".to_string(),
                parent_tool_name: None,
                tool_call_id: Some("call-1".to_string()),
                tool_name: "web_search".to_string(),
                tool_args: "{}".to_string(),
                result: nenjo::ToolResult {
                    success: true,
                    output: "{\"status\":\"completed\"}".to_string(),
                    error: None,
                },
                metadata: Some(metadata),
            },
            "agent",
            "run-1",
            Uuid::new_v4(),
        );

        match completed.as_slice() {
            [StreamEvent::ToolCallCompleted { payload, .. }] => {
                let payload = payload.as_ref().expect("payload");
                assert_eq!(payload["tool_origin"], "provider");
                assert_eq!(payload["provider_native"], true);
                assert_eq!(payload["provider"], "xai");
                assert_eq!(payload["tool_name"], "web_search");
            }
            other => panic!("unexpected stream events: {other:?}"),
        }
    }

    #[test]
    fn provider_tool_metadata_is_included_in_task_step_events() {
        let metadata = serde_json::json!({
            "tool_origin": "provider",
            "provider_native": true,
            "provider": "xai",
        });
        let context = TaskTurnEventContext {
            execution_run_id: Uuid::new_v4(),
            task_id: Some(Uuid::new_v4()),
            agent: None,
            routine_step: None,
            agent_duration_ms: None,
            emit_done: true,
            summarize_outputs: false,
        };

        let started = turn_event_to_task_step_response(
            &nenjo::TurnEvent::ToolCallStart {
                batch_id: "batch-1".to_string(),
                parent_tool_name: None,
                calls: vec![nenjo::agents::ToolCall {
                    tool_call_id: Some("call-1".to_string()),
                    tool_name: "web_search".to_string(),
                    tool_args: "{}".to_string(),
                    text_preview: Some("provider web search".to_string()),
                    metadata: Some(metadata.clone()),
                }],
            },
            &context,
        )
        .expect("tool start should bridge");

        match started {
            Response::TaskStepEvent { data, payload, .. } => {
                assert_eq!(data["tool_metadata"][0]["tool_origin"], "provider");
                let payload = payload.expect("payload");
                assert_eq!(payload["provider_native"], true);
                assert_eq!(payload["provider"], "xai");
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let completed = turn_event_to_task_step_response(
            &nenjo::TurnEvent::ToolCallEnd {
                batch_id: "batch-1".to_string(),
                parent_tool_name: None,
                tool_call_id: Some("call-1".to_string()),
                tool_name: "web_search".to_string(),
                tool_args: "{}".to_string(),
                result: nenjo::ToolResult {
                    success: true,
                    output: "{\"status\":\"completed\"}".to_string(),
                    error: None,
                },
                metadata: Some(metadata),
            },
            &context,
        )
        .expect("tool completion should bridge");

        match completed {
            Response::TaskStepEvent { data, payload, .. } => {
                assert_eq!(data["tool_metadata"]["tool_origin"], "provider");
                let payload = payload.expect("payload");
                assert_eq!(payload["provider_native"], true);
                assert_eq!(payload["provider"], "xai");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn direct_task_turn_done_emits_agent_response_step() {
        let execution_run_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let response = turn_event_to_task_step_response(
            &nenjo::TurnEvent::Done {
                output: nenjo::TurnOutput {
                    task_id: Some(task_id),
                    text: "final answer".to_string(),
                    input_tokens: 10,
                    output_tokens: 20,
                    tool_calls: 0,
                    messages: Vec::new(),
                },
            },
            &TaskTurnEventContext {
                execution_run_id,
                task_id: Some(task_id),
                agent: Some(StepAgent {
                    agent: "agent".to_string(),
                    agent_name: Some("agent".to_string()),
                    agent_color: None,
                }),
                routine_step: None,
                agent_duration_ms: Some(123),
                emit_done: true,
                summarize_outputs: false,
            },
        )
        .expect("direct task done should emit a task step");

        match response {
            Response::TaskStepEvent {
                execution_run_id: bridged_execution_run_id,
                task_id: bridged_task_id,
                event_type,
                step_name,
                step_type,
                duration_ms,
                data,
                payload,
                agent,
                ..
            } => {
                assert_eq!(bridged_execution_run_id, execution_run_id.to_string());
                assert_eq!(bridged_task_id, Some(task_id.to_string()));
                assert_eq!(event_type, "step_completed");
                assert_eq!(step_name, "agent_response");
                assert_eq!(step_type, "agent");
                assert_eq!(duration_ms, Some(123));
                assert_eq!(data["input_tokens"], 10);
                assert_eq!(data["output_tokens"], 20);
                assert_eq!(payload.unwrap()["output_preview"], "final answer");
                assert_eq!(agent.unwrap().agent, "agent");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn routine_turn_done_is_suppressed() {
        let response = turn_event_to_task_step_response(
            &nenjo::TurnEvent::Done {
                output: nenjo::TurnOutput {
                    task_id: None,
                    text: "done".to_string(),
                    input_tokens: 1,
                    output_tokens: 2,
                    tool_calls: 0,
                    messages: Vec::new(),
                },
            },
            &TaskTurnEventContext {
                execution_run_id: Uuid::new_v4(),
                task_id: Some(Uuid::new_v4()),
                agent: None,
                routine_step: Some(RoutineStepRef {
                    step_slug: "review".to_string(),
                    step_run_id: Uuid::new_v4(),
                }),
                agent_duration_ms: None,
                emit_done: false,
                summarize_outputs: true,
            },
        );

        assert!(response.is_none());
    }

    #[test]
    fn routine_step_ids_are_only_added_for_routine_turn_events() {
        let execution_run_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let step_slug = "review".to_string();
        let step_run_id = Uuid::new_v4();
        let event = nenjo::TurnEvent::AbilityStarted {
            call_id: "ability-call-1".to_string(),
            ability_tool_name: "ability.review".to_string(),
            ability_name: "Review".to_string(),
            task_input: "inspect".to_string(),
            caller_history: Vec::new(),
        };

        let routine = turn_event_to_task_step_response(
            &event,
            &TaskTurnEventContext {
                execution_run_id,
                task_id: Some(task_id),
                agent: None,
                routine_step: Some(RoutineStepRef {
                    step_slug: step_slug.clone(),
                    step_run_id,
                }),
                agent_duration_ms: None,
                emit_done: false,
                summarize_outputs: true,
            },
        )
        .expect("routine ability start should bridge");

        let direct = turn_event_to_task_step_response(
            &event,
            &TaskTurnEventContext {
                execution_run_id,
                task_id: Some(task_id),
                agent: None,
                routine_step: None,
                agent_duration_ms: None,
                emit_done: true,
                summarize_outputs: false,
            },
        )
        .expect("direct ability start should bridge");

        match routine {
            Response::TaskStepEvent { data, .. } => {
                assert_eq!(data["step_slug"], step_slug);
                assert_eq!(data["step_run_id"], step_run_id.to_string());
            }
            other => panic!("unexpected routine response: {other:?}"),
        }
        match direct {
            Response::TaskStepEvent { data, .. } => {
                assert_eq!(data["call_id"], "ability-call-1");
                assert!(data.get("step_slug").is_none());
                assert!(data.get("step_run_id").is_none());
            }
            other => panic!("unexpected direct response: {other:?}"),
        }
    }

    #[test]
    fn direct_task_tool_output_preview_is_not_summarized() {
        let output = "x".repeat(600);
        let response = turn_event_to_task_step_response(
            &nenjo::TurnEvent::ToolCallEnd {
                batch_id: "batch-1".to_string(),
                parent_tool_name: None,
                tool_call_id: Some("call-1".to_string()),
                tool_name: "tool".to_string(),
                tool_args: "{}".to_string(),
                result: nenjo::ToolResult {
                    success: true,
                    output: output.clone(),
                    error: None,
                },
                metadata: None,
            },
            &TaskTurnEventContext {
                execution_run_id: Uuid::new_v4(),
                task_id: Some(Uuid::new_v4()),
                agent: None,
                routine_step: None,
                agent_duration_ms: None,
                emit_done: true,
                summarize_outputs: false,
            },
        )
        .expect("tool completion should bridge");

        match response {
            Response::TaskStepEvent { payload, .. } => {
                assert_eq!(payload.unwrap()["output_preview"], output);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
}
