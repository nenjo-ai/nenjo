//! Platform-free direct task execution orchestration.

use anyhow::anyhow;
use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo::{AgentRun, RoutineRun, TaskInput};
use nenjo_sessions::{
    ExecutionPhase, SessionLeaseGrant, SessionOwnerKind, SessionRuntimeEvent, SessionStatus,
    SessionTransition, TaskSessionUpsert,
};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::events::HarnessEvent;
use crate::execution_context::{project_slug, summarize_turn_event};
use crate::handle::HarnessExecutionHandle;
use crate::registry::{ActiveExecution, ExecutionKind};
use crate::request::TaskRequest;
use crate::session::{
    TurnEventContext, session_runtime_events_from_turn_event, task_session_upsert_event,
};
use crate::{Harness, ProviderRuntime};

pub(crate) async fn task_stream<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: TaskRequest,
) -> crate::Result<HarnessExecutionHandle>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    if request.routine.is_some() {
        return routine_task_stream(harness, request).await;
    }

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let prepared = prepare_task_execution(harness, request, events_tx).await?;
    let runner = build_task_runner(harness, &prepared).await?;
    let handle = runner.run_stream(prepared.run.clone()).await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let registry_token = Uuid::new_v4();

    if let Some((_, previous)) = harness.executions().remove(&prepared.task_id) {
        previous.cancel.cancel();
    }
    harness.executions().insert(
        prepared.task_id,
        ActiveExecution {
            kind: ExecutionKind::Task,
            registry_token,
            execution_run_id: Some(prepared.execution_run_id),
            cancel: cancel.clone(),
            pause: Some(handle.pause_token()),
        },
    );

    let join = spawn_task_execution(
        harness.clone(),
        handle,
        prepared,
        cancel.clone(),
        registry_token,
    );

    Ok(HarnessExecutionHandle::new(events_rx, join, cancel))
}

