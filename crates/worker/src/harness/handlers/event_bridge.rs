//! Event bridging — converts SDK events to NATS response types.

use nenjo::manifest::Manifest;
use nenjo_events::{Response, StepAgent, StreamEvent, ToolCall};
use serde::Serialize;
use tracing::debug;
use uuid::Uuid;

use crate::harness::preview::{summarize_preview, truncate_preview};

// ---------------------------------------------------------------------------
// Typed step-event data payloads
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct StepCompletedData {
    pub passed: bool,
    pub output_preview: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Serialize)]
pub struct StepFailedData {
    pub error: String,
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
            task_preview: task_input.clone(),
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
            text_preview: calls.first().and_then(|c| c.text_preview.clone()),
            parent_tool_name: parent_tool_name.clone(),
        }),
        nenjo::TurnEvent::ToolCallEnd {
            parent_tool_name,
            tool_name,
            result,
        } => Some(StreamEvent::ToolCompleted {
            tool_name: tool_name.clone(),
            success: result.success,
            output_preview: summarize_preview(&result.output),
            error_preview: result.error.as_deref().and_then(summarize_preview),
            parent_tool_name: parent_tool_name.clone(),
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
            result_preview: final_output.clone(),
        }),
        nenjo::TurnEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => Some(StreamEvent::MessageCompacted {
            messages_before: *messages_before,
            messages_after: *messages_after,
        }),
        nenjo::TurnEvent::Paused => Some(StreamEvent::Paused),
        nenjo::TurnEvent::Resumed => Some(StreamEvent::Resumed),
        nenjo::TurnEvent::Done { output } => Some(StreamEvent::Done {
            final_output: output.text.clone(),
            project_id: None,
            agent_id: None,
            session_id: None,
        }),
    };

    if let Some(ref stream_event) = stream_event {
        debug!(
            turn_event = %summarize_turn_event(event),
            stream_event = %summarize_stream_event(stream_event),
            agent = agent_name,
            "Bridged turn event to stream event"
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

pub(crate) fn summarize_turn_event(event: &nenjo::TurnEvent) -> String {
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
            result,
        } => format!(
            "tool_call_end(parent={}, tool={tool_name}, success={}, output_len={}, error={})",
            parent_tool_name.as_deref().unwrap_or("-"),
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
        nenjo::TurnEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => format!("message_compacted({messages_before}->{messages_after})"),
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

pub(crate) fn summarize_stream_event(event: &StreamEvent) -> String {
    match event {
        StreamEvent::Token { text } => format!("token(len={})", text.len()),
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
            task_preview,
        } => format!(
            "ability_activated(agent={agent}, ability={ability}, tool={ability_tool_name}, task_len={})",
            task_preview.len()
        ),
        StreamEvent::AbilityCompleted {
            agent,
            ability,
            ability_tool_name,
            success,
            result_preview,
        } => format!(
            "ability_completed(agent={agent}, ability={ability}, tool={ability_tool_name}, success={success}, result_len={})",
            result_preview.len()
        ),
        StreamEvent::Error { message } => {
            format!(
                "error(message={:?}, len={})",
                truncate_preview(message, 80),
                message.len()
            )
        }
        StreamEvent::Done {
            final_output,
            project_id,
            agent_id,
            session_id,
        } => format!(
            "done(output_len={}, project_id={}, agent_id={}, session_id={})",
            final_output.len(),
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
                data: serde_json::Value::Null,
                agent,
            })
        }
        nenjo::RoutineEvent::StepCompleted {
            result,
            duration_ms,
            ..
        } => {
            let output_preview = match result.output.char_indices().nth(500) {
                Some((idx, _)) => format!("{}...", &result.output[..idx]),
                None => result.output.clone(),
            };

            let data = StepCompletedData {
                passed: result.passed,
                output_preview,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                verdict: result
                    .data
                    .get("verdict")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                reasoning: result
                    .data
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            };

            Some(Response::TaskStepEvent {
                execution_run_id: eid,
                task_id: tid,
                event_type: "step_completed".to_string(),
                step_name: result.step_name.clone(),
                step_type: String::new(),
                duration_ms: Some(*duration_ms),
                data: serde_json::to_value(data).unwrap_or_default(),
                agent: resolve_agent(manifest, current_agent_id),
            })
        }
        nenjo::RoutineEvent::StepFailed {
            error, duration_ms, ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "step_failed".to_string(),
            step_name: String::new(),
            step_type: String::new(),
            duration_ms: Some(*duration_ms),
            data: serde_json::to_value(StepFailedData {
                error: error.clone(),
            })
            .unwrap_or_default(),
            agent: None,
        }),
        nenjo::RoutineEvent::AgentEvent { event, .. } => routine_agent_event_to_response(
            event,
            execution_run_id,
            task_id,
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
                agent: None,
            })
        }
    }
}

fn routine_agent_event_to_response(
    event: &nenjo::TurnEvent,
    execution_run_id: Uuid,
    task_id: Option<Uuid>,
    current_agent_id: Option<Uuid>,
    manifest: &Manifest,
) -> Option<Response> {
    let agent = resolve_agent(manifest, current_agent_id);

    match event {
        nenjo::TurnEvent::ToolCallStart {
            parent_tool_name,
            calls,
        } => Some(Response::TaskStepEvent {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            event_type: "step_started".to_string(),
            step_name: calls
                .first()
                .map(|c| c.tool_name.clone())
                .unwrap_or_else(|| "tool_call".to_string()),
            step_type: "tool".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "parent_tool_name": parent_tool_name,
                "tool_names": calls.iter().map(|c| c.tool_name.clone()).collect::<Vec<_>>(),
                "text_preview": calls.first().and_then(|c| c.text_preview.clone()),
            }),
            agent,
        }),
        nenjo::TurnEvent::ToolCallEnd {
            parent_tool_name,
            tool_name,
            result,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            event_type: if result.success {
                "step_completed".to_string()
            } else {
                "step_failed".to_string()
            },
            step_name: tool_name.clone(),
            step_type: "tool".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "parent_tool_name": parent_tool_name,
                "success": result.success,
                "output_preview": summarize_preview(&result.output),
                "error": result.error,
            }),
            agent,
        }),
        nenjo::TurnEvent::AbilityStarted {
            ability_name,
            task_input,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            event_type: "step_started".to_string(),
            step_name: ability_name.clone(),
            step_type: "ability".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "task_preview": task_input,
            }),
            agent,
        }),
        nenjo::TurnEvent::AbilityCompleted {
            ability_name,
            success,
            final_output,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            event_type: if *success {
                "step_completed".to_string()
            } else {
                "step_failed".to_string()
            },
            step_name: ability_name.clone(),
            step_type: "ability".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "success": success,
                "output_preview": summarize_preview(final_output),
            }),
            agent,
        }),
        // The enclosing routine step already emits StepCompleted/StepFailed.
        // Suppress the nested agent Done event here to avoid a duplicate
        // synthetic "agent_response" step in task timelines.
        nenjo::TurnEvent::Done { .. } => None,
        nenjo::TurnEvent::MessageCompacted { .. } => None,
        nenjo::TurnEvent::Paused | nenjo::TurnEvent::Resumed => None,
    }
}

/// Get project slug from manifest, falling back to UUID string.
pub fn project_slug(manifest: &Manifest, project_id: Uuid) -> String {
    if project_id.is_nil() {
        return String::new();
    }
    match manifest.projects.iter().find(|p| p.id == project_id) {
        Some(p) if p.is_system => String::new(),
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
