//! Cron schedule handlers.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::Result;
use nenjo::memory::MemoryScope;
use nenjo::{CronInput, RoutineEvent, RoutineRun, Slug, TaskInput};
use nenjo_events::{CronScheduleStatus, CronTaskContent, Response};
use nenjo_sessions::{
    CheckpointQuery, CronScheduleState, ExecutionPhase, RunCompletion, ScheduleState,
    SchedulerRuntimeSnapshot, SchedulerSessionUpsert, SessionCheckpointUpdate, SessionKind,
    SessionStatus,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;

use nenjo_harness::registry::{ActiveExecution, ExecutionKind};
use nenjo_harness::{Harness, ProviderRuntime};

use crate::event_bridge::{project_slug, routine_event_to_responses};
use crate::handlers::ResponseSender;
use crate::handlers::notification::platform_notification_emitter;
use crate::resource_resolver::PlatformResourceResolver;
use crate::tools::{register_platform_notification_emitter, with_platform_notification_emitter};

#[derive(Clone)]
pub struct CronCommandContext<S> {
    pub response_sink: S,
    pub worker_id: String,
}

pub struct CronEnableRequest<'a> {
    pub routine: &'a str,
    pub project: Option<&'a str>,
    pub schedule: &'a str,
    pub timezone: Option<&'a str>,
    pub task_content: Option<CronTaskContent>,
    pub start_at: Option<chrono::DateTime<chrono::Utc>>,
}

struct CronSessionUpsert<'a> {
    routine_id: Uuid,
    project_id: Option<Uuid>,
    memory_namespace: Option<&'a str>,
    schedule: &'a str,
    timezone: Option<&'a str>,
    task: Option<&'a CronTaskContent>,
    status: SessionStatus,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    last_completion: Option<RunCompletion>,
}

async fn upsert_cron_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    worker_id: &str,
    params: CronSessionUpsert<'_>,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let CronSessionUpsert {
        routine_id,
        project_id,
        memory_namespace,
        schedule,
        timezone,
        task,
        status,
        last_run_at,
        next_run_at,
        last_completion,
    } = params;
    let manifest = harness.provider().manifest_snapshot();
    let resolver = PlatformResourceResolver::new(&manifest);
    let project = project_id
        .and_then(|project_id| resolver.project(project_id).ok().flatten())
        .map(|slug| slug.to_string());
    let routine = resolver
        .routine(routine_id)
        .ok()
        .map(|slug| slug.to_string());
    if let Err(error) = harness
        .sessions()
        .upsert_scheduler(SchedulerSessionUpsert {
            session_id: routine_id,
            kind: SessionKind::CronSchedule,
            status,
            project,
            agent: None,
            routine,
            worker_id: worker_id.to_string(),
            memory_namespace: memory_namespace.map(ToString::to_string),
            scheduler: ScheduleState::Cron(CronScheduleState {
                schedule_expr: schedule.to_string(),
                timezone: timezone.map(ToString::to_string),
                task: task.and_then(|task| serde_json::to_value(task).ok()),
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

fn resolve_cron_memory_namespace<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    routine_id: Uuid,
    project_id: Option<Uuid>,
) -> Option<String>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let manifest = harness.provider().manifest_snapshot().clone();
    let routine = manifest.routines.iter().find(|routine| {
        crate::resource_resolver::stable_resource_id("routine", &routine.slug) == routine_id
    })?;
    let mut agent_names: BTreeSet<String> = BTreeSet::new();

    for step in &routine.steps {
        if let Some(agent_slug) = &step.agent
            && let Some(agent) = manifest
                .agents
                .iter()
                .find(|agent| nenjo::Slug::derive(&agent.name) == *agent_slug)
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
    routine: &str,
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    next_fire_at: chrono::DateTime<chrono::Utc>,
) where
    S: ResponseSender,
{
    let _ = response_sink.send(Response::CronHeartbeat {
        active_schedules: vec![CronScheduleStatus {
            routine: routine.to_string(),
            last_run_at: last_run_at.map(|ts| ts.to_rfc3339()),
            next_fire_at: Some(next_fire_at.to_rfc3339()),
        }],
    });
}

fn cron_task_input(content: Option<&CronTaskContent>, project: Option<Slug>) -> Option<TaskInput> {
    let content = content?;
    let mut task = TaskInput::new(
        if content.title.trim().is_empty() {
            "Cron"
        } else {
            content.title.trim()
        },
        content
            .description
            .as_deref()
            .map(str::trim)
            .unwrap_or_default(),
    )
    .source("cron");
    if let Some(project) = project {
        task = task.with_project(project);
    }
    if let Some(criteria) = content
        .acceptance_criteria
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        task = task.acceptance_criteria(criteria);
    }
    Some(task)
}

async fn last_persisted_cron_cycle<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    routine_id: Uuid,
) -> u32
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    match harness
        .sessions()
        .latest_checkpoint(routine_id, CheckpointQuery::default())
        .await
    {
        Ok(Some(checkpoint)) => checkpoint
            .scheduler_runtime
            .and_then(|runtime| runtime.cycle)
            .unwrap_or(0),
        Ok(None) => 0,
        Err(error) => {
            error!(%routine_id, %error, "Failed to load cron checkpoint; starting cycle counter from zero");
            0
        }
    }
}

