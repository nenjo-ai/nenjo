use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use nenjo::memory::MemoryScope;
use nenjo::{AgentRun, AgentRunKind, HeartbeatInput, Slug};
use nenjo_events::{ExecutionType, Response};
use nenjo_sessions::{
    ExecutionPhase, HeartbeatScheduleState, RunCompletion, ScheduleState, SchedulerRuntimeSnapshot,
    SchedulerSessionUpsert, SessionCheckpointUpdate, SessionKind, SessionStatus,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;

use nenjo_harness::registry::{ActiveExecution, ExecutionKind};
use nenjo_harness::{Harness, ProviderRuntime};

use crate::handlers::ResponseSender;
use crate::resource_resolver::PlatformResourceResolver;

#[derive(Clone)]
pub struct HeartbeatCommandContext<S> {
    pub response_sink: S,
    pub worker_id: String,
}

pub struct HeartbeatRestoreRequest {
    pub agent_id: Uuid,
    pub interval: Duration,
    pub timezone: Option<String>,
    pub start_at: Option<chrono::DateTime<chrono::Utc>>,
    pub instructions: Option<String>,
    pub previous_output_ref: Option<String>,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub start_paused: bool,
}

async fn session_memory_scope<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    session_id: Uuid,
) -> Option<nenjo::memory::MemoryScope>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let namespace = harness
        .sessions()
        .memory_namespace(session_id)
        .await
        .ok()
        .flatten()?;
    nenjo::memory::MemoryScope::from_namespace(&namespace)
}

async fn apply_session_memory_scope<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    builder: nenjo::AgentBuilder<P>,
    session_id: Uuid,
) -> nenjo::AgentBuilder<P>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    match session_memory_scope(harness, session_id).await {
        Some(scope) => builder.with_memory_scope(scope),
        None => builder,
    }
}

#[derive(Debug, Clone, Default)]
struct HeartbeatRunState {
    previous_output: Option<String>,
    previous_output_ref: Option<String>,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
}

struct SpawnAgentHeartbeatRequest {
    agent_id: Uuid,
    interval: Duration,
    timezone: Option<String>,
    instructions: Option<String>,
    start_at: Option<chrono::DateTime<chrono::Utc>>,
    restored_state: HeartbeatRunState,
    start_paused: bool,
}

#[derive(Debug, Clone, Default)]
struct HeartbeatTaskState {
    previous_output: Option<String>,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_run_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn heartbeat_output_ref(agent_id: Uuid, completed_at: chrono::DateTime<chrono::Utc>) -> String {
    format!(
        "heartbeat_outputs/{agent_id}/{}.txt",
        completed_at.timestamp_millis()
    )
}

fn heartbeat_memory_namespace(agent_name: &str) -> String {
    MemoryScope::new(agent_name, None).core
}

async fn load_heartbeat_task_state<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    agent_id: Uuid,
    fallback: HeartbeatTaskState,
) -> HeartbeatTaskState
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let Some(record) = harness.sessions().get(agent_id).await.ok().flatten() else {
        return fallback;
    };
    let scheduler = match record.scheduler {
        Some(ScheduleState::Heartbeat(state)) => state,
        _ => return fallback,
    };
    let previous_output = record
        .summary
        .last_progress_message
        .or(fallback.previous_output);
    HeartbeatTaskState {
        previous_output,
        last_run_at: scheduler.last_run_at.or(fallback.last_run_at),
        next_run_at: scheduler.next_run_at.or(fallback.next_run_at),
    }
}

struct HeartbeatSessionUpsert<'a> {
    agent_id: Uuid,
    memory_namespace: Option<&'a str>,
    interval: Duration,
    timezone: Option<&'a str>,
    instructions: Option<&'a str>,
    status: SessionStatus,
    next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    previous_output_ref: Option<String>,
    previous_output: Option<String>,
    run_in_progress: bool,
    last_completion: Option<RunCompletion>,
}

