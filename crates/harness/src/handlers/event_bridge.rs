//! Event bridging — converts SDK events to NATS response types.

use nenjo::manifest::Manifest;
use nenjo_events::{Response, StreamEvent};
use uuid::Uuid;

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

/// Convert a RoutineEvent to a NATS Response.
pub fn routine_event_to_response(
    event: &nenjo::RoutineEvent,
    execution_run_id: Uuid,
    task_id: Option<Uuid>,
) -> Option<Response> {
    let eid = execution_run_id.to_string();
    let tid = task_id.map(|id| id.to_string());

    match event {
        nenjo::RoutineEvent::StepStarted {
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
            data: serde_json::Value::Null,
        }),
        nenjo::RoutineEvent::StepCompleted { result, .. } => {
            let output_preview = if result.output.len() > 500 {
                format!("{}...", &result.output[..500])
            } else {
                result.output.clone()
            };

            let mut data = serde_json::json!({
                "passed": result.passed,
                "output_preview": output_preview,
                "input_tokens": result.input_tokens,
                "output_tokens": result.output_tokens,
            });

            // Surface structured gate verdict data (verdict + reasoning)
            // so the frontend can display why a gate passed or failed.
            if let Some(verdict) = result.data.get("verdict").and_then(|v| v.as_str()) {
                data["verdict"] = serde_json::json!(verdict);
            }
            if let Some(reasoning) = result.data.get("reasoning").and_then(|v| v.as_str()) {
                data["reasoning"] = serde_json::json!(reasoning);
            }

            Some(Response::TaskStepEvent {
                execution_run_id: eid,
                task_id: tid,
                event_type: "step_completed".to_string(),
                step_name: result.step_name.clone(),
                step_type: String::new(),
                duration_ms: None,
                data,
            })
        }
        nenjo::RoutineEvent::StepFailed { error, .. } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "step_failed".to_string(),
            step_name: String::new(),
            step_type: String::new(),
            duration_ms: None,
            data: serde_json::json!({ "error": error }),
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
            data: serde_json::json!({ "cycle": cycle }),
        }),
        nenjo::RoutineEvent::CronCycleCompleted { cycle, result, .. } => {
            Some(Response::TaskStepEvent {
                execution_run_id: eid,
                task_id: tid,
                event_type: "cron_cycle_completed".to_string(),
                step_name: format!("cycle-{cycle}"),
                step_type: "cron".to_string(),
                duration_ms: None,
                data: serde_json::json!({ "cycle": cycle, "passed": result.passed }),
            })
        }
    }
}

/// Get project slug from manifest, falling back to UUID string.
pub fn project_slug(manifest: &Manifest, project_id: Uuid) -> String {
    manifest
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .map(|p| p.slug.clone())
        .unwrap_or_else(|| project_id.to_string())
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
