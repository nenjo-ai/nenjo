use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use nenjo::types::TaskType;
use nenjo_events::{ExecutionType, Response};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;

use crate::harness::{ActiveExecution, CommandContext, ExecutionKind};

#[derive(Debug, Clone, Default)]
struct HeartbeatRunState {
    previous_output: Option<String>,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn emit_heartbeat_state(
    response_tx: &tokio::sync::mpsc::UnboundedSender<Response>,
    agent_id: Uuid,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_run_at: chrono::DateTime<chrono::Utc>,
) {
    let _ = response_tx.send(Response::AgentHeartbeatHeartbeat {
        agent_id,
        last_run_at: last_run_at.map(|ts| ts.to_rfc3339()),
        next_run_at: Some(next_run_at.to_rfc3339()),
    });
}

pub async fn handle_agent_heartbeat_enable(
    ctx: &CommandContext,
    agent_id: Uuid,
    interval_str: &str,
    start_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    let interval = nenjo::routines::types::parse_duration(interval_str)?;
    if interval.is_zero() {
        anyhow::bail!("Heartbeat interval must be greater than zero");
    }

    let cancel = CancellationToken::new();
    if let Some((_, prev)) = ctx.executions.remove(&agent_id) {
        prev.cancel.cancel();
    }
    let registry_token = Uuid::new_v4();
    ctx.executions.insert(
        agent_id,
        ActiveExecution {
            kind: ExecutionKind::Heartbeat,
            registry_token,
            execution_run_id: None,
            cancel: cancel.clone(),
            pause: None,
        },
    );

    let response_tx = ctx.response_tx.clone();
    let executions = ctx.executions.clone();
    let active_run = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<()>>));
    let active_run_for_schedule = active_run.clone();
    let run_state = Arc::new(Mutex::new(HeartbeatRunState::default()));
    let provider_cell = ctx.provider.clone();

    tokio::spawn(async move {
        let mut next_run_at = start_at.unwrap_or_else(|| {
            chrono::Utc::now()
                + chrono::Duration::from_std(interval)
                    .unwrap_or_else(|_| chrono::Duration::seconds(60))
        });
        let _ = response_tx.send(Response::AgentHeartbeatScheduled {
            agent_id,
            next_run_at: Some(next_run_at.to_rfc3339()),
        });
        emit_heartbeat_state(&response_tx, agent_id, None, next_run_at);

        loop {
            let delay = (next_run_at - chrono::Utc::now())
                .to_std()
                .unwrap_or(Duration::ZERO);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => break,
            }

            let scheduled_next_run_at = next_run_at
                + chrono::Duration::from_std(interval)
                    .unwrap_or_else(|_| chrono::Duration::seconds(60));

            let mut active_run_guard = active_run_for_schedule.lock().await;
            let finished_handle = match active_run_guard.as_ref() {
                Some(handle) if handle.is_finished() => active_run_guard.take(),
                Some(_) => {
                    emit_heartbeat_state(&response_tx, agent_id, None, scheduled_next_run_at);
                    next_run_at = scheduled_next_run_at;
                    continue;
                }
                None => None,
            };
            drop(active_run_guard);

            if let Some(handle) = finished_handle {
                let _ = handle.await;
            }

            let provider_cell = provider_cell.clone();
            let response_tx = response_tx.clone();
            let active_run = active_run_for_schedule.clone();
            let run_state = run_state.clone();
            let run_next_run_at = scheduled_next_run_at;
            let state_snapshot = {
                let state = run_state.lock().await;
                state.clone()
            };
            let mut active_run_guard = active_run_for_schedule.lock().await;
            *active_run_guard = Some(tokio::spawn(async move {
                let execution_id = Uuid::new_v4();
                let _ = response_tx.send(Response::ExecutionStarted {
                    id: execution_id,
                    project_id: None,
                    routine_id: None,
                    routine_name: None,
                    agent_id: Some(agent_id),
                    config: serde_json::json!({
                        "trigger": "agent_heartbeat",
                        "interval_secs": interval.as_secs(),
                        "agent_id": agent_id.to_string(),
                    }),
                });

                let result = async {
                    let provider = provider_cell.load_full();
                    let runner = provider.agent_by_id(agent_id).await?.build().await?;
                    runner
                        .task(TaskType::Heartbeat {
                            agent_id,
                            project_id: None,
                            interval,
                            start_at: None,
                            previous_output: state_snapshot.previous_output.clone(),
                            last_run_at: state_snapshot.last_run_at,
                            next_run_at: Some(run_next_run_at),
                        })
                        .await
                }
                .await;

                let completed_at = chrono::Utc::now();
                match result {
                    Ok(output) => {
                        {
                            let mut state = run_state.lock().await;
                            state.previous_output = Some(output.text.clone());
                            state.last_run_at = Some(completed_at);
                        }
                        let _ = response_tx.send(Response::ExecutionCompleted {
                            id: execution_id,
                            success: true,
                            error: None,
                            total_input_tokens: output.input_tokens,
                            total_output_tokens: output.output_tokens,
                            execution_type: Some(ExecutionType::Heartbeat),
                            routine_id: None,
                            routine_name: None,
                            agent_id: Some(agent_id),
                        });
                    }
                    Err(e) => {
                        {
                            let mut state = run_state.lock().await;
                            state.previous_output = Some(e.to_string());
                            state.last_run_at = Some(completed_at);
                        }
                        error!(%agent_id, error = %e, "Agent heartbeat execution failed");
                        let _ = response_tx.send(Response::ExecutionCompleted {
                            id: execution_id,
                            success: false,
                            error: Some(e.to_string()),
                            total_input_tokens: 0,
                            total_output_tokens: 0,
                            execution_type: Some(ExecutionType::Heartbeat),
                            routine_id: None,
                            routine_name: None,
                            agent_id: Some(agent_id),
                        });
                    }
                }

                emit_heartbeat_state(&response_tx, agent_id, Some(completed_at), run_next_run_at);

                let mut active_run_guard = active_run.lock().await;
                active_run_guard.take();
            }));
            drop(active_run_guard);

            next_run_at = scheduled_next_run_at;
        }

        if let Some(handle) = active_run.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }

        if executions
            .get(&agent_id)
            .is_some_and(|entry| entry.registry_token == registry_token)
        {
            executions.remove(&agent_id);
        }
    });

    info!(%agent_id, interval_secs = interval.as_secs(), "Enabled agent heartbeat");
    Ok(())
}

