//! Event bridging — converts SDK events to NATS response types.

use nenjo::manifest::Manifest;
use nenjo_events::{Response, StepAgent, StreamEvent};
use serde::Serialize;
use uuid::Uuid;

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
    match event {
        nenjo::TurnEvent::ToolCallStart { calls } => {
            if calls.len() == 1 {
                Some(StreamEvent::ToolInvoked {
                    tool_name: calls[0].tool_name.clone(),
                    tool_args: calls[0].tool_args.clone(),
                    agent_name: agent_name.to_string(),
                })
            } else {
                Some(StreamEvent::ToolsInvoked {
                    tool_names: calls.iter().map(|c| c.tool_name.clone()).collect(),
                    tool_args: calls.iter().map(|c| c.tool_args.clone()).collect(),
                    agent_name: agent_name.to_string(),
                })
            }
        }
        nenjo::TurnEvent::ToolCallEnd { .. } => None,
        nenjo::TurnEvent::Paused => Some(StreamEvent::Paused),
        nenjo::TurnEvent::Resumed => Some(StreamEvent::Resumed),
        nenjo::TurnEvent::Done { output } => Some(StreamEvent::Done {
            final_output: output.text.clone(),
        }),
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
        nenjo::RoutineEvent::AgentEvent { event, .. } => turn_event_to_stream_event(event, "agent")
            .map(|se| Response::AgentResponse { payload: se }),
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