async fn upsert_heartbeat_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    worker_id: &str,
    params: HeartbeatSessionUpsert<'_>,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let HeartbeatSessionUpsert {
        agent_id,
        memory_namespace,
        interval,
        timezone,
        instructions,
        status,
        next_run_at,
        last_run_at,
        previous_output_ref,
        previous_output,
        run_in_progress,
        last_completion,
    } = params;
    let agent = harness
        .provider()
        .manifest_snapshot()
        .agents
        .iter()
        .find(|agent| {
            crate::resource_resolver::stable_resource_id("agent", &agent.slug) == agent_id
        })
        .map(|agent| agent.slug.to_string());
    if let Err(error) = harness
        .sessions()
        .upsert_scheduler(SchedulerSessionUpsert {
            session_id: agent_id,
            kind: SessionKind::HeartbeatSchedule,
            status,
            project: None,
            agent,
            routine: None,
            worker_id: worker_id.to_string(),
            memory_namespace: memory_namespace.map(ToString::to_string),
            scheduler: ScheduleState::Heartbeat(HeartbeatScheduleState {
                interval_secs: interval.as_secs(),
                timezone: timezone.map(ToString::to_string),
                instructions: instructions.map(ToString::to_string),
                next_run_at,
                last_run_at,
                previous_output_ref,
                last_completion,
                run_in_progress,
                paused: status == SessionStatus::Paused,
            }),
            progress_message: previous_output,
        })
        .await
    {
        error!(%agent_id, error = %error, "Failed to upsert heartbeat scheduler session");
    }
}

fn emit_heartbeat_state<S>(
    response_sink: &S,
    agent: &str,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_run_at: chrono::DateTime<chrono::Utc>,
) where
    S: ResponseSender,
{
    let _ = response_sink.send(Response::AgentHeartbeatHeartbeat {
        agent: agent.to_string(),
        last_run_at: last_run_at.map(|ts| ts.to_rfc3339()),
        next_run_at: Some(next_run_at.to_rfc3339()),
    });
}

