//! Cron schedule handlers.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::Result;
use nenjo::memory::MemoryScope;
use nenjo::{CronInput, RoutineRun};
use nenjo_events::{CronScheduleStatus, Response};
use nenjo_sessions::{
    CronScheduleState, ExecutionPhase, RunCompletion, ScheduleState, SchedulerRuntimeSnapshot,
    SchedulerSessionUpsert, SessionCheckpointUpdate, SessionKind, SessionStatus,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;

use super::ResponseSender;
use crate::event_bridge::{project_slug, routine_event_to_response};
use crate::execution_trace::ExecutionTraceRuntime;
use crate::{ActiveExecution, ExecutionKind, Harness, HarnessProvider};

#[derive(Clone)]
pub struct CronCommandContext<S> {
    pub response_sink: S,
    pub worker_id: String,
}

struct CronSessionUpsert<'a> {
    routine_id: Uuid,
    project_id: Option<Uuid>,
    memory_namespace: Option<&'a str>,
    schedule: &'a str,
    status: SessionStatus,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    last_completion: Option<RunCompletion>,
}

async fn upsert_cron_session<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    worker_id: &str,
    params: CronSessionUpsert<'_>,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let CronSessionUpsert {
        routine_id,
        project_id,
        memory_namespace,
        schedule,
        status,
        last_run_at,
        next_run_at,
        last_completion,
    } = params;
    if let Err(error) = harness
        .upsert_scheduler_session(SchedulerSessionUpsert {
            session_id: routine_id,
            kind: SessionKind::CronSchedule,
            status,
            project_id,
            agent_id: None,
            routine_id: Some(routine_id),
            worker_id: worker_id.to_string(),
            memory_namespace: memory_namespace.map(ToString::to_string),
            scheduler: ScheduleState::Cron(CronScheduleState {
                schedule_expr: schedule.to_string(),
                next_run_at,
                last_run_at,
                last_completion,
                paused: status == SessionStatus::Paused,
            }),
            progress_message: None,
        })
        .await
    {
        error!(%routine_id, error = %error, "Failed to upsert cron scheduler session");
    }
}

fn resolve_cron_memory_namespace<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    routine_id: Uuid,
    project_id: Option<Uuid>,
) -> Option<String>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let manifest = harness.provider().manifest().clone();
    let routine = manifest
        .routines
        .iter()
        .find(|routine| routine.id == routine_id)?;
    let mut agent_names: BTreeSet<String> = BTreeSet::new();

    for step in &routine.steps {
        if let Some(agent_id) = step.agent_id
            && let Some(agent) = manifest.agents.iter().find(|agent| agent.id == agent_id)
        {
            agent_names.insert(agent.name.clone());
        }
    }

    if agent_names.len() != 1 {
        return None;
    }

    let agent_name = agent_names.into_iter().next()?;
    let slug = project_id
        .filter(|project_id| !project_id.is_nil())
        .map(|project_id| project_slug(&manifest, project_id));

    Some(MemoryScope::new(&agent_name, slug.as_deref().filter(|slug| !slug.is_empty())).project)
}

fn emit_cron_heartbeat<S>(
    response_sink: &S,
    routine_id: Uuid,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_fire_at: chrono::DateTime<chrono::Utc>,
) where
    S: ResponseSender,
{
    let _ = response_sink.send(Response::CronHeartbeat {
        active_schedules: vec![CronScheduleStatus {
            routine_id: routine_id.to_string(),
            last_run_at: last_run_at.map(|ts| ts.to_rfc3339()),
            next_fire_at: Some(next_fire_at.to_rfc3339()),
        }],
    });
}

impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    /// Enable a cron schedule. Keyed by routine id for cancellation.
    pub async fn handle_cron_enable<S>(
        &self,
        ctx: &CronCommandContext<S>,
        routine_id: Uuid,
        project_id: Option<Uuid>,
        schedule: &str,
        start_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()>
    where
        S: ResponseSender + Clone + 'static,
    {
        info!(%routine_id, %schedule, "Enabling cron schedule");

        let cron_schedule = nenjo::routines::types::parse_schedule(schedule).map_err(|error| {
            error!(%routine_id, error = %error, "Invalid cron schedule");
            error
        })?;

        let cancel = CancellationToken::new();
        let executions = self.executions();
        if let Some((_, prev)) = executions.remove(&routine_id) {
            prev.cancel.cancel();
        }
        let registry_token = Uuid::new_v4();
        executions.insert(
            routine_id,
            ActiveExecution {
                kind: ExecutionKind::Cron,
                registry_token,
                execution_run_id: None,
                cancel: cancel.clone(),
                pause: None,
            },
        );

        let task = RoutineRun::cron(CronInput {
            task: None,
            project_id,
            schedule: cron_schedule.clone(),
            start_at: None,
            timeout: Duration::from_secs(24 * 3600),
        });

        let response_sink = ctx.response_sink.clone();
        let schedule_owned = schedule.to_string();
        let provider_cell = self.provider_handle();
        let worker_id = ctx.worker_id.clone();
        let cron_memory_namespace = resolve_cron_memory_namespace(self, routine_id, project_id);

        let routine_name = self
            .provider()
            .manifest()
            .routines
            .iter()
            .find(|routine| routine.id == routine_id)
            .map(|routine| routine.name.clone())
            .unwrap_or_else(|| routine_id.to_string());

        let opt_project_id = project_id;
        let initial_next_run_at = start_at.unwrap_or_else(|| cron_schedule.next_fire_at());
        upsert_cron_session(
            self,
            &worker_id,
            CronSessionUpsert {
                routine_id,
                project_id,
                memory_namespace: cron_memory_namespace.as_deref(),
                schedule,
                status: SessionStatus::Active,
                last_run_at: None,
                next_run_at: Some(initial_next_run_at),
                last_completion: None,
            },
        )
        .await;

        let harness = self.clone();
        tokio::spawn(async move {
            let mut last_run_at: Option<chrono::DateTime<chrono::Utc>> = None;
            let mut next_run_at = initial_next_run_at;

            emit_cron_heartbeat(&response_sink, routine_id, None, next_run_at);
            let _ = response_sink.send(Response::CronScheduled {
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
                            let mut current_cycle: Option<u32> = None;
                            let mut current_agent_id: Option<Uuid> = None;
                            let mut cycle_completed = false;
                            let mut schedule_cancelled = false;

                            loop {
                                tokio::select! {
                                    event = handle.recv() => {
                                        match event {
                                            Some(event) => {
                                                if let nenjo::RoutineEvent::StepStarted { agent_id, .. } = &event {
                                                    current_agent_id = *agent_id;
                                                }

                                                match &event {
                                                    nenjo::RoutineEvent::CronCycleStarted { cycle } => {
                                                        let cycle_id = Uuid::new_v4();
                                                        current_cycle_id = Some(cycle_id);
                                                        current_cycle = Some(*cycle);
                                                        let _ = harness.update_session_checkpoint(SessionCheckpointUpdate {
                                                            session_id: routine_id,
                                                            phase: ExecutionPhase::ExecutingTools,
                                                            worktree: None,
                                                            active_tool_name: None,
                                                            scheduler_runtime: Some(SchedulerRuntimeSnapshot {
                                                                active_execution_id: Some(cycle_id),
                                                                cycle: Some(*cycle),
                                                            }),
                                                        }).await;

                                                        let _ = response_sink.send(Response::ExecutionStarted {
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
                                                            let cycle = current_cycle.take();
                                                            let completed_at = chrono::Utc::now();
                                                            last_run_at = Some(completed_at);
                                                            let _ = response_sink.send(Response::ExecutionCompleted {
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
                                                            upsert_cron_session(
                                                                &harness,
                                                                &worker_id,
                                                                CronSessionUpsert {
                                                                    routine_id,
                                                                    project_id: opt_project_id,
                                                                    memory_namespace: cron_memory_namespace.as_deref(),
                                                                    schedule: &schedule_owned,
                                                                    status: SessionStatus::Active,
                                                                    last_run_at,
                                                                    next_run_at: Some(next_run_at),
                                                                    last_completion: Some(RunCompletion {
                                                                        success: result.passed,
                                                                        error_summary: if result.passed {
                                                                            None
                                                                        } else {
                                                                            Some(result.output.clone())
                                                                        },
                                                                        completed_at,
                                                                    }),
                                                                },
                                                            )
                                                            .await;
                                                            let _ = harness.update_session_checkpoint(SessionCheckpointUpdate {
                                                                session_id: routine_id,
                                                                phase: ExecutionPhase::Waiting,
                                                                worktree: None,
                                                                active_tool_name: None,
                                                                scheduler_runtime: Some(SchedulerRuntimeSnapshot {
                                                                    active_execution_id: None,
                                                                    cycle,
                                                                }),
                                                            }).await;
                                                        }
                                                    }
                                                    _ => {}
                                                }

                                                let execution_id = current_cycle_id.unwrap_or(routine_id);
                                                if let Some(response) = routine_event_to_response(
                                                    &event,
                                                    execution_id,
                                                    None,
                                                    current_agent_id,
                                                    provider.manifest(),
                                                ) {
                                                    let _ = response_sink.send(response);
                                                }
                                            }
                                            None => break,
                                        }
                                    }
                                    _ = cancel.cancelled() => {
                                        handle.cancel();
                                        schedule_cancelled = true;
                                        if let Some(cycle_id) = current_cycle_id.take() {
                                            let _ = response_sink.send(Response::ExecutionCompleted {
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
                        Err(error) => {
                            error!(%routine_id, error = %error, "Cron routine execution failed");
                        }
                    },
                    Err(error) => {
                        error!(error = %error, routine_id = %routine_id, "Cron routine not found");
                    }
                }

                next_run_at = cron_schedule.next_fire_at();
                emit_cron_heartbeat(&response_sink, routine_id, last_run_at, next_run_at);
                upsert_cron_session(
                    &harness,
                    &worker_id,
                    CronSessionUpsert {
                        routine_id,
                        project_id: opt_project_id,
                        memory_namespace: cron_memory_namespace.as_deref(),
                        schedule: &schedule_owned,
                        status: SessionStatus::Active,
                        last_run_at,
                        next_run_at: Some(next_run_at),
                        last_completion: None,
                    },
                )
                .await;
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

    /// Disable a cron schedule by routine id.
    pub async fn handle_cron_disable<S>(
        &self,
        ctx: &CronCommandContext<S>,
        routine_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
    {
        if let Some((_, exec)) = self.executions().remove(&routine_id) {
            exec.cancel.cancel();
            let _ = ctx.response_sink.send(Response::CronStopped { routine_id });
            upsert_cron_session(
                self,
                &ctx.worker_id,
                CronSessionUpsert {
                    routine_id,
                    project_id: None,
                    memory_namespace: None,
                    schedule: "",
                    status: SessionStatus::Cancelled,
                    last_run_at: None,
                    next_run_at: None,
                    last_completion: None,
                },
            )
            .await;
            info!(%routine_id, "Disabled cron schedule");
        }
        Ok(())
    }

    /// Trigger a routine manually.
    pub async fn handle_cron_trigger<S>(
        &self,
        ctx: &CronCommandContext<S>,
        routine_id: Uuid,
        project_id: Option<Uuid>,
    ) -> Result<()>
    where
        S: ResponseSender,
    {
        info!(%routine_id, "Manual cron trigger");

        let project_id = project_id.unwrap_or(Uuid::nil());
        let opt_project_id = if project_id.is_nil() {
            None
        } else {
            Some(project_id)
        };

        let provider = self.provider();
        let routine_name = provider
            .manifest()
            .routines
            .iter()
            .find(|routine| routine.id == routine_id)
            .map(|routine| routine.name.clone())
            .unwrap_or_else(|| routine_id.to_string());

        let execution_id = Uuid::new_v4();
        let _ = ctx.response_sink.send(Response::ExecutionStarted {
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

        let task = RoutineRun::cron(CronInput {
            task: None,
            project_id: opt_project_id,
            schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_secs(0)),
            start_at: None,
            timeout: Duration::from_secs(0),
        });

        let result = async { provider.routine_by_id(routine_id)?.run(task).await }.await;

        let (success, error, total_input_tokens, total_output_tokens) = match &result {
            Ok(result) => (
                result.passed,
                if result.passed {
                    None
                } else {
                    Some(result.output.clone())
                },
                result.input_tokens,
                result.output_tokens,
            ),
            Err(error) => (false, Some(error.to_string()), 0, 0),
        };

        let _ = ctx.response_sink.send(Response::ExecutionCompleted {
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
}