pub async fn handle_agent_heartbeat_disable(ctx: &CommandContext, agent_id: Uuid) -> Result<()> {
    if let Some((_, exec)) = ctx.executions.remove(&agent_id) {
        exec.cancel.cancel();
        let _ = ctx
            .response_tx
            .send(Response::AgentHeartbeatStopped { agent_id });
    }
    Ok(())
}

pub async fn handle_agent_heartbeat_trigger(ctx: &CommandContext, agent_id: Uuid) -> Result<()> {
    let execution_id = Uuid::new_v4();
    let _ = ctx.response_tx.send(Response::ExecutionStarted {
        id: execution_id,
        project_id: None,
        routine_id: None,
        routine_name: None,
        agent_id: Some(agent_id),
        config: serde_json::json!({
            "trigger": "agent_heartbeat",
            "manual": true,
            "agent_id": agent_id.to_string(),
        }),
    });

    let result = async {
        let runner = ctx.provider().agent_by_id(agent_id).await?.build().await?;
        runner
            .task(TaskType::Heartbeat {
                agent_id,
                project_id: None,
                interval: Duration::from_secs(1),
                start_at: None,
                previous_output: None,
                last_run_at: None,
                next_run_at: None,
            })
            .await
    }
    .await;

    let (success, error, total_input_tokens, total_output_tokens) = match result {
        Ok(output) => (true, None, output.input_tokens, output.output_tokens),
        Err(e) => (false, Some(e.to_string()), 0, 0),
    };

    let _ = ctx.response_tx.send(Response::ExecutionCompleted {
        id: execution_id,
        success,
        error,
        total_input_tokens,
        total_output_tokens,
        execution_type: Some(ExecutionType::Heartbeat),
        routine_id: None,
        routine_name: None,
        agent_id: Some(agent_id),
    });

    Ok(())
}
