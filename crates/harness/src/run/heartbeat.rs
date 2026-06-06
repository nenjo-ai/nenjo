//! Platform-free agent heartbeat scheduling.

use anyhow::anyhow;
use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo::{AgentRun, AgentRunKind, HeartbeatInput};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::events::HarnessScheduleEvent;
use crate::handle::HarnessScheduleHandle;
use crate::registry::{ActiveExecution, ExecutionKind};
use crate::request::HeartbeatRequest;
use crate::{Harness, ProviderRuntime};

pub(crate) async fn heartbeat<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: HeartbeatRequest,
) -> crate::Result<HarnessScheduleHandle>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    if request.interval.is_zero() {
        return Err(crate::HarnessError::InvalidCommand(
            "HeartbeatRequest interval must be greater than zero".to_string(),
        ));
    }

    let memory_scope = None::<MemoryScope>;
    let initial_next_run_at = request.start_at.unwrap_or_else(|| {
        Utc::now()
            + chrono::Duration::from_std(request.interval)
                .unwrap_or_else(|_| chrono::Duration::seconds(60))
    });
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
            kind: ExecutionKind::Heartbeat,
            registry_token,
            execution_run_id: request.execution_run_id,
            cancel: cancel.clone(),
            pause: None,
        },
    );

    let harness = harness.clone();
    let join_cancel = cancel.clone();
    let join = tokio::spawn(async move {
        let mut next_run_at = initial_next_run_at;
        let mut previous_output = request.previous_output.clone();
        let mut last_run_at = request.last_run_at;
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
            let scheduled_for = next_run_at;
            next_run_at = scheduled_for
                + chrono::Duration::from_std(request.interval)
                    .unwrap_or_else(|_| chrono::Duration::seconds(60));
            let _ = events_tx.send(HarnessScheduleEvent::Started {
                session_id: schedule_id,
                id: schedule_id,
                execution_id,
                scheduled_for,
            });

            let result = async {
                let mut builder = harness
                    .provider()
                    .agent(&request.agent)
                    .await
                    .map_err(anyhow::Error::from)?;
                if let Some(scope) = memory_scope.clone() {
                    builder = builder.with_memory_scope(scope);
                }
                let runner = builder.build().await.map_err(anyhow::Error::from)?;
                let mut handle = runner
                    .run_stream(
                        AgentRun {
                            kind: AgentRunKind::Heartbeat(HeartbeatInput {
                                agent: request.agent.clone(),
                                interval: request.interval,
                                start_at: Some(scheduled_for),
                                instructions: request.instructions.clone(),
                                previous_output: previous_output.clone(),
                                last_run_at,
                                next_run_at: Some(next_run_at),
                            }),
                            execution: Default::default(),
                        }
                        .execution_run(execution_id),
                    )
                    .await?;

                loop {
                    tokio::select! {
                        event = handle.recv() => {
                            match event {
                                Some(event) => {
                                    let _ = events_tx.send(HarnessScheduleEvent::Heartbeat {
                                        session_id: schedule_id,
                                        execution_id,
                                        event,
                                    });
                                }
                                None => break,
                            }
                        }
                        _ = join_cancel.cancelled() => {
                            handle.abort();
                            break;
                        }
                    }
                }
                if join_cancel.is_cancelled() {
                    return Err(anyhow!("heartbeat schedule stopped"));
                }
                handle.output().await
            }
            .await;

            let completed_at = Utc::now();
            if join_cancel.is_cancelled() {
                break;
            }
            match result {
                Ok(output) => {
                    previous_output = Some(output.text.clone());
                    last_run_at = Some(completed_at);
                    let _ = events_tx.send(HarnessScheduleEvent::Completed {
                        session_id: schedule_id,
                        id: schedule_id,
                        execution_id,
                        success: true,
                        error: None,
                        input_tokens: output.input_tokens,
                        output_tokens: output.output_tokens,
                        completed_at,
                        next_run_at,
                    });
                }
                Err(error) => {
                    previous_output = Some(error.to_string());
                    last_run_at = Some(completed_at);
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
        Ok(())
    });

    Ok(HarnessScheduleHandle::new(events_rx, join, cancel))
}