async fn routine_task_stream<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: TaskRequest,
) -> crate::Result<HarnessExecutionHandle>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let routine = request.routine.clone().ok_or_else(|| {
        crate::HarnessError::InvalidCommand("TaskRequest missing routine".to_string())
    })?;
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let pslug = project_slug(Some(&request.project));
    let task_slug = request.slug.clone().unwrap_or_else(|| "task".to_string());
    let execution_run_id = request.execution_run_id.unwrap_or_else(Uuid::new_v4);
    let task = task_input_from_request(&request, task_slug.clone());
    let mut run = RoutineRun::task(task).execution_run(execution_run_id);
    if let Some(location) = request.project_location.clone() {
        run = run.project_location(location);
    }
    let memory_namespace = harness.sessions().memory_namespace(request.task_id).await?;
    let lease = harness
        .sessions()
        .acquire_lease(request.task_id, "harness", SessionOwnerKind::Task)
        .await?;
    harness
        .sessions()
        .record_batch(
            &lease,
            vec![
                task_session_upsert_event(TaskSessionUpsert {
                    task_id: request.task_id,
                    status: SessionStatus::Active,
                    project: request.project.to_string(),
                    agent: None,
                    routine: Some(routine.to_string()),
                    execution_run_id,
                    memory_namespace: memory_namespace.clone(),
                    metadata: json!({
                        "source": "harness_task",
                        "project_slug": pslug,
                        "routine_slug": routine.to_string(),
                    }),
                }),
                SessionRuntimeEvent::Transition(SessionTransition {
                    session_id: request.task_id,
                    worker_id: "harness".to_string(),
                    phase: Some(ExecutionPhase::CallingModel),
                    status: SessionStatus::Active,
                }),
            ],
        )
        .await?;
    let mut handle = harness
        .provider()
        .routine(&routine)
        .map_err(anyhow::Error::from)?
        .with_session_binding(nenjo::routines::SessionBinding {
            session_id: request.task_id,
            memory_namespace,
        })
        .run_stream(run)
        .await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let registry_token = Uuid::new_v4();

    if let Some((_, previous)) = harness.executions().remove(&request.task_id) {
        previous.cancel.cancel();
    }
    harness.executions().insert(
        request.task_id,
        ActiveExecution {
            kind: ExecutionKind::Task,
            registry_token,
            execution_run_id: Some(execution_run_id),
            cancel: cancel.clone(),
            pause: None,
        },
    );

    let harness = harness.clone();
    let task_id = request.task_id;
    let join_cancel = cancel.clone();
    let join = tokio::spawn(async move {
        let lease_renewal = harness.sessions().spawn_lease_renewer(lease.clone());
        loop {
            tokio::select! {
                event = handle.recv() => {
                    match event {
                        Some(ev) => {
                            let _ = events_tx.send(HarnessEvent::Routine {
                                session_id: task_id,
                                execution_run_id,
                                event: ev,
                            });
                        }
                        None => break,
                    }
                }
                _ = join_cancel.cancelled() => {
                    handle.cancel();
                    harness.sessions().record_events(
                        lease.clone(),
                        vec![SessionRuntimeEvent::Transition(SessionTransition {
                            session_id: task_id,
                            worker_id: "harness".to_string(),
                            phase: Some(ExecutionPhase::Finalizing),
                            status: SessionStatus::Cancelled,
                        })],
                    );
                    break;
                }
            }
        }

        if harness
            .executions()
            .get(&task_id)
            .is_some_and(|entry| entry.registry_token == registry_token)
        {
            harness.executions().remove(&task_id);
        }

        if join_cancel.is_cancelled() {
            lease_renewal.cancel();
            if let Err(error) = harness.sessions().flush_events(lease.clone()).await {
                warn!(error = %error, task_id = %task_id, "Failed to flush cancelled routine task session events");
            }
            if let Err(error) = harness.sessions().release_lease(lease).await {
                warn!(error = %error, task_id = %task_id, "Failed to release cancelled routine task session lease");
            }
            return Err(crate::HarnessError::Other(anyhow!("Cancelled")));
        }

        lease_renewal.cancel();
        let result = match handle.output().await {
            Ok(result) => result,
            Err(error) => {
                harness.sessions().record_events(
                    lease.clone(),
                    vec![SessionRuntimeEvent::Transition(SessionTransition {
                        session_id: task_id,
                        worker_id: "harness".to_string(),
                        phase: Some(ExecutionPhase::Finalizing),
                        status: SessionStatus::Failed,
                    })],
                );
                if let Err(flush_error) = harness.sessions().flush_events(lease.clone()).await {
                    warn!(error = %flush_error, task_id = %task_id, "Failed to flush failed routine task session events");
                }
                if let Err(release_error) = harness.sessions().release_lease(lease).await {
                    warn!(error = %release_error, task_id = %task_id, "Failed to release failed routine task session lease");
                }
                return Err(error.into());
            }
        };
        harness.sessions().record_events(
            lease.clone(),
            vec![SessionRuntimeEvent::Transition(SessionTransition {
                session_id: task_id,
                worker_id: "harness".to_string(),
                phase: Some(ExecutionPhase::Finalizing),
                status: if result.passed {
                    SessionStatus::Completed
                } else {
                    SessionStatus::Failed
                },
            })],
        );
        if let Err(error) = harness.sessions().flush_events(lease.clone()).await {
            warn!(error = %error, task_id = %task_id, "Failed to flush completed routine task session events");
        }
        if let Err(error) = harness.sessions().release_lease(lease).await {
            warn!(error = %error, task_id = %task_id, "Failed to release completed routine task session lease");
        }

        Ok(nenjo::TurnOutput {
            task_id: result.task_id,
            text: result.output,
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            tool_calls: result.tool_calls,
            messages: result.messages,
        })
    });

    Ok(HarnessExecutionHandle::new(events_rx, join, cancel))
}

struct PreparedTaskExecution {
    task_id: Uuid,
    project: nenjo::Slug,
    execution_run_id: Uuid,
    agent_id: Option<Uuid>,
    agent: nenjo::Slug,
    agent_name: String,
    run: AgentRun,
    events_tx: mpsc::UnboundedSender<HarnessEvent>,
    lease: SessionLeaseGrant,
}

