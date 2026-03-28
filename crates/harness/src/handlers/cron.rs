//! Cron schedule handlers.

use std::time::Duration;

use anyhow::Result;
use nenjo_events::Response;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;

use super::event_bridge::routine_event_to_response;
use crate::harness::{ActiveExecution, CommandContext};

/// Enable a cron schedule. Keyed by `assignment_id` for cancellation.
pub async fn handle_cron_enable(
    ctx: &CommandContext,
    assignment_id: Uuid,
    routine_id: Uuid,
    project_id: Uuid,
    schedule: &str,
) -> Result<()> {
    info!(%assignment_id, %routine_id, %schedule, "Enabling cron schedule");

    let cancel = CancellationToken::new();
    if let Some((_, prev)) = ctx.executions.remove(&assignment_id) {
        prev.cancel.cancel();
    }
    ctx.executions.insert(
        assignment_id,
        ActiveExecution {
            kind: crate::harness::ExecutionKind::Cron,
            cancel: cancel.clone(),
            pause: None,
        },
    );

    let interval =
        nenjo::routines::types::parse_duration(schedule).unwrap_or(Duration::from_secs(60));

    let task = nenjo::types::TaskType::Cron {
        task: None,
        project_id,
        interval,
        timeout: Duration::from_secs(24 * 3600),
    };

    let provider = ctx.provider();
    let response_tx = ctx.response_tx.clone();
    let executions = ctx.executions.clone();
    let schedule_owned = schedule.to_string();

    // Resolve routine name from manifest for activity logging
    let routine_name = provider
        .manifest()
        .routines
        .iter()
        .find(|r| r.id == routine_id)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| routine_id.to_string());

    // Nil project_id means no project assigned
    let opt_project_id = if project_id.is_nil() {
        None
    } else {
        Some(project_id)
    };

    tokio::spawn(async move {
        let runner = match provider.routine_by_id(routine_id) {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, routine_id = %routine_id, "Cron routine not found");
                return;
            }
        };
        match runner.run_stream(task).await {
            Ok(mut handle) => {
                let mut current_cycle_id: Option<Uuid> = None;

                loop {
                    tokio::select! {
                        event = handle.recv() => {
                            match event {
                                Some(ev) => {
                                    // Intercept cron cycle lifecycle events
                                    match &ev {
                                        nenjo::RoutineEvent::CronCycleStarted { cycle } => {
                                            let cycle_id = Uuid::new_v4();
                                            current_cycle_id = Some(cycle_id);

                                            let _ = response_tx.send(Response::ExecutionStarted {
                                                id: cycle_id,
                                                project_id: opt_project_id,
                                                routine_id,
                                                routine_name: routine_name.clone(),
                                                config: serde_json::json!({
                                                    "trigger": "cron",
                                                    "cycle": cycle,
                                                    "schedule": schedule_owned,
                                                    "assignment_id": assignment_id.to_string(),
                                                }),
                                            });
                                        }
                                        nenjo::RoutineEvent::CronCycleCompleted {
                                            result,
                                            total_input_tokens,
                                            total_output_tokens,
                                            ..
                                        } => {
                                            if let Some(cycle_id) = current_cycle_id.take() {
                                                let _ = response_tx.send(Response::ExecutionCompleted {
                                                    id: cycle_id,
                                                    success: result.passed,
                                                    error: if result.passed {
                                                        None
                                                    } else {
                                                        Some(result.output.clone())
                                                    },
                                                    total_input_tokens: *total_input_tokens,
                                                    total_output_tokens: *total_output_tokens,
                                                });
                                            }
                                        }
                                        _ => {}
                                    }

                                    // Forward step events with the cycle-scoped execution_run_id
                                    let eid = current_cycle_id.unwrap_or(assignment_id);
                                    if let Some(r) = routine_event_to_response(&ev, eid, None) {
                                        let _ = response_tx.send(r);
                                    }
                                }
                                None => break,
                            }
                        }
                        _ = cancel.cancelled() => {
                            handle.cancel();
                            // Mark any in-flight cycle as cancelled
                            if let Some(cycle_id) = current_cycle_id.take() {
                                let _ = response_tx.send(Response::ExecutionCompleted {
                                    id: cycle_id,
                                    success: false,
                                    error: Some("Cron schedule disabled".to_string()),
                                    total_input_tokens: 0,
                                    total_output_tokens: 0,
                                });
                            }
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                error!(%assignment_id, error = %e, "Cron routine execution failed");
            }
        }

        executions.remove(&assignment_id);
    });

    Ok(())
}

/// Disable a cron schedule by assignment_id.
pub async fn handle_cron_disable(ctx: &CommandContext, assignment_id: Uuid) -> Result<()> {
    if let Some((_, exec)) = ctx.executions.remove(&assignment_id) {
        exec.cancel.cancel();
        info!(%assignment_id, "Disabled cron schedule");
    }
    Ok(())
}

/// Trigger a routine manually (one-shot).
pub async fn handle_cron_trigger(
    ctx: &CommandContext,
    routine_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<()> {
    info!(%routine_id, "Manual cron trigger");

    let project_id = project_id.unwrap_or(Uuid::nil());
    let opt_project_id = if project_id.is_nil() {
        None
    } else {
        Some(project_id)
    };

    let routine_name = ctx
        .provider()
        .manifest()
        .routines
        .iter()
        .find(|r| r.id == routine_id)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| routine_id.to_string());

    // Generate execution ID and send lifecycle events around the run
    let execution_id = Uuid::new_v4();
    let _ = ctx.response_tx.send(Response::ExecutionStarted {
        id: execution_id,
        project_id: opt_project_id,
        routine_id,
        routine_name,
        config: serde_json::json!({
            "trigger": "cron",
            "manual": true,
        }),
    });

    let task = nenjo::types::TaskType::Cron {
        task: None,
        project_id,
        interval: Duration::from_secs(0),
        timeout: Duration::from_secs(0),
    };

    let result = async { ctx.provider().routine_by_id(routine_id)?.run(task).await }.await;

    let (success, error, total_input_tokens, total_output_tokens) = match &result {
        Ok(r) => (
            r.passed,
            if r.passed {
                None
            } else {
                Some(r.output.clone())
            },
            r.input_tokens,
            r.output_tokens,
        ),
        Err(e) => (false, Some(e.to_string()), 0, 0),
    };

    let _ = ctx.response_tx.send(Response::ExecutionCompleted {
        id: execution_id,
        success,
        error,
        total_input_tokens,
        total_output_tokens,
    });

    info!(%routine_id, %execution_id, success, "Manual trigger complete");
    result?;
    Ok(())
}
