//! Cron schedule handlers.

use std::time::Duration;

use anyhow::Result;
use nenjo_events::{CronScheduleStatus, Response};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;

use super::event_bridge::routine_event_to_response;
use crate::harness::{ActiveExecution, CommandContext};

fn emit_cron_heartbeat(
    response_tx: &tokio::sync::mpsc::UnboundedSender<Response>,
    routine_id: Uuid,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_fire_at: chrono::DateTime<chrono::Utc>,
) {
    let _ = response_tx.send(Response::CronHeartbeat {
        active_schedules: vec![CronScheduleStatus {
            routine_id: routine_id.to_string(),
            last_run_at: last_run_at.map(|ts| ts.to_rfc3339()),
            next_fire_at: Some(next_fire_at.to_rfc3339()),
        }],
    });
}

/// Enable a cron schedule. Keyed by `routine_id` for cancellation.
pub async fn handle_cron_enable(
    ctx: &CommandContext,
    routine_id: Uuid,
    project_id: Option<Uuid>,
    schedule: &str,
    start_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    info!(%routine_id, %schedule, "Enabling cron schedule");

    let cron_schedule = nenjo::routines::types::parse_schedule(schedule).map_err(|e| {
        error!(%routine_id, error = %e, "Invalid cron schedule");
        e
    })?;

    let cancel = CancellationToken::new();
    if let Some((_, prev)) = ctx.executions.remove(&routine_id) {
        prev.cancel.cancel();
    }
    let registry_token = Uuid::new_v4();
    ctx.executions.insert(
        routine_id,
        ActiveExecution {
            kind: crate::harness::ExecutionKind::Cron,
            registry_token,
            execution_run_id: None,
            cancel: cancel.clone(),
            pause: None,
        },
    );

    let task = nenjo::types::TaskType::Cron {
        task: None,
        project_id: project_id.unwrap_or(Uuid::nil()),
        schedule: cron_schedule.clone(),
        start_at: None,
        timeout: Duration::from_secs(24 * 3600),
    };

    let response_tx = ctx.response_tx.clone();
    let executions = ctx.executions.clone();
    let schedule_owned = schedule.to_string();
    let provider_cell = ctx.provider.clone();

    // Resolve routine name from manifest for activity logging
    let routine_name = ctx
        .provider()
        .manifest()
        .routines
        .iter()
        .find(|r| r.id == routine_id)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| routine_id.to_string());

    let opt_project_id = project_id;

    tokio::spawn(async move {
        let mut last_run_at: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut next_run_at = start_at.unwrap_or_else(|| cron_schedule.next_fire_at());

        emit_cron_heartbeat(&response_tx, routine_id, None, next_run_at);
        let _ = response_tx.send(Response::CronScheduled {
            routine_id,
            next_run_at: Some(next_run_at.to_rfc3339()),
        });

        loop {
            let delay = (next_run_at - chrono::Utc::now())
                .to_std()
                .unwrap_or(Duration::ZERO);

            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => break,
            }

            let provider = provider_cell.load_full();
            match provider.routine_by_id(routine_id) {
                Ok(runner) => match runner.run_stream(task.clone()).await {
                    Ok(mut handle) => {
                        let mut current_cycle_id: Option<Uuid> = None;
                        let mut current_agent_id: Option<Uuid> = None;
                        let mut cycle_completed = false;
                        let mut schedule_cancelled = false;

                        loop {
                            tokio::select! {
                                event = handle.recv() => {
                                    match event {
                                        Some(ev) => {
                                            if let nenjo::RoutineEvent::StepStarted { agent_id, .. } = &ev {
                                                current_agent_id = *agent_id;
                                            }

                                            match &ev {
                                                nenjo::RoutineEvent::CronCycleStarted { cycle } => {
                                                    let cycle_id = Uuid::new_v4();
                                                    current_cycle_id = Some(cycle_id);

                                                    let _ = response_tx.send(Response::ExecutionStarted {
                                                        id: cycle_id,
                                                        project_id: opt_project_id,
                                                        routine_id: Some(routine_id),
                                                        routine_name: Some(routine_name.clone()),
                                                        agent_id: None,
                                                        config: serde_json::json!({
                                                            "trigger": "cron",
                                                            "cycle": cycle,
                                                            "schedule": schedule_owned,
                                                            "routine_id": routine_id.to_string(),
                                                        }),
                                                    });
                                                }
                                                nenjo::RoutineEvent::CronCycleCompleted {
                                                    result,
                                                    total_input_tokens,
                                                    total_output_tokens,
                                                    ..
                                                } => {
                                                    cycle_completed = true;
                                                    if let Some(cycle_id) = current_cycle_id.take() {
                                                        let completed_at = chrono::Utc::now();
                                                        last_run_at = Some(completed_at);
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
                                                            execution_type: Some(nenjo_events::ExecutionType::Cron),
                                                            routine_id: Some(routine_id),
                                                            routine_name: Some(routine_name.clone()),
                                                            agent_id: None,
                                                        });
                                                    }
                                                }
                                                _ => {}
                                            }

                                            let eid = current_cycle_id.unwrap_or(routine_id);
                                            if let Some(r) = routine_event_to_response(
                                                &ev,
                                                eid,
                                                None,
                                                current_agent_id,
                                                provider.manifest(),
                                            ) {
                                                let _ = response_tx.send(r);
                                            }
                                        }
                                        None => break,
                                    }
                                }
                                _ = cancel.cancelled() => {
                                    handle.cancel();
                                    schedule_cancelled = true;
                                    if let Some(cycle_id) = current_cycle_id.take() {
                                        let _ = response_tx.send(Response::ExecutionCompleted {
                                            id: cycle_id,
                                            success: false,
                                            error: Some("Cron schedule disabled".to_string()),
                                            total_input_tokens: 0,
                                            total_output_tokens: 0,
                                            execution_type: Some(nenjo_events::ExecutionType::Cron),
                                            routine_id: Some(routine_id),
                                            routine_name: Some(routine_name.clone()),
                                            agent_id: None,
                                        });
                                    }
                                    break;
                                }
                            }
                        }

                        if schedule_cancelled {
                            break;
                        }

                        if !cycle_completed {
                            error!(%routine_id, "Cron routine stream ended before cycle completion");
                        }
                    }
                    Err(e) => {
                        error!(%routine_id, error = %e, "Cron routine execution failed");
                    }
                },
                Err(e) => {
                    error!(error = %e, routine_id = %routine_id, "Cron routine not found");
                }
            }

            next_run_at = cron_schedule.next_fire_at();
            emit_cron_heartbeat(&response_tx, routine_id, last_run_at, next_run_at);
        }

        if executions
            .get(&routine_id)
            .is_some_and(|entry| entry.registry_token == registry_token)
        {
            executions.remove(&routine_id);
        }
    });

    Ok(())
}

/// Disable a cron schedule by routine_id.
pub async fn handle_cron_disable(ctx: &CommandContext, routine_id: Uuid) -> Result<()> {
    if let Some((_, exec)) = ctx.executions.remove(&routine_id) {
        exec.cancel.cancel();
        let _ = ctx.response_tx.send(Response::CronStopped { routine_id });
        info!(%routine_id, "Disabled cron schedule");
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
        routine_id: Some(routine_id),
        routine_name: Some(routine_name.clone()),
        agent_id: None,
        config: serde_json::json!({
            "trigger": "cron",
            "manual": true,
        }),
    });

    let task = nenjo::types::TaskType::Cron {
        task: None,
        project_id,
        schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_secs(0)),
        start_at: None,
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
        execution_type: Some(nenjo_events::ExecutionType::Cron),
        routine_id: Some(routine_id),
        routine_name: Some(routine_name),
        agent_id: None,
    });

    info!(%routine_id, %execution_id, success, "Manual trigger complete");
    result?;
    Ok(())
}