async fn spawn_agent_heartbeat<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    ctx: &HeartbeatCommandContext<S>,
    request: SpawnAgentHeartbeatRequest,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
{
    let SpawnAgentHeartbeatRequest {
        agent_id,
        interval,
        timezone,
        instructions,
        start_at,
        restored_state,
        start_paused,
    } = request;

    if interval.is_zero() {
        anyhow::bail!("Heartbeat interval must be greater than zero");
    }

    let cancel = CancellationToken::new();
    let pause = nenjo::agents::runner::types::PauseToken::new();
    if start_paused {
        pause.pause();
    }
    let executions = harness.executions();
    if let Some((_, prev)) = executions.remove(&agent_id) {
        prev.cancel.cancel();
    }
    let registry_token = Uuid::new_v4();
    executions.insert(
        agent_id,
        ActiveExecution {
            kind: ExecutionKind::Heartbeat,
            registry_token,
            execution_run_id: None,
            cancel: cancel.clone(),
            pause: Some(pause.clone()),
        },
    );

    let response_tx = ctx.response_sink.clone();
    let instructions_for_session = instructions.clone();
    let active_run = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<()>>));
    let active_run_for_schedule = active_run.clone();
    let restored_last_run_at = restored_state.last_run_at;
    let restored_previous_output_ref = restored_state.previous_output_ref.clone();
    let run_state = Arc::new(Mutex::new(restored_state));
    let provider_cell = harness.provider_handle();
    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
    let heartbeat_agent_slug = PlatformResourceResolver::new(&manifest).agent(agent_id)?;
    let heartbeat_agent_name = manifest
        .agents
        .iter()
        .find(|agent| {
            crate::resource_resolver::stable_resource_id("agent", &agent.slug) == agent_id
        })
        .map(|agent| agent.name.clone())
        .unwrap_or_else(|| agent_id.to_string());
    let heartbeat_memory_namespace = heartbeat_memory_namespace(&heartbeat_agent_name);
    let worker_id = ctx.worker_id.clone();
    let pause_token = pause.clone();
    let initial_next_run_at = start_at.unwrap_or_else(|| {
        chrono::Utc::now()
            + chrono::Duration::from_std(interval).unwrap_or_else(|_| chrono::Duration::seconds(60))
    });
    upsert_heartbeat_session(
        harness,
        &worker_id,
        HeartbeatSessionUpsert {
            agent_id,
            memory_namespace: Some(&heartbeat_memory_namespace),
            interval,
            timezone: timezone.as_deref(),
            instructions: instructions_for_session.as_deref(),
            status: if start_paused {
                SessionStatus::Paused
            } else {
                SessionStatus::Active
            },
            next_run_at: Some(initial_next_run_at),
            last_run_at: restored_last_run_at,
            previous_output_ref: restored_previous_output_ref,
            previous_output: None,
            run_in_progress: false,
            last_completion: None,
        },
    )
    .await;

    let harness_for_schedule = harness.clone();
    let timezone_for_log = timezone.clone();
    let heartbeat_agent_slug_for_log = heartbeat_agent_slug.to_string();
    tokio::spawn(async move {
        let mut next_run_at = initial_next_run_at;
        let _ = response_tx.send(Response::AgentHeartbeatScheduled {
            agent: heartbeat_agent_slug.to_string(),
            next_run_at: Some(next_run_at.to_rfc3339()),
        });
        emit_heartbeat_state(
            &response_tx,
            heartbeat_agent_slug.as_str(),
            None,
            next_run_at,
        );

        loop {
            pause_token.wait_if_paused().await;
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
                    emit_heartbeat_state(
                        &response_tx,
                        heartbeat_agent_slug.as_str(),
                        None,
                        scheduled_next_run_at,
                    );
                    upsert_heartbeat_session(
                        &harness_for_schedule,
                        &worker_id,
                        HeartbeatSessionUpsert {
                            agent_id,
                            memory_namespace: Some(&heartbeat_memory_namespace),
                            interval,
                            timezone: timezone.as_deref(),
                            instructions: instructions_for_session.as_deref(),
                            status: SessionStatus::Active,
                            next_run_at: Some(scheduled_next_run_at),
                            last_run_at: None,
                            previous_output_ref: None,
                            previous_output: None,
                            run_in_progress: true,
                            last_completion: None,
                        },
                    )
                    .await;
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
            let worker_id_for_run = worker_id.clone();
            let heartbeat_memory_namespace_for_run = heartbeat_memory_namespace.clone();
            let harness_for_run = harness_for_schedule.clone();
            let timezone_for_run = timezone.clone();
            let run_next_run_at = scheduled_next_run_at;
            let state_snapshot = {
                let state = run_state.lock().await;
                state.clone()
            };
            upsert_heartbeat_session(
                &harness_for_schedule,
                &worker_id,
                HeartbeatSessionUpsert {
                    agent_id,
                    memory_namespace: Some(&heartbeat_memory_namespace),
                    interval,
                    timezone: timezone.as_deref(),
                    instructions: instructions_for_session.as_deref(),
                    status: SessionStatus::Active,
                    next_run_at: Some(run_next_run_at),
                    last_run_at: state_snapshot.last_run_at,
                    previous_output_ref: state_snapshot.previous_output_ref.clone(),
                    previous_output: state_snapshot.previous_output.clone(),
                    run_in_progress: true,
                    last_completion: None,
                },
            )
            .await;
            let mut active_run_guard = active_run_for_schedule.lock().await;
            let heartbeat_agent_slug_for_run = heartbeat_agent_slug.clone();
            let instructions_for_run = instructions.clone();
            *active_run_guard = Some(tokio::spawn(async move {
                let execution_id = Uuid::new_v4();
                let _ = response_tx.send(Response::ExecutionStarted {
                    id: execution_id,
                    project: None,
                    routine: None,
                    routine_name: None,
                    agent: Some(heartbeat_agent_slug_for_run.to_string()),
                    config: serde_json::json!({
                        "trigger": "agent_heartbeat",
                        "interval_secs": interval.as_secs(),
                        "agent": heartbeat_agent_slug_for_run.clone(),
                        "timezone": timezone_for_run.as_deref(),
                    }),
                });
                let _ = harness_for_run
                    .sessions()
                    .update_checkpoint(SessionCheckpointUpdate {
                        session_id: agent_id,
                        phase: ExecutionPhase::ExecutingTools,
                        worktree: None,
                        active_tool_name: None,
                        scheduler_runtime: Some(SchedulerRuntimeSnapshot {
                            active_execution_id: Some(execution_id),
                            cycle: None,
                        }),
                    })
                    .await;

                let result = async {
                    let task_state = load_heartbeat_task_state(
                        &harness_for_run,
                        agent_id,
                        HeartbeatTaskState {
                            previous_output: state_snapshot.previous_output.clone(),
                            last_run_at: state_snapshot.last_run_at,
                            next_run_at: Some(run_next_run_at),
                        },
                    )
                    .await;
                    let provider = provider_cell.load_full();
                    let manifest = provider.manifest_snapshot();
                    let resolver = PlatformResourceResolver::new(&manifest);
                    let agent = resolver.agent(agent_id)?;
                    let builder = provider.agent(&agent).await?;
                    let builder =
                        apply_session_memory_scope(&harness_for_run, builder, agent_id).await;
                    let runner = builder.build().await?;
                    runner
                        .run(AgentRun {
                            kind: AgentRunKind::Heartbeat(HeartbeatInput {
                                agent: agent.clone(),
                                interval,
                                start_at: None,
                                instructions: instructions_for_run.clone(),
                                previous_output: task_state.previous_output,
                                last_run_at: task_state.last_run_at,
                                next_run_at: task_state.next_run_at,
                            }),
                            execution: Default::default(),
                        })
                        .await
                }
                .await;

                let completed_at = chrono::Utc::now();
                match result {
                    Ok(output) => {
                        let output_ref = heartbeat_output_ref(agent_id, completed_at);
                        {
                            let mut state = run_state.lock().await;
                            state.previous_output = Some(output.text.clone());
                            state.previous_output_ref = Some(output_ref.clone());
                            state.last_run_at = Some(completed_at);
                        }
                        let _ = response_tx.send(Response::ExecutionCompleted {
                            id: execution_id,
                            success: true,
                            error: None,
                            total_input_tokens: output.input_tokens,
                            total_output_tokens: output.output_tokens,
                            execution_type: Some(ExecutionType::Heartbeat),
                            routine: None,
                            routine_name: None,
                            agent: Some(heartbeat_agent_slug_for_run.to_string()),
                        });
                        upsert_heartbeat_session(
                            &harness_for_run,
                            &worker_id_for_run,
                            HeartbeatSessionUpsert {
                                agent_id,
                                memory_namespace: Some(&heartbeat_memory_namespace_for_run),
                                interval,
                                timezone: timezone_for_run.as_deref(),
                                instructions: instructions_for_run.as_deref(),
                                status: SessionStatus::Active,
                                next_run_at: Some(run_next_run_at),
                                last_run_at: Some(completed_at),
                                previous_output_ref: Some(output_ref),
                                previous_output: Some(output.text.clone()),
                                run_in_progress: false,
                                last_completion: Some(RunCompletion {
                                    success: true,
                                    error_summary: None,
                                    completed_at,
                                }),
                            },
                        )
                        .await;
                    }
                    Err(e) => {
                        let output_ref = heartbeat_output_ref(agent_id, completed_at);
                        let error_text = e.to_string();
                        {
                            let mut state = run_state.lock().await;
                            state.previous_output = Some(error_text.clone());
                            state.previous_output_ref = Some(output_ref.clone());
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
                            routine: None,
                            routine_name: None,
                            agent: Some(heartbeat_agent_slug_for_run.to_string()),
                        });
                        upsert_heartbeat_session(
                            &harness_for_run,
                            &worker_id_for_run,
                            HeartbeatSessionUpsert {
                                agent_id,
                                memory_namespace: Some(&heartbeat_memory_namespace_for_run),
                                interval,
                                timezone: timezone_for_run.as_deref(),
                                instructions: instructions_for_run.as_deref(),
                                status: SessionStatus::Active,
                                next_run_at: Some(run_next_run_at),
                                last_run_at: Some(completed_at),
                                previous_output_ref: Some(output_ref),
                                previous_output: Some(error_text.clone()),
                                run_in_progress: false,
                                last_completion: Some(RunCompletion {
                                    success: false,
                                    error_summary: Some(error_text),
                                    completed_at,
                                }),
                            },
                        )
                        .await;
                    }
                }

                emit_heartbeat_state(
                    &response_tx,
                    heartbeat_agent_slug_for_run.as_str(),
                    Some(completed_at),
                    run_next_run_at,
                );
                let _ = harness_for_run
                    .sessions()
                    .update_checkpoint(SessionCheckpointUpdate {
                        session_id: agent_id,
                        phase: ExecutionPhase::Waiting,
                        worktree: None,
                        active_tool_name: None,
                        scheduler_runtime: Some(SchedulerRuntimeSnapshot {
                            active_execution_id: None,
                            cycle: None,
                        }),
                    })
                    .await;

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

    info!(
        agent = %heartbeat_agent_slug_for_log,
        interval_secs = interval.as_secs(),
        timezone = timezone_for_log.as_deref(),
        "Enabled agent heartbeat"
    );
    Ok(())
}

#[async_trait::async_trait]
/// Worker integration methods for agent heartbeat platform commands.
///
/// Heartbeats are scheduled by the worker because they depend on timers,
/// recovery, response delivery, and process lifecycle. Each heartbeat run uses
/// the harness/provider to invoke the configured agent.
pub(crate) trait WorkerHeartbeatHarnessExt<S>
where
    S: ResponseSender,
{
    /// Enable or replace an interval-based heartbeat for an agent.
    async fn handle_agent_heartbeat_enable(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        agent: &str,
        interval_str: &str,
        timezone: Option<&str>,
        instructions: Option<String>,
        start_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()>
    where
        S: Clone + 'static;

    /// Restore a heartbeat from persisted scheduler session state.
    async fn restore_agent_heartbeat(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        request: HeartbeatRestoreRequest,
    ) -> Result<()>
    where
        S: Clone + 'static;

    /// Disable an active heartbeat schedule.
    async fn handle_agent_heartbeat_disable(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        agent: &str,
    ) -> Result<()>;

    /// Trigger an agent heartbeat immediately outside its regular interval.
    async fn handle_agent_heartbeat_trigger(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        agent: &str,
        instructions: Option<String>,
    ) -> Result<()>;
}

#[async_trait::async_trait]
impl<P, SessionRt, S> WorkerHeartbeatHarnessExt<S> for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
{
    async fn handle_agent_heartbeat_enable(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        agent: &str,
        interval_str: &str,
        timezone: Option<&str>,
        instructions: Option<String>,
        start_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()>
    where
        S: Clone + 'static,
    {
        let manifest = self.provider().manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let agent_id = resolver.agent_id(&Slug::parse(agent)?)?;
        let interval = nenjo::routines::types::parse_duration(interval_str)?;
        spawn_agent_heartbeat(
            self,
            ctx,
            SpawnAgentHeartbeatRequest {
                agent_id,
                interval,
                timezone: timezone.map(ToOwned::to_owned),
                instructions,
                start_at,
                restored_state: HeartbeatRunState::default(),
                start_paused: false,
            },
        )
        .await
    }

    async fn restore_agent_heartbeat(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        request: HeartbeatRestoreRequest,
    ) -> Result<()>
    where
        S: Clone + 'static,
    {
        let HeartbeatRestoreRequest {
            agent_id,
            interval,
            timezone,
            start_at,
            instructions,
            previous_output_ref,
            last_run_at,
            start_paused,
        } = request;

        let previous_output = self
            .sessions()
            .get(agent_id)
            .await
            .ok()
            .flatten()
            .and_then(|record| record.summary.last_progress_message);
        spawn_agent_heartbeat(
            self,
            ctx,
            SpawnAgentHeartbeatRequest {
                agent_id,
                interval,
                timezone,
                instructions,
                start_at,
                restored_state: HeartbeatRunState {
                    previous_output,
                    previous_output_ref,
                    last_run_at,
                },
                start_paused,
            },
        )
        .await
    }

    async fn handle_agent_heartbeat_disable(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        agent: &str,
    ) -> Result<()> {
        let manifest = self.provider().manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let agent_id = resolver.agent_id(&Slug::parse(agent)?)?;
        if let Some((_, exec)) = self.executions().remove(&agent_id) {
            exec.cancel.cancel();
            let _ = ctx.response_sink.send(Response::AgentHeartbeatStopped {
                agent: agent.to_string(),
            });
            upsert_heartbeat_session(
                self,
                &ctx.worker_id,
                HeartbeatSessionUpsert {
                    agent_id,
                    memory_namespace: None,
                    interval: Duration::from_secs(0),
                    timezone: None,
                    instructions: None,
                    status: SessionStatus::Cancelled,
                    next_run_at: None,
                    last_run_at: None,
                    previous_output_ref: None,
                    previous_output: None,
                    run_in_progress: false,
                    last_completion: None,
                },
            )
            .await;
        }
        Ok(())
    }

    async fn handle_agent_heartbeat_trigger(
        &self,
        ctx: &HeartbeatCommandContext<S>,
        agent: &str,
        instructions: Option<String>,
    ) -> Result<()> {
        let manifest = self.provider().manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let agent_id = resolver.agent_id(&Slug::parse(agent)?)?;
        let execution_id = Uuid::new_v4();
        let _ = ctx.response_sink.send(Response::ExecutionStarted {
            id: execution_id,
            project: None,
            routine: None,
            routine_name: None,
            agent: Some(agent.to_string()),
            config: serde_json::json!({
                "trigger": "agent_heartbeat",
                "manual": true,
                "agent": agent,
            }),
        });

        let result = async {
            let task_state =
                load_heartbeat_task_state(self, agent_id, HeartbeatTaskState::default()).await;
            let manifest = self.provider().manifest_snapshot();
            let resolver = PlatformResourceResolver::new(&manifest);
            let agent = resolver.agent(agent_id)?;
            let builder = self.provider().agent(&agent).await?;
            let builder = apply_session_memory_scope(self, builder, agent_id).await;
            let runner = builder.build().await?;
            runner
                .run(AgentRun {
                    kind: AgentRunKind::Heartbeat(HeartbeatInput {
                        agent,
                        interval: Duration::from_secs(1),
                        start_at: None,
                        instructions,
                        previous_output: task_state.previous_output,
                        last_run_at: task_state.last_run_at,
                        next_run_at: task_state.next_run_at,
                    }),
                    execution: Default::default(),
                })
                .await
        }
        .await;

        let (success, error, total_input_tokens, total_output_tokens) = match result {
            Ok(output) => (true, None, output.input_tokens, output.output_tokens),
            Err(e) => (false, Some(e.to_string()), 0, 0),
        };

        let _ = ctx.response_sink.send(Response::ExecutionCompleted {
            id: execution_id,
            success,
            error,
            total_input_tokens,
            total_output_tokens,
            execution_type: Some(ExecutionType::Heartbeat),
            routine: None,
            routine_name: None,
            agent: Some(agent.to_string()),
        });

        Ok(())
    }
}
