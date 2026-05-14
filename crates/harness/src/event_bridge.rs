//! Event bridging — converts runtime events to NATS response types.

use nenjo::manifest::Manifest;
use nenjo_events::{Response, StepAgent, StreamEvent, ToolCall};
use serde::Serialize;
use tracing::{debug, trace};
use uuid::Uuid;

use crate::preview::{summarize_preview, truncate_preview};

// ---------------------------------------------------------------------------
// Typed step-event data payloads
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct StepCompletedData {
    pub step_id: Uuid,
    pub step_run_id: Uuid,
    pub passed: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Serialize)]
pub struct StepFailedData {
    pub step_id: Uuid,
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

/// Convert a TurnEvent to a StreamEvent for the frontend.
pub fn turn_event_to_stream_event(
    event: &nenjo::TurnEvent,
    agent_name: &str,
) -> Option<StreamEvent> {
    let stream_event = match event {
        nenjo::TurnEvent::AbilityStarted {
            ability_tool_name,
            ability_name,
            task_input,
            ..
        } => Some(StreamEvent::AbilityActivated {
            agent: agent_name.to_string(),
            ability: ability_name.clone(),
            ability_tool_name: ability_tool_name.clone(),
            payload: Some(serde_json::json!({
                "task_preview": task_input,
            })),
            encrypted_payload: None,
        }),
        nenjo::TurnEvent::ToolCallStart {
            parent_tool_name,
            calls,
        } => Some(StreamEvent::ToolCalls {
            tool_calls: calls
                .iter()
                .map(|call| ToolCall {
                    tool_name: call.tool_name.clone(),
                    tool_args: call.tool_args.clone(),
                })
                .collect(),
            agent_name: agent_name.to_string(),
            parent_tool_name: parent_tool_name.clone(),
            payload: Some(serde_json::json!({
                "text_preview": calls.first().and_then(|c| c.text_preview.clone()),
            })),
            encrypted_payload: None,
        }),
        nenjo::TurnEvent::ToolCallEnd {
            parent_tool_name,
            tool_name,
            tool_args,
            result,
            ..
        } => Some(StreamEvent::ToolCompleted {
            tool_name: tool_name.clone(),
            success: result.success,
            parent_tool_name: parent_tool_name.clone(),
            payload: Some(serde_json::json!({
                "tool_args": tool_args,
                "output_preview": summarize_preview(&result.output),
                "error_preview": result.error.as_deref().and_then(summarize_preview),
            })),
            encrypted_payload: None,
        }),
        nenjo::TurnEvent::AbilityCompleted {
            ability_tool_name,
            ability_name,
            success,
            final_output,
        } => Some(StreamEvent::AbilityCompleted {
            agent: agent_name.to_string(),
            ability: ability_name.clone(),
            ability_tool_name: ability_tool_name.clone(),
            success: *success,
            payload: Some(serde_json::json!({
                "result_preview": final_output,
            })),
            encrypted_payload: None,
        }),
        nenjo::TurnEvent::DelegationStarted {
            delegate_tool_name,
            target_agent_name,
            target_agent_id,
            task_input,
            ..
        } => Some(StreamEvent::DelegationStarted {
            agent: agent_name.to_string(),
            target_agent: target_agent_name.clone(),
            target_agent_id: *target_agent_id,
            delegate_tool_name: delegate_tool_name.clone(),
            payload: Some(serde_json::json!({
                "task_preview": task_input,
            })),
            encrypted_payload: None,
        }),
        nenjo::TurnEvent::DelegationCompleted {
            delegate_tool_name,
            target_agent_name,
            target_agent_id,
            success,
            final_output,
        } => Some(StreamEvent::DelegationCompleted {
            agent: agent_name.to_string(),
            target_agent: target_agent_name.clone(),
            target_agent_id: *target_agent_id,
            delegate_tool_name: delegate_tool_name.clone(),
            success: *success,
            payload: Some(serde_json::json!({
                "result_preview": final_output,
            })),
            encrypted_payload: None,
        }),
        nenjo::TurnEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => Some(StreamEvent::MessageCompacted {
            messages_before: *messages_before,
            messages_after: *messages_after,
        }),
        nenjo::TurnEvent::TranscriptMessage { .. } => None,
        nenjo::TurnEvent::Paused => Some(StreamEvent::Paused),
        nenjo::TurnEvent::Resumed => Some(StreamEvent::Resumed),
        nenjo::TurnEvent::Done { output } => Some(StreamEvent::Done {
            payload: Some(serde_json::Value::String(output.text.clone())),
            encrypted_payload: None,
            total_input_tokens: output.input_tokens,
            total_output_tokens: output.output_tokens,
            project_id: None,
            agent_id: None,
            session_id: None,
        }),
    };

    if let Some(ref stream_event) = stream_event {
        trace!(
            turn_event = %summarize_turn_event(event),
            stream_event = %summarize_stream_event(stream_event),
            agent = agent_name,
            "Bridged turn event to pre-codec stream event"
        );
    } else {
        debug!(
            turn_event = %summarize_turn_event(event),
            agent = agent_name,
            "Turn event did not produce a stream event"
        );
    }

    stream_event
}

pub fn summarize_turn_event(event: &nenjo::TurnEvent) -> String {
    match event {
        nenjo::TurnEvent::AbilityStarted {
            ability_tool_name,
            ability_name,
            task_input,
            caller_history,
        } => format!(
            "ability_started(tool={ability_tool_name}, ability={ability_name}, task_preview={:?}, task_len={}, caller_messages={})",
            truncate_preview(task_input, 80),
            task_input.len(),
            caller_history.len()
        ),
        nenjo::TurnEvent::DelegationStarted {
            delegate_tool_name,
            target_agent_name,
            task_input,
            caller_history,
            ..
        } => format!(
            "delegation_started(tool={delegate_tool_name}, target={target_agent_name}, task_preview={:?}, task_len={}, caller_messages={})",
            truncate_preview(task_input, 80),
            task_input.len(),
            caller_history.len()
        ),
        nenjo::TurnEvent::ToolCallStart {
            parent_tool_name,
            calls,
        } => format!(
            "tool_call_start(parent={}, tools=[{}], count={})",
            parent_tool_name.as_deref().unwrap_or("-"),
            calls
                .iter()
                .map(|call| call.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            calls.len()
        ),
        nenjo::TurnEvent::ToolCallEnd {
            parent_tool_name,
            tool_name,
            tool_args,
            result,
            ..
        } => format!(
            "tool_call_end(parent={}, tool={tool_name}, args_len={}, success={}, output_len={}, error={})",
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
            ability_tool_name,
            ability_name,
            success,
            final_output,
        } => format!(
            "ability_completed(tool={ability_tool_name}, ability={ability_name}, success={success}, output_len={})",
            final_output.len()
        ),
        nenjo::TurnEvent::DelegationCompleted {
            delegate_tool_name,
            target_agent_name,
            success,
            final_output,
            ..
        } => format!(
            "delegation_completed(tool={delegate_tool_name}, target={target_agent_name}, success={success}, output_len={})",
            final_output.len()
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
        StreamEvent::ToolCalls {
            tool_calls,
            agent_name,
            parent_tool_name,
            ..
        } => format!(
            "tool_calls(count={}, agent={agent_name}, parent={}, tools=[{}])",
            tool_calls.len(),
            parent_tool_name.as_deref().unwrap_or("-"),
            tool_calls
                .iter()
                .map(|call| call.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        StreamEvent::ToolCompleted {
            tool_name, success, ..
        } => format!("tool_completed(tool={tool_name}, success={success})"),
        StreamEvent::AbilityActivated {
            agent,
            ability,
            ability_tool_name,
            ..
        } => {
            format!("ability_activated(agent={agent}, ability={ability}, tool={ability_tool_name})")
        }
        StreamEvent::AbilityCompleted {
            agent,
            ability,
            ability_tool_name,
            success,
            ..
        } => format!(
            "ability_completed(agent={agent}, ability={ability}, tool={ability_tool_name}, success={success})"
        ),
        StreamEvent::DelegationStarted {
            agent,
            target_agent,
            delegate_tool_name,
            ..
        } => format!(
            "delegation_started(agent={agent}, target={target_agent}, tool={delegate_tool_name})"
        ),
        StreamEvent::DelegationCompleted {
            agent,
            target_agent,
            delegate_tool_name,
            success,
            ..
        } => format!(
            "delegation_completed(agent={agent}, target={target_agent}, tool={delegate_tool_name}, success={success})"
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
            project_id,
            agent_id,
            session_id,
        } => format!(
            "done(payload={}, encrypted={}, input_tokens={}, output_tokens={}, project_id={}, agent_id={}, session_id={})",
            if payload.is_some() { "yes" } else { "no" },
            encrypted_payload.is_some(),
            total_input_tokens,
            total_output_tokens,
            project_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            agent_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
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
        let a = manifest.agents.iter().find(|a| a.id == aid);
        StepAgent {
            agent_id: aid,
            agent_name: a.map(|a| a.name.clone()),
            agent_color: a.and_then(|a| a.color.clone()),
        }
    })
}

#[derive(Debug, Clone, Copy)]
pub struct RoutineStepRef {
    pub step_id: Uuid,
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
            "step_id".to_string(),
            serde_json::Value::String(routine_step.step_id.to_string()),
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
                context.routine_step,
                serde_json::Map::from_iter([
                    (
                        "parent_tool_name".to_string(),
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
                ]),
            ),
            payload: calls.first().and_then(|c| {
                c.text_preview
                    .as_ref()
                    .map(|preview| serde_json::json!({ "text_preview": preview }))
            }),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::ToolCallEnd {
            parent_tool_name,
            tool_call_id,
            tool_name,
            tool_args,
            result,
            ..
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
                context.routine_step,
                serde_json::Map::from_iter([
                    (
                        "parent_tool_name".to_string(),
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
                ]),
            ),
            payload: Some(serde_json::json!({
                "output_preview": task_output_preview(&result.output, context.summarize_outputs),
                "error": result.error,
            })),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::AbilityStarted {
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
            data: context
                .routine_step
                .map(|step| task_data(Some(step), serde_json::Map::new()))
                .unwrap_or(serde_json::Value::Null),
            payload: Some(serde_json::json!({
                "task_preview": task_input,
            })),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::AbilityCompleted {
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
                context.routine_step,
                serde_json::Map::from_iter([(
                    "success".to_string(),
                    serde_json::Value::Bool(*success),
                )]),
            ),
            payload: Some(serde_json::json!({
                "output_preview": final_output,
            })),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::DelegationStarted {
            target_agent_name,
            task_input,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id,
            task_id,
            event_type: "step_started".to_string(),
            step_name: target_agent_name.clone(),
            step_type: "delegation".to_string(),
            duration_ms: None,
            data: context
                .routine_step
                .map(|step| task_data(Some(step), serde_json::Map::new()))
                .unwrap_or(serde_json::Value::Null),
            payload: Some(serde_json::json!({
                "task_preview": task_input,
            })),
            encrypted_payload: None,
            agent,
        }),
        nenjo::TurnEvent::DelegationCompleted {
            target_agent_name,
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
            step_name: target_agent_name.clone(),
            step_type: "delegation".to_string(),
            duration_ms: None,
            data: task_data(
                context.routine_step,
                serde_json::Map::from_iter([(
                    "success".to_string(),
                    serde_json::Value::Bool(*success),
                )]),
            ),
            payload: Some(serde_json::json!({
                "output_preview": final_output,
            })),
            encrypted_payload: None,
            agent,
        }),
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
            step_id,
            step_run_id,
            step_name,
            step_type,
            agent_id,
            ..
        } => {
            let agent = resolve_agent(manifest, *agent_id);
            Some(Response::TaskStepEvent {
                execution_run_id: eid,
                task_id: tid,
                event_type: "step_started".to_string(),
                step_name: step_name.clone(),
                step_type: step_type.clone(),
                duration_ms: None,
                data: serde_json::json!({ "step_id": step_id, "step_run_id": step_run_id }),
                payload: None,
                encrypted_payload: None,
                agent,
            })
        }
        nenjo::RoutineEvent::StepCompleted {
            step_id,
            step_run_id,
            result,
            duration_ms,
            ..
        } => {
            let data = StepCompletedData {
                step_id: *step_id,
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
            step_id,
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
                step_id: *step_id,
                step_run_id: *step_run_id,
                error: "Step failed",
            })
            .unwrap_or_default(),
            payload: Some(serde_json::json!({ "error": error })),
            encrypted_payload: None,
            agent: None,
        }),
        nenjo::RoutineEvent::AgentEvent {
            step_id,
            step_run_id,
            event,
        } => routine_agent_event_to_response(
            event,
            execution_run_id,
            task_id,
            *step_id,
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
    routine_step_id: Uuid,
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
                step_id: routine_step_id,
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
    match manifest.projects.iter().find(|p| p.id == project_id) {
        Some(p) => p.slug.clone(),
        None => project_id.to_string(),
    }
}

/// Get agent name from manifest, falling back to UUID string.
pub fn agent_name(manifest: &Manifest, agent_id: Uuid) -> String {
    manifest
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| agent_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo_models::ChatMessage;

    #[test]
    fn bridges_delegation_lifecycle_to_stream_events() {
        let target_agent_id = Uuid::new_v4();

        let started = turn_event_to_stream_event(
            &nenjo::TurnEvent::DelegationStarted {
                delegate_tool_name: "delegate_to".to_string(),
                target_agent_name: "specialist".to_string(),
                target_agent_id,
                task_input: "review this".to_string(),
                caller_history: vec![ChatMessage::user("review this")],
            },
            "leader",
        )
        .expect("delegation start should bridge");

        match started {
            StreamEvent::DelegationStarted {
                agent,
                target_agent,
                target_agent_id: bridged_id,
                delegate_tool_name,
                payload,
                ..
            } => {
                assert_eq!(agent, "leader");
                assert_eq!(target_agent, "specialist");
                assert_eq!(bridged_id, target_agent_id);
                assert_eq!(delegate_tool_name, "delegate_to");
                assert_eq!(payload.unwrap()["task_preview"], "review this");
            }
            other => panic!("unexpected stream event: {other:?}"),
        }

        let completed = turn_event_to_stream_event(
            &nenjo::TurnEvent::DelegationCompleted {
                delegate_tool_name: "delegate_to".to_string(),
                target_agent_name: "specialist".to_string(),
                target_agent_id,
                success: true,
                final_output: "done".to_string(),
            },
            "leader",
        )
        .expect("delegation completion should bridge");

        match completed {
            StreamEvent::DelegationCompleted {
                agent,
                target_agent,
                target_agent_id: bridged_id,
                delegate_tool_name,
                success,
                payload,
                ..
            } => {
                assert_eq!(agent, "leader");
                assert_eq!(target_agent, "specialist");
                assert_eq!(bridged_id, target_agent_id);
                assert_eq!(delegate_tool_name, "delegate_to");
                assert!(success);
                assert_eq!(payload.unwrap()["result_preview"], "done");
            }
            other => panic!("unexpected stream event: {other:?}"),
        }
    }

    #[test]
    fn direct_task_turn_done_emits_agent_response_step() {
        let execution_run_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let response = turn_event_to_task_step_response(
            &nenjo::TurnEvent::Done {
                output: nenjo::TurnOutput {
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
                    agent_id,
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
                assert_eq!(agent.unwrap().agent_id, agent_id);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn routine_turn_done_is_suppressed() {
        let response = turn_event_to_task_step_response(
            &nenjo::TurnEvent::Done {
                output: nenjo::TurnOutput {
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
                    step_id: Uuid::new_v4(),
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
        let step_id = Uuid::new_v4();
        let step_run_id = Uuid::new_v4();
        let event = nenjo::TurnEvent::AbilityStarted {
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
                    step_id,
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
                assert_eq!(data["step_id"], step_id.to_string());
                assert_eq!(data["step_run_id"], step_run_id.to_string());
            }
            other => panic!("unexpected routine response: {other:?}"),
        }
        match direct {
            Response::TaskStepEvent { data, .. } => assert!(data.is_null()),
            other => panic!("unexpected direct response: {other:?}"),
        }
    }

    #[test]
    fn direct_task_tool_output_preview_is_not_summarized() {
        let output = "x".repeat(600);
        let response = turn_event_to_task_step_response(
            &nenjo::TurnEvent::ToolCallEnd {
                parent_tool_name: None,
                tool_call_id: Some("call-1".to_string()),
                tool_name: "tool".to_string(),
                tool_args: "{}".to_string(),
                result: nenjo::ToolResult {
                    success: true,
                    output: output.clone(),
                    error: None,
                },
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