async fn prepare_task_execution<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: TaskRequest,
    events_tx: mpsc::UnboundedSender<HarnessEvent>,
) -> crate::Result<PreparedTaskExecution>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let Some(agent) = &request.agent else {
        return Err(crate::HarnessError::InvalidCommand(
            "TaskRequest requires with_agent(...) for direct task execution".to_string(),
        ));
    };

    let provider = harness.provider();
    let agent_manifest = provider
        .find_agent_manifest(agent)
        .ok_or_else(|| anyhow!("agent not found: {}", agent))?;
    let agent_id = None;
    let aname = agent_manifest.name.clone();
    let pslug = project_slug(Some(&request.project));
    let task_slug = request.slug.clone().unwrap_or_else(|| "task".to_string());
    let execution_run_id = request.execution_run_id.unwrap_or_else(Uuid::new_v4);
    let task = task_input_from_request(&request, task_slug.clone());
    let mut run = AgentRun::task(task).execution_run(execution_run_id);
    if let Some(location) = request.project_location {
        run = run.project_location(location);
    }
    let memory_namespace = task_memory_namespace(Some(&aname), &pslug);
    let lease = harness
        .sessions()
        .acquire_lease(request.task_id, "harness", SessionOwnerKind::Task)
        .await?;
    harness
        .sessions()
        .record_batch(
            &lease,
            vec![
                task_session_upsert_event(TaskSessionUpsert {
                    task_id: request.task_id,
                    status: SessionStatus::Active,
                    project: request.project.to_string(),
                    agent: Some(agent.to_string()),
                    routine: None,
                    execution_run_id,
                    memory_namespace,
                    metadata: json!({
                        "source": "harness_task",
                        "project_slug": pslug,
                        "agent_name": aname,
                        "agent_slug": agent.to_string(),
                    }),
                }),
                SessionRuntimeEvent::Transition(SessionTransition {
                    session_id: request.task_id,
                    worker_id: "harness".to_string(),
                    phase: Some(ExecutionPhase::CallingModel),
                    status: SessionStatus::Active,
                }),
            ],
        )
        .await?;

    info!(
        task_id = %request.task_id,
        execution_run_id = %execution_run_id,
        agent = %aname,
        project = %pslug,
        "Harness direct task request received"
    );

    Ok(PreparedTaskExecution {
        task_id: request.task_id,
        project: request.project,
        execution_run_id,
        agent_id,
        agent: agent.clone(),
        agent_name: aname,
        run,
        events_tx,
        lease,
    })
}

async fn build_task_runner<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    prepared: &PreparedTaskExecution,
) -> crate::Result<nenjo::AgentRunner<P>>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let provider = harness.provider();
    let mut builder = provider
        .agent(&prepared.agent)
        .await
        .map_err(anyhow::Error::from)?;

    if let Some(project) = provider.find_project(&prepared.project) {
        builder = builder.with_project_context(project);
    } else {
        warn!(
            project = %prepared.project,
            agent = %prepared.agent,
            "Project not found in manifest for harness task"
        );
    }
    if let Some(ref location) = prepared.run.execution.project_location
        && let Some(ref work_dir) = location.working_dir
    {
        builder = builder.with_work_dir(work_dir);
    }

    let runner = match harness
        .sessions()
        .memory_namespace(prepared.task_id)
        .await?
        .and_then(|namespace| MemoryScope::from_namespace(&namespace))
    {
        Some(scope) => builder.with_memory_scope(scope),
        None => builder,
    }
    .build()
    .await
    .map_err(anyhow::Error::from)?;

    Ok(runner)
}

