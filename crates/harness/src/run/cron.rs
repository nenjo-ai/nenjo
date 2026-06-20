//! Platform-free cron routine scheduling.

use anyhow::anyhow;
use chrono::Utc;
use nenjo::{CronInput, RoutineRun};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::events::HarnessScheduleEvent;
use crate::handle::HarnessScheduleHandle;
use crate::registry::{ActiveExecution, ExecutionKind};
use crate::request::CronRequest;
use crate::{Harness, ProviderRuntime};

pub(crate) async fn cron<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: CronRequest,
) -> crate::Result<HarnessScheduleHandle>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let schedule = nenjo::routines::types::parse_schedule_in_timezone(
        &request.schedule,
        request.timezone.as_deref(),
    )?;
    let next_run_at = request.start_at.unwrap_or_else(|| schedule.next_fire_at());
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let registry_token = Uuid::new_v4();
    let schedule_id = request.execution_run_id.unwrap_or_else(Uuid::new_v4);

    if let Some((_, previous)) = harness.executions().remove(&schedule_id) {
        previous.cancel.cancel();
    }
    harness.executions().insert(
        schedule_id,
        ActiveExecution {
            kind: ExecutionKind::Cron,
            registry_token,
            execution_run_id: request.execution_run_id,
            cancel: cancel.clone(),
            pause: None,
            turn_input: None,
        },
    );

    let harness = harness.clone();
    let join_cancel = cancel.clone();
    let join = tokio::spawn(async move {
        let mut next_run_at = next_run_at;
        let _ = events_tx.send(HarnessScheduleEvent::Scheduled {
            session_id: schedule_id,
            id: schedule_id,
            next_run_at,
        });

        loop {
            let delay = (next_run_at - Utc::now())
                .to_std()
                .unwrap_or(std::time::Duration::ZERO);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = join_cancel.cancelled() => break,
            }
            if join_cancel.is_cancelled() {
                break;
            }

            let execution_id = request.execution_run_id.unwrap_or_else(Uuid::new_v4);
            let _ = events_tx.send(HarnessScheduleEvent::Started {
                session_id: schedule_id,
                id: schedule_id,
                execution_id,
                scheduled_for: next_run_at,
            });

            let mut run = RoutineRun::cron(CronInput {
                task: None,
                project: request.project.clone(),
                schedule: schedule.clone(),
                start_at: Some(next_run_at),
                timeout: request.timeout,
            })
            .execution_run(execution_id);
            if let Some(location) = request.project_location.clone() {
                run = run.project_location(location);
            }

            match harness
                .provider()
                .routine(&request.routine)
                .map_err(anyhow::Error::from)
            {
                Ok(runner) => match runner.run_stream(run).await {
                    Ok(mut handle) => {
                        loop {
                            tokio::select! {
                                event = handle.recv() => {
                                    match event {
                                        Some(event) => {
                                            let _ = events_tx.send(HarnessScheduleEvent::Cron {
                                                session_id: schedule_id,
                                                execution_id,
                                                event,
                                            });
                                        }
                                        None => break,
                                    }
                                }
                                _ = join_cancel.cancelled() => {
                                    handle.cancel();
                                    break;
                                }
                            }
                        }
                        if join_cancel.is_cancelled() {
                            break;
                        }
                        match handle.output().await {
                            Ok(result) => {
                                next_run_at = schedule.next_fire_at();
                                let _ = events_tx.send(HarnessScheduleEvent::Completed {
                                    session_id: schedule_id,
                                    id: schedule_id,
                                    execution_id,
                                    success: result.passed,
                                    error: (!result.passed).then_some(result.output.clone()),
                                    input_tokens: result.input_tokens,
                                    output_tokens: result.output_tokens,
                                    completed_at: Utc::now(),
                                    next_run_at,
                                });
                            }
                            Err(error) => {
                                next_run_at = schedule.next_fire_at();
                                let _ = events_tx.send(HarnessScheduleEvent::Failed {
                                    session_id: schedule_id,
                                    id: schedule_id,
                                    execution_id: Some(execution_id),
                                    error: error.to_string(),
                                    next_run_at,
                                });
                            }
                        }
                    }
                    Err(error) => {
                        next_run_at = schedule.next_fire_at();
                        let _ = events_tx.send(HarnessScheduleEvent::Failed {
                            session_id: schedule_id,
                            id: schedule_id,
                            execution_id: Some(execution_id),
                            error: error.to_string(),
                            next_run_at,
                        });
                    }
                },
                Err(error) => {
                    next_run_at = schedule.next_fire_at();
                    let _ = events_tx.send(HarnessScheduleEvent::Failed {
                        session_id: schedule_id,
                        id: schedule_id,
                        execution_id: Some(execution_id),
                        error: error.to_string(),
                        next_run_at,
                    });
                }
            }

            let _ = events_tx.send(HarnessScheduleEvent::Scheduled {
                session_id: schedule_id,
                id: schedule_id,
                next_run_at,
            });
        }

        if harness
            .executions()
            .get(&schedule_id)
            .is_some_and(|entry| entry.registry_token == registry_token)
        {
            harness.executions().remove(&schedule_id);
        }
        let _ = events_tx.send(HarnessScheduleEvent::Stopped {
            session_id: schedule_id,
            id: schedule_id,
        });

        if join_cancel.is_cancelled() {
            return Ok(());
        }
        Err(crate::HarnessError::Other(anyhow!("cron schedule stopped")))
    });

    Ok(HarnessScheduleHandle::new(events_rx, join, cancel))
}