#[async_trait::async_trait]
/// Worker integration methods for cron routine platform commands.
///
/// Cron scheduling is worker-owned because it depends on process lifecycle,
/// timers, recovery, and platform response delivery. The worker uses the
/// harness/provider to execute each scheduled routine run.
pub(crate) trait WorkerCronHarnessExt<S>
where
    S: ResponseSender,
{
    /// Enable or replace a cron schedule for a routine.
    async fn handle_cron_enable(
        &self,
        ctx: &CronCommandContext<S>,
        request: CronEnableRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static;

    /// Disable an active cron schedule.
    async fn handle_cron_disable(&self, ctx: &CronCommandContext<S>, routine: &str) -> Result<()>;

    /// Trigger a cron routine immediately outside its regular schedule.
    async fn handle_cron_trigger(
        &self,
        ctx: &CronCommandContext<S>,
        routine: &str,
        project: Option<&str>,
        task_content: Option<CronTaskContent>,
    ) -> Result<()>
    where
        S: Clone + 'static;
}

#[async_trait::async_trait]
impl<P, SessionRt, S> WorkerCronHarnessExt<S> for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
{
    /// Enable a cron schedule. Keyed by routine id for cancellation.
    async fn handle_cron_enable(
        &self,
        ctx: &CronCommandContext<S>,
        request: CronEnableRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static,
    {
        let CronEnableRequest {
            routine,
            project,
            schedule,
            timezone,
            task_content,
            start_at,
        } = request;
        let manifest = self.provider().manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let routine_slug = Slug::parse(routine)?;
        let routine_id = resolver.routine_id(&routine_slug)?;
        let project_id = project
            .map(Slug::parse)
            .transpose()?
            .as_ref()
            .map(|slug| resolver.project_id(slug))
            .transpose()?;
        info!(%routine_id, %schedule, timezone, "Enabling cron schedule");

        let cron_schedule = nenjo::routines::types::parse_schedule_in_timezone(schedule, timezone)
            .map_err(|error| {
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
                turn_input: None,
            },
        );

        let manifest = self.provider().manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let project = project_id.and_then(|project_id| resolver.project(project_id).ok().flatten());
        let project_slug_for_response = project.as_ref().map(ToString::to_string);
        let routine_slug_for_response = routine_slug.to_string();
        let cron_task = cron_task_input(task_content.as_ref(), project.clone());

        let cron_run = RoutineRun::cron(CronInput {
            task: cron_task,
            project,
            schedule: cron_schedule.clone(),
            start_at: None,
            timeout: Duration::from_secs(24 * 3600),
        });

        let response_sink = ctx.response_sink.clone();
        let schedule_owned = schedule.to_string();
        let timezone_owned = timezone.map(ToOwned::to_owned);
        let task_content_for_session = task_content.clone();
        let provider_cell = self.provider_handle();
        let worker_id = ctx.worker_id.clone();
        let cron_memory_namespace = resolve_cron_memory_namespace(self, routine_id, project_id);

        let routine_name = self
            .provider()
            .manifest_snapshot()
            .routines
            .iter()
            .find(|routine| {
                crate::resource_resolver::stable_resource_id("routine", &routine.slug) == routine_id
            })
            .map(|routine| routine.name.clone())
            .unwrap_or_else(|| routine_id.to_string());

        let opt_project_id = project_id;
        let persisted_cycle = last_persisted_cron_cycle(self, routine_id).await;
        let initial_next_run_at = start_at.unwrap_or_else(|| cron_schedule.next_fire_at());
        upsert_cron_session(
            self,
            &worker_id,
            CronSessionUpsert {
                routine_id,
                project_id,
                memory_namespace: cron_memory_namespace.as_deref(),
                schedule,
                timezone,
                task: task_content.as_ref(),
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
            let mut persisted_cycle = persisted_cycle;

            emit_cron_heartbeat(
                &response_sink,
                &routine_slug_for_response,
                None,
                next_run_at,
            );
            let _ = response_sink.send(Response::CronScheduled {
                routine: routine_slug_for_response.clone(),
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
                let manifest = provider.manifest_snapshot();
                let resolver = PlatformResourceResolver::new(&manifest);
                let routine_slug = match resolver.routine(routine_id) {
                    Ok(slug) => slug,
                    Err(error) => {
                        error!(%routine_id, %error, "Routine manifest not found for cron schedule");
                        break;
                    }
                };
                match provider.routine(&routine_slug) {
                    Ok(runner) => {
                        let notification_emitter =
                            platform_notification_emitter(response_sink.clone(), routine_id);
                        let _notification_registration =
                            register_platform_notification_emitter(notification_emitter.clone());
                        let stream_result = with_platform_notification_emitter(
                            notification_emitter,
                            runner.run_stream(cron_run.clone()),
                        )
                        .await;
                        match stream_result {
                            Ok(mut handle) => {
                                let mut current_cycle_id: Option<Uuid> = None;
                                let mut current_cycle: Option<u32> = None;
                                let current_agent_id: Option<Uuid> = None;
                                let mut cycle_completed = false;
                                let mut schedule_cancelled = false;

                                loop {
                                    tokio::select! {
                                        event = handle.recv() => {
                                            match event {
                                                Some(event) => {
                                                    let mut response_execution_id =
                                                        current_cycle_id.unwrap_or(routine_id);
                                                    let response_event = match &event {
                                                        RoutineEvent::CronCycleStarted { cycle } => {
                                                            let durable_cycle = persisted_cycle.saturating_add(*cycle);
                                                            let cycle_id = Uuid::new_v4();
                                                            current_cycle_id = Some(cycle_id);
                                                            current_cycle = Some(durable_cycle);
                                                            response_execution_id = cycle_id;
                                                            let _ = harness.sessions().update_checkpoint(SessionCheckpointUpdate {
                                                                session_id: routine_id,
                                                                phase: ExecutionPhase::ExecutingTools,
                                                                worktree: None,
                                                                active_tool_name: None,
                                                                scheduler_runtime: Some(SchedulerRuntimeSnapshot {
                                                                    active_execution_id: Some(cycle_id),
                                                                    cycle: Some(durable_cycle),
                                                                }),
                                                            }).await;

                                                            let _ = response_sink.send(Response::ExecutionStarted {
                                                                id: cycle_id,
                                                                project: project_slug_for_response.clone(),
                                                                routine: Some(routine_slug_for_response.clone()),
                                                                routine_name: Some(routine_name.clone()),
                                                                agent: None,
                                                                config: serde_json::json!({
                                                                    "trigger": "cron",
                                                                    "cycle": durable_cycle,
                                                                    "schedule": schedule_owned,
                                                                    "routine": routine_slug_for_response.clone(),
                                                                }),
                                                            });
                                                            Some(RoutineEvent::CronCycleStarted {
                                                                cycle: durable_cycle,
                                                            })
                                                        }
                                                        RoutineEvent::CronCycleCompleted {
                                                            result,
                                                            total_input_tokens,
                                                            total_output_tokens,
                                                            ..
                                                        } => {
                                                            cycle_completed = true;
                                                            let durable_cycle = current_cycle
                                                                .take()
                                                                .unwrap_or_else(|| persisted_cycle.saturating_add(1));
                                                            if let Some(cycle_id) = current_cycle_id.take() {
                                                                response_execution_id = cycle_id;
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
                                                                    routine: Some(routine_slug_for_response.clone()),
                                                                    routine_name: Some(routine_name.clone()),
                                                                    agent: None,
                                                                });
                                                                persisted_cycle =
                                                                    persisted_cycle.max(durable_cycle);
                                                                upsert_cron_session(
                                                                    &harness,
                                                                    &worker_id,
                                                                    CronSessionUpsert {
                                                                        routine_id,
                                                                        project_id: opt_project_id,
                                                                        memory_namespace: cron_memory_namespace.as_deref(),
                                                                        schedule: &schedule_owned,
                                                                        timezone: timezone_owned.as_deref(),
                                                                        task: task_content_for_session.as_ref(),
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
                                                                let _ = harness.sessions().update_checkpoint(SessionCheckpointUpdate {
                                                                    session_id: routine_id,
                                                                    phase: ExecutionPhase::Waiting,
                                                                    worktree: None,
                                                                    active_tool_name: None,
                                                                    scheduler_runtime: Some(SchedulerRuntimeSnapshot {
                                                                        active_execution_id: None,
                                                                        cycle: Some(durable_cycle),
                                                                    }),
                                                                }).await;
                                                            }
                                                            Some(RoutineEvent::CronCycleCompleted {
                                                                cycle: durable_cycle,
                                                                result: result.clone(),
                                                                total_input_tokens: *total_input_tokens,
                                                                total_output_tokens: *total_output_tokens,
                                                            })
                                                        }
                                                        _ => None,
                                                    };

                                                    let event_for_response =
                                                        response_event.as_ref().unwrap_or(&event);
                                                    for response in routine_event_to_responses(
                                                        event_for_response,
                                                        response_execution_id,
                                                        None,
                                                        current_agent_id,
                                                        &provider.manifest_snapshot(),
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
                                                if let Some(cycle) = current_cycle.take() {
                                                    persisted_cycle = persisted_cycle.max(cycle);
                                                    let _ = harness.sessions().update_checkpoint(SessionCheckpointUpdate {
                                                        session_id: routine_id,
                                                        phase: ExecutionPhase::Waiting,
                                                        worktree: None,
                                                        active_tool_name: None,
                                                        scheduler_runtime: Some(SchedulerRuntimeSnapshot {
                                                            active_execution_id: None,
                                                            cycle: Some(cycle),
                                                        }),
                                                    }).await;
                                                }
                                                let _ = response_sink.send(Response::ExecutionCompleted {
                                                    id: cycle_id,
                                                    success: false,
                                                    error: Some("Cron schedule disabled".to_string()),
                                                    total_input_tokens: 0,
                                                    total_output_tokens: 0,
                                                    execution_type: Some(nenjo_events::ExecutionType::Cron),
                                                    routine: Some(routine_slug_for_response.clone()),
                                                    routine_name: Some(routine_name.clone()),
                                                    agent: None,
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
                        }
                    }
                    Err(error) => {
                        error!(error = %error, routine_id = %routine_id, "Cron routine not found");
                    }
                }

                next_run_at = cron_schedule.next_fire_at();
                emit_cron_heartbeat(
                    &response_sink,
                    &routine_slug_for_response,
                    last_run_at,
                    next_run_at,
                );
                upsert_cron_session(
                    &harness,
                    &worker_id,
                    CronSessionUpsert {
                        routine_id,
                        project_id: opt_project_id,
                        memory_namespace: cron_memory_namespace.as_deref(),
                        schedule: &schedule_owned,
                        timezone: timezone_owned.as_deref(),
                        task: task_content_for_session.as_ref(),
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
    async fn handle_cron_disable(&self, ctx: &CronCommandContext<S>, routine: &str) -> Result<()> {
        let manifest = self.provider().manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let routine_id = resolver.routine_id(&Slug::parse(routine)?)?;
        if let Some((_, exec)) = self.executions().remove(&routine_id) {
            exec.cancel.cancel();
            let _ = ctx.response_sink.send(Response::CronStopped {
                routine: routine.to_string(),
            });
            upsert_cron_session(
                self,
                &ctx.worker_id,
                CronSessionUpsert {
                    routine_id,
                    project_id: None,
                    memory_namespace: None,
                    schedule: "",
                    timezone: None,
                    task: None,
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
    async fn handle_cron_trigger(
        &self,
        ctx: &CronCommandContext<S>,
        routine: &str,
        project: Option<&str>,
        task_content: Option<CronTaskContent>,
    ) -> Result<()>
    where
        S: Clone + 'static,
    {
        let manifest = self.provider().manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let routine_id = resolver.routine_id(&Slug::parse(routine)?)?;
        let project_id = project
            .map(Slug::parse)
            .transpose()?
            .as_ref()
            .map(|slug| resolver.project_id(slug))
            .transpose()?;
        info!(%routine_id, "Manual cron trigger");

        let project_id = project_id.unwrap_or(Uuid::nil());
        let opt_project_id = if project_id.is_nil() {
            None
        } else {
            Some(project_id)
        };

        let provider = self.provider();
        let routine_slug = Slug::parse(routine)?;
        let routine_slug_for_response = routine_slug.to_string();
        let project_slug_for_response = project.map(ToOwned::to_owned);
        let routine_name = provider
            .manifest_snapshot()
            .routines
            .iter()
            .find(|routine| {
                crate::resource_resolver::stable_resource_id("routine", &routine.slug) == routine_id
            })
            .map(|routine| routine.name.clone())
            .unwrap_or_else(|| routine_id.to_string());

        let execution_id = Uuid::new_v4();
        let _ = ctx.response_sink.send(Response::ExecutionStarted {
            id: execution_id,
            project: project_slug_for_response.clone(),
            routine: Some(routine_slug_for_response.clone()),
            routine_name: Some(routine_name.clone()),
            agent: None,
            config: serde_json::json!({
                "trigger": "cron",
                "manual": true,
                "routine": routine_slug_for_response,
            }),
        });

        let manifest = provider.manifest_snapshot();
        let resolver = PlatformResourceResolver::new(&manifest);
        let project =
            opt_project_id.and_then(|project_id| resolver.project(project_id).ok().flatten());
        let cron_task = cron_task_input(task_content.as_ref(), project.clone());

        let cron_run = RoutineRun::cron(CronInput {
            task: cron_task,
            project,
            schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_secs(0)),
            start_at: None,
            timeout: Duration::from_secs(0),
        });

        let notification_emitter =
            platform_notification_emitter(ctx.response_sink.clone(), execution_id);
        let _notification_registration =
            register_platform_notification_emitter(notification_emitter.clone());
        let result = with_platform_notification_emitter(notification_emitter, async {
            let routine = resolver.routine(routine_id)?;
            provider.routine(&routine)?.run(cron_run).await
        })
        .await;

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
            routine: Some(routine_slug.to_string()),
            routine_name: Some(routine_name),
            agent: None,
        });

        info!(%routine_id, %execution_id, success, "Manual trigger complete");
        result?;
        Ok(())
    }
}