fn spawn_task_execution<P, SessionRt>(
    harness: Harness<P, SessionRt>,
    mut handle: nenjo::ExecutionHandle,
    prepared: PreparedTaskExecution,
    cancel: tokio_util::sync::CancellationToken,
    registry_token: Uuid,
) -> tokio::task::JoinHandle<crate::Result<nenjo::TurnOutput>>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let join_cancel = cancel.clone();
    tokio::spawn(async move {
        let PreparedTaskExecution {
            task_id,
            execution_run_id,
            agent_id,
            agent_name,
            events_tx,
            lease,
            ..
        } = prepared;
        let lease_renewal = harness.sessions().spawn_lease_renewer(lease.clone());

        loop {
            tokio::select! {
                event = handle.recv() => {
                    match event {
                        Some(ev) => {
                            debug!(
                                event = %summarize_turn_event(&ev),
                                agent = %agent_name,
                                "Harness task received turn event"
                            );
                            let session_event_context = TurnEventContext {
                                session_id: task_id,
                                turn_id: Some(execution_run_id),
                                agent_id,
                                agent_name: Some(agent_name.clone()),
                                recorded_at: Utc::now(),
                            };
                            let runtime_events =
                                session_runtime_events_from_turn_event(&session_event_context, &ev);
                            let _ = events_tx.send(HarnessEvent::Turn {
                                session_id: task_id,
                                turn_id: Some(execution_run_id),
                                event: ev,
                            });
                            harness
                                .sessions()
                                .record_events(lease.clone(), runtime_events);
                        }
                        None => break,
                    }
                }
                _ = join_cancel.cancelled() => {
                    warn!(agent = %agent_name, task_id = %task_id, "Harness task execution cancelled");
                    handle.abort();
                    harness.sessions().record_events(
                        lease.clone(),
                        vec![SessionRuntimeEvent::Transition(SessionTransition {
                            session_id: task_id,
                            worker_id: "harness".to_string(),
                            phase: Some(ExecutionPhase::Finalizing),
                            status: SessionStatus::Cancelled,
                        })],
                    );
                    break;
                }
            }
        }
        if harness
            .executions()
            .get(&task_id)
            .is_some_and(|entry| entry.registry_token == registry_token)
        {
            harness.executions().remove(&task_id);
        }

        if join_cancel.is_cancelled() {
            lease_renewal.cancel();
            if let Err(error) = harness.sessions().flush_events(lease.clone()).await {
                warn!(error = %error, task_id = %task_id, "Failed to flush cancelled task session events");
            }
            if let Err(error) = harness.sessions().release_lease(lease).await {
                warn!(error = %error, task_id = %task_id, "Failed to release cancelled task session lease");
            }
            return Err(crate::HarnessError::Other(anyhow!("Cancelled")));
        }

        lease_renewal.cancel();
        let output = match handle.output().await {
            Ok(output) => output,
            Err(error) => {
                harness.sessions().record_events(
                    lease.clone(),
                    vec![SessionRuntimeEvent::Transition(SessionTransition {
                        session_id: task_id,
                        worker_id: "harness".to_string(),
                        phase: Some(ExecutionPhase::Finalizing),
                        status: SessionStatus::Failed,
                    })],
                );
                if let Err(flush_error) = harness.sessions().flush_events(lease.clone()).await {
                    warn!(error = %flush_error, task_id = %task_id, "Failed to flush failed task session events");
                }
                if let Err(release_error) = harness.sessions().release_lease(lease).await {
                    warn!(error = %release_error, task_id = %task_id, "Failed to release failed task session lease");
                }
                return Err(error.into());
            }
        };
        harness.sessions().record_events(
            lease.clone(),
            vec![SessionRuntimeEvent::Transition(SessionTransition {
                session_id: task_id,
                worker_id: "harness".to_string(),
                phase: Some(ExecutionPhase::Finalizing),
                status: SessionStatus::Completed,
            })],
        );
        if let Err(error) = harness.sessions().flush_events(lease.clone()).await {
            warn!(error = %error, task_id = %task_id, "Failed to flush completed task session events");
        }
        if let Err(error) = harness.sessions().release_lease(lease).await {
            warn!(error = %error, task_id = %task_id, "Failed to release completed task session lease");
        }

        Ok(output)
    })
}

fn task_input_from_request(request: &TaskRequest, task_slug: String) -> TaskInput {
    TaskInput {
        project: Some(request.project.clone()),
        task_id: request.task_id,
        title: request.title.clone(),
        description: request.description.clone(),
        acceptance_criteria: request.acceptance_criteria.clone(),
        tags: request.tags.clone(),
        source: Some("task".to_string()),
        status: request.status.clone(),
        priority: request.priority.clone(),
        task_type: request.task_type.clone(),
        slug: Some(task_slug),
        complexity: request.complexity.clone(),
    }
}

fn task_memory_namespace(agent_name: Option<&str>, project_slug: &str) -> Option<String> {
    agent_name.map(|agent_name| {
        MemoryScope::new(
            agent_name,
            if project_slug.is_empty() {
                None
            } else {
                Some(project_slug)
            },
        )
        .project
    })
}
