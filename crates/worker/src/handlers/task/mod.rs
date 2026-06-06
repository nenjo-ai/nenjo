//! Task execution handlers — with git worktree lifecycle.
mod runtime;
use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::Utc;
use dashmap::mapref::entry::Entry;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    CheckpointQuery, ExecutionPhase, SessionCheckpointUpdate, SessionOwnerKind, SessionStatus,
    SessionTransition, TaskSessionUpsert, WorktreeSnapshot,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo::types::GitContext;
use nenjo::{ProjectLocation, Slug, TaskInput};
use nenjo_events::{Response, StepAgent};
use serde_json::json;

use nenjo_harness::events::HarnessEvent;
use nenjo_harness::registry::{ActiveExecution, ExecutionKind};
use nenjo_harness::request::TaskRequest;
use nenjo_harness::session::{TurnEventContext, session_runtime_events_from_turn_event};
use nenjo_harness::{Harness, ProviderRuntime};

use crate::event_bridge::{
    TaskTurnEventContext, agent_name, project_slug, routine_event_to_response,
    turn_event_to_task_step_response,
};
use crate::handlers::ResponseSender;
use crate::resource_resolver::PlatformResourceResolver;
use crate::runtime::GitLocks;
pub use runtime::{TaskCommandContext, TaskWorktreeManager};

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

async fn restore_task_git_context<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    task_id: Uuid,
) -> Option<GitContext>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let record = harness.sessions().get(task_id).await.ok().flatten()?;
    let _checkpoint_ref = record.refs.checkpoint_ref?;
    let checkpoint = harness
        .sessions()
        .latest_checkpoint(task_id, CheckpointQuery::default())
        .await
        .ok()
        .flatten()?;
    let worktree = checkpoint.worktree?;
    if worktree.work_dir.is_empty()
        || worktree.repo_dir.is_empty()
        || !Path::new(&worktree.work_dir).exists()
        || !Path::new(&worktree.repo_dir).exists()
    {
        return None;
    }
    Some(GitContext {
        branch: worktree.branch,
        target_branch: worktree.target_branch.unwrap_or_else(|| "main".to_string()),
        work_dir: worktree.work_dir,
        repo_url: String::new(),
    })
}

#[derive(Clone)]
struct TaskSessionRecord<'a> {
    task_id: Uuid,
    memory_namespace: Option<&'a str>,
    execution_run_id: Uuid,
    status: SessionStatus,
}

#[derive(Clone, Copy)]
enum SessionUpsertMode {
    Await,
    Spawn,
}

async fn upsert_task_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    params: &TaskSessionRecord<'_>,
    routine_slug: Option<&str>,
    project_slug: &str,
    agent_name: Option<&str>,
    agent_slug: Option<&str>,
    mode: SessionUpsertMode,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let upsert = TaskSessionUpsert {
        task_id: params.task_id,
        status: params.status,
        project: project_slug.to_string(),
        agent: agent_slug.map(ToString::to_string),
        routine: routine_slug.map(ToString::to_string),
        execution_run_id: params.execution_run_id,
        memory_namespace: params.memory_namespace.map(ToOwned::to_owned),
        metadata: json!({
            "source": "worker_task",
            "project_slug": project_slug,
            "agent_name": agent_name,
        }),
    };

    match mode {
        SessionUpsertMode::Await => {
            if let Err(error) = harness.sessions().upsert_task(upsert).await {
                warn!(
                    error = %error,
                    task_id = %params.task_id,
                    "Failed to upsert task session"
                );
            }
        }
        SessionUpsertMode::Spawn => {
            let harness = harness.clone();
            let task_id = params.task_id;
            tokio::spawn(async move {
                if let Err(error) = harness.sessions().upsert_task(upsert).await {
                    warn!(
                        error = %error,
                        task_id = %task_id,
                        "Failed to upsert task session"
                    );
                }
            });
        }
    }
}

fn record_task_turn_event<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    task_id: Uuid,
    agent_id: Option<Uuid>,
    agent_name: Option<&str>,
    event: &nenjo::TurnEvent,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let context = TurnEventContext {
        session_id: task_id,
        turn_id: None,
        agent_id,
        agent_name: agent_name.map(ToOwned::to_owned),
        recorded_at: Utc::now(),
    };
    harness.sessions().record_events_best_effort(
        task_id,
        SessionOwnerKind::Task,
        session_runtime_events_from_turn_event(&context, event),
    );
}

async fn update_task_checkpoint<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    task_id: Uuid,
    phase: ExecutionPhase,
    worktree: Option<WorktreeSnapshot>,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    if let Err(error) = harness
        .sessions()
        .update_checkpoint(SessionCheckpointUpdate {
            session_id: task_id,
            phase,
            worktree,
            active_tool_name: None,
            scheduler_runtime: None,
        })
        .await
    {
        warn!(
            error = %error,
            task_id = %task_id,
            "Failed to update task checkpoint through session runtime"
        );
    }
}

async fn transition_task_session<P, SessionRt, S, W>(
    harness: &Harness<P, SessionRt>,
    ctx: &TaskCommandContext<S, W>,
    task_id: Uuid,
    phase: Option<ExecutionPhase>,
    status: SessionStatus,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let _ = harness
        .sessions()
        .transition(SessionTransition {
            session_id: task_id,
            worker_id: ctx.worker_id.clone(),
            phase,
            status,
        })
        .await;
}

fn task_worktree_snapshot(
    repo_dir: Option<&Path>,
    git_ctx: Option<&GitContext>,
) -> Option<WorktreeSnapshot> {
    git_ctx.map(|git| WorktreeSnapshot {
        repo_dir: repo_dir
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
        work_dir: git.work_dir.clone(),
        branch: git.branch.clone(),
        target_branch: if git.target_branch.is_empty() {
            None
        } else {
            Some(git.target_branch.clone())
        },
    })
}

#[derive(Debug, Clone)]
struct TaskExecutionOutcome {
    success: bool,
    error: Option<String>,
    total_input_tokens: u64,
    total_output_tokens: u64,
}

impl TaskExecutionOutcome {
    fn success(total_input_tokens: u64, total_output_tokens: u64) -> Self {
        Self {
            success: true,
            error: None,
            total_input_tokens,
            total_output_tokens,
        }
    }

    fn failed<Error>(error: Error, total_input_tokens: u64, total_output_tokens: u64) -> Self
    where
        Error: Into<String>,
    {
        Self {
            success: false,
            error: Some(error.into()),
            total_input_tokens,
            total_output_tokens,
        }
    }
}

pub struct TaskExecuteRequest<'a> {
    pub task_id: Uuid,
    pub project: &'a str,
    pub routine: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub execution_run_id: Uuid,
    pub title: &'a str,
    pub description: &'a str,
    pub slug: Option<&'a str>,
    pub acceptance_criteria: Option<&'a str>,
    pub tags: &'a [String],
    pub status: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub task_type: Option<&'a str>,
    pub complexity: Option<&'a str>,
}

/// Worker integration methods for task execution platform commands.
///
/// The worker owns platform task semantics such as response streaming,
/// git-worktree lifecycle, pause/resume/cancel routing, and checkpoint updates.
/// Actual agent/routine execution still goes through the harness/provider.
#[async_trait::async_trait]
pub(crate) trait WorkerTaskHarnessExt<S, W>
where
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    /// Execute a task command and stream platform responses.
    async fn handle_task_execute(
        &self,
        ctx: &TaskCommandContext<S, W>,
        request: TaskExecuteRequest<'_>,
    ) -> Result<()>;

    /// Cancel an active task execution by execution run id.
    async fn handle_execution_cancel(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()>;

    /// Pause an active task execution by execution run id.
    async fn handle_execution_pause(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()>;

    /// Resume a paused task execution by execution run id.
    async fn handle_execution_resume(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()>;
}

#[async_trait::async_trait]
impl<P, SessionRt, S, W> WorkerTaskHarnessExt<S, W> for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    async fn handle_task_execute(
        &self,
        ctx: &TaskCommandContext<S, W>,
        request: TaskExecuteRequest<'_>,
    ) -> Result<()> {
        handle_task_execute(self, ctx, request).await
    }

    async fn handle_execution_cancel(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()> {
        handle_execution_cancel(self, ctx, execution_run_id).await
    }

    async fn handle_execution_pause(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()> {
        handle_execution_pause(self, ctx, execution_run_id).await
    }

    async fn handle_execution_resume(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()> {
        handle_execution_resume(self, ctx, execution_run_id).await
    }
}

async fn handle_task_execute<P, SessionRt, S, W>(
    harness: &Harness<P, SessionRt>,
    ctx: &TaskCommandContext<S, W>,
    request: TaskExecuteRequest<'_>,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let TaskExecuteRequest {
        task_id,
        project,
        routine,
        agent,
        execution_run_id,
        title,
        description,
        slug,
        acceptance_criteria,
        tags,
        status,
        priority,
        task_type,
        complexity,
    } = request;
    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
    let resolver = PlatformResourceResolver::new(&manifest);
    let project = Slug::parse(project)?;
    let project_id = resolver.project_id(&project)?;
    let pslug = project_slug(&manifest, project_id);
    let agent_slug = agent.map(Slug::parse).transpose()?;
    let routine_slug = routine.map(Slug::parse).transpose()?;
    let assigned_agent_id = agent_slug
        .as_ref()
        .map(|slug| resolver.agent_id(slug))
        .transpose()?;
    let routine_id = routine_slug
        .as_ref()
        .map(|slug| resolver.routine_id(slug))
        .transpose()?;
    let task_slug = slug.unwrap_or("task");
    let repo_dir = ctx.worktrees.repo_dir(&pslug);
    let cancel = CancellationToken::new();
    let pause = nenjo::agents::runner::types::PauseToken::new();
    let registry_token = Uuid::new_v4();

    let executions = harness.executions();
    match executions.entry(task_id) {
        Entry::Occupied(entry) => {
            let active = entry.get();
            warn!(
                task_id = %task_id,
                execution_run_id = %execution_run_id,
                active_execution_run_id = ?active.execution_run_id,
                active_kind = ?active.kind,
                "Ignoring duplicate task.execute for already active task"
            );
            return Ok(());
        }
        Entry::Vacant(entry) => {
            entry.insert(ActiveExecution {
                kind: ExecutionKind::Task,
                registry_token,
                execution_run_id: Some(execution_run_id),
                cancel: cancel.clone(),
                pause: Some(pause.clone()),
            });
        }
    }

    // Resolve target branch from project settings.
    let target_branch = manifest
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .and_then(|p| p.settings.get("target_branch"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    let aname = assigned_agent_id.map(|id| agent_name(&manifest, id));
    let task_memory_namespace = task_memory_namespace(aname.as_deref(), &pslug);
    let active_session = TaskSessionRecord {
        task_id,
        memory_namespace: task_memory_namespace.as_deref(),
        execution_run_id,
        status: SessionStatus::Active,
    };
    upsert_task_session(
        harness,
        &active_session,
        routine_slug.as_ref().map(|slug| slug.as_str()),
        &pslug,
        aname.as_deref(),
        agent_slug.as_ref().map(|slug| slug.as_str()),
        SessionUpsertMode::Await,
    )
    .await;
    update_task_checkpoint(
        harness,
        task_id,
        ExecutionPhase::Preparing,
        task_worktree_snapshot(Some(&repo_dir), None),
    )
    .await;

    info!(
        agent = ?aname,
        task_id = %task_id,
        routine_id = ?routine_id,
        execution_run_id = %execution_run_id,
        project = %pslug,
        title = %title,
        "Task execution started"
    );

    // Set up git worktree if the project has a synced repo.
    // If the repo exists but worktree creation fails, the task fails —
    // we don't run tasks against a dirty or shared working tree.
    let eid = execution_run_id.to_string();
    let tid = Some(task_id.to_string());
    // Per-repo mutex — git's .git/config lock doesn't support concurrent writes,
    // so parallel worktree add/remove on the same repo must be serialized.
    let git_locks = ctx.git_locks.clone();
    let git_lock = git_locks
        .entry(repo_dir.clone())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone();

    let restored_git_ctx = restore_task_git_context(harness, task_id).await;
    let git_ctx = if let Some(wt) = restored_git_ctx {
        info!(branch = %wt.branch, work_dir = %wt.work_dir, "Restored git worktree from task checkpoint");
        let _ = ctx.response_sink.send(Response::TaskStepEvent {
            execution_run_id: eid.clone(),
            task_id: tid.clone(),
            event_type: "step_completed".to_string(),
            step_name: "worktree_restore".to_string(),
            step_type: "worktree".to_string(),
            duration_ms: Some(0),
            data: serde_json::json!({
                "branch": wt.branch,
                "target_branch": wt.target_branch,
            }),
            payload: Some(serde_json::json!({
                "work_dir": wt.work_dir,
            })),
            encrypted_payload: None,
            agent: None,
        });
        Some(wt)
    } else if repo_dir.join(".git").exists() {
        let _ = ctx.response_sink.send(Response::TaskStepEvent {
            execution_run_id: eid.clone(),
            task_id: tid.clone(),
            event_type: "step_started".to_string(),
            step_name: "worktree_setup".to_string(),
            step_type: "worktree".to_string(),
            duration_ms: None,
            data: serde_json::Value::Null,
            payload: None,
            encrypted_payload: None,
            agent: None,
        });

        let start = std::time::Instant::now();
        let setup_result = {
            let _guard = git_lock.lock().await;
            ctx.worktrees
                .setup_worktree(&repo_dir, execution_run_id, task_slug, target_branch)
                .await
        };
        match setup_result {
            Ok(wt) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                info!(branch = %wt.branch, work_dir = %wt.work_dir, "Created git worktree for task");

                let _ = ctx.response_sink.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_completed".to_string(),
                    step_name: "worktree_setup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({
                        "branch": wt.branch,
                        "target_branch": wt.target_branch,
                    }),
                    payload: Some(serde_json::json!({
                        "work_dir": wt.work_dir,
                    })),
                    encrypted_payload: None,
                    agent: None,
                });

                Some(wt)
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let error_msg = format!("{e:#}");
                warn!(error = %error_msg, "Worktree setup failed");

                let _ = ctx.response_sink.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_failed".to_string(),
                    step_name: "worktree_setup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({ "error": "Worktree setup failed" }),
                    payload: Some(serde_json::json!({ "error": &error_msg })),
                    encrypted_payload: None,
                    agent: None,
                });

                send_task_failed(ctx, &eid, &tid, &error_msg);
                update_task_checkpoint(
                    harness,
                    task_id,
                    ExecutionPhase::Finalizing,
                    task_worktree_snapshot(Some(&repo_dir), None),
                )
                .await;
                let failed_session = TaskSessionRecord {
                    task_id,
                    memory_namespace: task_memory_namespace.as_deref(),
                    execution_run_id,
                    status: SessionStatus::Failed,
                };
                upsert_task_session(
                    harness,
                    &failed_session,
                    routine_slug.as_ref().map(|slug| slug.as_str()),
                    &pslug,
                    aname.as_deref(),
                    agent_slug.as_ref().map(|slug| slug.as_str()),
                    SessionUpsertMode::Spawn,
                )
                .await;
                harness.executions().remove(&task_id);
                return Ok(());
            }
        }
    } else {
        None
    };

    let task = TaskInput {
        project: Some(project.clone()),
        task_id,
        title: title.to_string(),
        description: description.to_string(),
        acceptance_criteria: acceptance_criteria.map(|s| s.to_string()),
        tags: tags.to_vec(),
        source: Some("task".to_string()),
        status: status.map(ToOwned::to_owned),
        priority: priority.map(ToOwned::to_owned),
        task_type: task_type.map(ToOwned::to_owned),
        slug: Some(task_slug.to_string()),
        complexity: complexity.map(ToOwned::to_owned),
    };
    let mut request =
        TaskRequest::from_task_input(&task, project).with_execution_run(execution_run_id);
    if let Some(location) = git_ctx.clone().map(ProjectLocation::from_git) {
        request = request.with_project_location(location);
    }

    update_task_checkpoint(
        harness,
        task_id,
        ExecutionPhase::CallingModel,
        task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
    )
    .await;

    let execution = TaskExecutionShared {
        harness,
        command_ctx: ctx,
        execution_run_id,
        task_id,
        task_slug,
        cancel: &cancel,
    };

    let result = if let Some(rid) = routine_id {
        let routine = routine_slug
            .clone()
            .ok_or_else(|| anyhow!("routine not found: {rid}"))?;
        execute_routine_task(RoutineTaskExecution {
            shared: execution,
            request: request.clone().with_routine(routine),
        })
        .await
    } else if let Some(aid) = assigned_agent_id {
        let agent = agent_slug
            .clone()
            .ok_or_else(|| anyhow!("agent not found: {aid}"))?;
        execute_direct_task(DirectTaskExecution {
            shared: execution,
            agent_id: aid,
            request: request.clone().with_agent(agent),
        })
        .await
    } else {
        warn!("TaskExecute without routine_id or assigned_agent_id");
        Err(anyhow!("No routine_id or assigned_agent_id"))
    };

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(ref e) => TaskExecutionOutcome::failed(format!("{e:#}"), 0, 0),
    };

    // If execution itself errored (e.g. routine not found, agent build failure),
    // clean up before telling the platform the task is terminal.
    if !outcome.success {
        update_task_checkpoint(
            harness,
            task_id,
            ExecutionPhase::Finalizing,
            task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
        )
        .await;
        harness.executions().remove(&task_id);
        // Still clean up worktree even on failure.
        if let Some(ref wt) = git_ctx {
            let _guard = git_lock.lock().await;
            if let Err(e) = ctx
                .worktrees
                .cleanup_worktree(&repo_dir, &wt.work_dir, &wt.branch)
                .await
            {
                warn!(error = %e, branch = %wt.branch, "Failed to clean up worktree");
            }
        }
        let failed_session = TaskSessionRecord {
            task_id,
            memory_namespace: task_memory_namespace.as_deref(),
            execution_run_id,
            status: SessionStatus::Failed,
        };
        upsert_task_session(
            harness,
            &failed_session,
            routine_slug.as_ref().map(|slug| slug.as_str()),
            &pslug,
            aname.as_deref(),
            agent_slug.as_ref().map(|slug| slug.as_str()),
            SessionUpsertMode::Spawn,
        )
        .await;
        send_task_completed(ctx, &eid, &tid, &outcome);
        evict_git_lock(&git_locks, &repo_dir, &git_lock);
        return Ok(());
    }

    // Unregister execution
    harness.executions().remove(&task_id);
    let final_status = if cancel.is_cancelled() {
        SessionStatus::Cancelled
    } else {
        SessionStatus::Completed
    };
    if final_status != SessionStatus::Cancelled {
        update_task_checkpoint(
            harness,
            task_id,
            ExecutionPhase::Finalizing,
            task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
        )
        .await;
    }

    // Clean up worktree after execution
    if let Some(ref wt) = git_ctx
        && final_status != SessionStatus::Cancelled
    {
        let _ = ctx.response_sink.send(Response::TaskStepEvent {
            execution_run_id: eid.clone(),
            task_id: tid.clone(),
            event_type: "step_started".to_string(),
            step_name: "worktree_cleanup".to_string(),
            step_type: "worktree".to_string(),
            duration_ms: None,
            data: serde_json::Value::Null,
            payload: None,
            encrypted_payload: None,
            agent: None,
        });

        let start = std::time::Instant::now();
        let cleanup_result: Result<()> = {
            let _guard = git_lock.lock().await;
            ctx.worktrees
                .cleanup_worktree(&repo_dir, &wt.work_dir, &wt.branch)
                .await
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        match &cleanup_result {
            Ok(()) => {
                debug!(branch = %wt.branch, "Cleaned up worktree");
                let _ = ctx.response_sink.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_completed".to_string(),
                    step_name: "worktree_cleanup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({ "branch": wt.branch }),
                    payload: None,
                    encrypted_payload: None,
                    agent: None,
                });
            }
            Err(e) => {
                warn!(error = %e, branch = %wt.branch, "Failed to clean up worktree");
                let _ = ctx.response_sink.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_failed".to_string(),
                    step_name: "worktree_cleanup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({ "error": "Worktree cleanup failed" }),
                    payload: Some(serde_json::json!({ "error": e.to_string() })),
                    encrypted_payload: None,
                    agent: None,
                });
            }
        }
    }

    let final_session = TaskSessionRecord {
        task_id,
        memory_namespace: task_memory_namespace.as_deref(),
        execution_run_id,
        status: final_status,
    };
    upsert_task_session(
        harness,
        &final_session,
        routine_slug.as_ref().map(|slug| slug.as_str()),
        &pslug,
        aname.as_deref(),
        agent_slug.as_ref().map(|slug| slug.as_str()),
        SessionUpsertMode::Spawn,
    )
    .await;
    send_task_completed(ctx, &eid, &tid, &outcome);
    evict_git_lock(&git_locks, &repo_dir, &git_lock);
    Ok(())
}

/// Cancel all tasks belonging to an execution run.
async fn handle_execution_cancel<P, SessionRt, S, W>(
    harness: &Harness<P, SessionRt>,
    ctx: &TaskCommandContext<S, W>,
    execution_run_id: Uuid,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let mut cancelled = 0u32;
    // Collect keys first to avoid holding DashMap ref during remove.
    let keys: Vec<Uuid> = harness
        .executions()
        .iter()
        .filter(|e| e.execution_run_id == Some(execution_run_id))
        .map(|e| *e.key())
        .collect();
    for key in keys {
        if let Some((_, exec)) = harness.executions().remove(&key) {
            exec.cancel.cancel();
            transition_task_session(
                harness,
                ctx,
                key,
                Some(ExecutionPhase::Waiting),
                SessionStatus::Cancelled,
            )
            .await;
            cancelled += 1;
        }
    }
    if cancelled > 0 {
        info!(%execution_run_id, cancelled, "Cancelled active task executions");
    }
    Ok(())
}

/// Pause all tasks belonging to an execution run.
async fn handle_execution_pause<P, SessionRt, S, W>(
    harness: &Harness<P, SessionRt>,
    ctx: &TaskCommandContext<S, W>,
    execution_run_id: Uuid,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let mut paused = 0u32;
    for entry in harness.executions().iter() {
        if entry.execution_run_id == Some(execution_run_id)
            && let Some(ref pt) = entry.pause
        {
            pt.pause();
            transition_task_session(
                harness,
                ctx,
                *entry.key(),
                Some(ExecutionPhase::Waiting),
                SessionStatus::Paused,
            )
            .await;
            paused += 1;
        }
    }
    if paused > 0 {
        info!(%execution_run_id, paused, "Paused task executions");
    }
    Ok(())
}

/// Resume all paused tasks belonging to an execution run.
async fn handle_execution_resume<P, SessionRt, S, W>(
    harness: &Harness<P, SessionRt>,
    ctx: &TaskCommandContext<S, W>,
    execution_run_id: Uuid,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let mut resumed = 0u32;
    for entry in harness.executions().iter() {
        if entry.execution_run_id == Some(execution_run_id)
            && let Some(ref pt) = entry.pause
        {
            pt.resume();
            transition_task_session(
                harness,
                ctx,
                *entry.key(),
                Some(ExecutionPhase::CallingModel),
                SessionStatus::Active,
            )
            .await;
            resumed += 1;
        }
    }
    if resumed > 0 {
        info!(%execution_run_id, resumed, "Resumed task executions");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Execution helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct TaskExecutionShared<
    'a,
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
> {
    harness: &'a Harness<P, SessionRt>,
    command_ctx: &'a TaskCommandContext<S, W>,
    execution_run_id: Uuid,
    task_id: Uuid,
    task_slug: &'a str,
    cancel: &'a CancellationToken,
}

struct RoutineTaskExecution<
    'a,
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
> {
    shared: TaskExecutionShared<'a, P, SessionRt, S, W>,
    request: TaskRequest,
}

struct DirectTaskExecution<
    'a,
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
> {
    shared: TaskExecutionShared<'a, P, SessionRt, S, W>,
    agent_id: Uuid,
    request: TaskRequest,
}

async fn execute_routine_task<P, SessionRt, S, W>(
    exec: RoutineTaskExecution<'_, P, SessionRt, S, W>,
) -> Result<TaskExecutionOutcome>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let TaskExecutionShared {
        harness,
        command_ctx: ctx,
        execution_run_id,
        task_id,
        task_slug,
        cancel,
    } = exec.shared;
    let mut request = exec.request;
    if request.slug.is_none() {
        request = request.with_slug(task_slug.to_string());
    }
    let mut stream = harness.task_stream(request).await?;

    // Accumulate token metrics from step events as they stream through.
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    // Track the current agent_id so step_completed events can carry it.
    let mut current_agent_id: Option<uuid::Uuid> = None;
    let mut routine_passed = false;
    let mut step_agents: std::collections::HashMap<uuid::Uuid, (uuid::Uuid, String)> =
        std::collections::HashMap::new();

    loop {
        tokio::select! {
            event = stream.recv() => {
                match event {
                    Some(HarnessEvent::Routine { event: ev, .. }) => {
                        // Track agent identity across step events.
                        if let nenjo::RoutineEvent::StepStarted { step_run_id, step_name, agent_id, .. } = &ev {
                            current_agent_id = *agent_id;
                            if let Some(agent_id) = agent_id {
                                step_agents.insert(*step_run_id, (*agent_id, step_name.clone()));
                            }
                        }
                        // Track token totals from completed steps
                        if let nenjo::RoutineEvent::StepCompleted { result, .. } = &ev {
                            total_input_tokens += result.input_tokens;
                            total_output_tokens += result.output_tokens;
                        }
                        if let nenjo::RoutineEvent::Done { result, .. } = &ev {
                            routine_passed = result.passed;
                        }
                        if let nenjo::RoutineEvent::AgentEvent { step_id, step_run_id, event } = &ev
                            && let Some((agent_id, step_name)) = step_agents.get(step_run_id)
                        {
                            let agent_name = harness.provider()
                                .manifest_snapshot()
                                .agents
                                .iter()
                                .find(|agent| agent.id == *agent_id)
                                .map(|agent| agent.name.clone())
                                .unwrap_or_else(|| "agent".to_string());
                            record_task_turn_event(
                                harness,
                                task_id,
                                Some(*agent_id),
                                Some(&agent_name),
                                event,
                            );
                            let _ = (step_id, step_name);
                            let _ = event;
                        }
                        if let Some(r) = routine_event_to_response(&ev, execution_run_id, Some(task_id), current_agent_id, &harness.provider().manifest_snapshot()) {
                            let _ = ctx.response_sink.send(r);
                        }
                    }
                    Some(HarnessEvent::Turn { .. }) | Some(HarnessEvent::DomainEntered { .. }) => {}
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                stream.cancel();
                break;
            }
        }
    }

    let output = stream.output().await?;
    Ok(if cancel.is_cancelled() {
        TaskExecutionOutcome::failed("Cancelled", total_input_tokens, total_output_tokens)
    } else if routine_passed {
        TaskExecutionOutcome::success(total_input_tokens, total_output_tokens)
    } else {
        TaskExecutionOutcome::failed(output.text, total_input_tokens, total_output_tokens)
    })
}

async fn execute_direct_task<P, SessionRt, S, W>(
    exec: DirectTaskExecution<'_, P, SessionRt, S, W>,
) -> Result<TaskExecutionOutcome>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let TaskExecutionShared {
        harness,
        command_ctx: ctx,
        execution_run_id,
        task_id,
        task_slug,
        cancel,
    } = exec.shared;
    let DirectTaskExecution {
        agent_id, request, ..
    } = exec;
    let manifest = harness.provider().manifest_snapshot();
    let aname = agent_name(&manifest, agent_id);
    let agent_slug = request
        .agent
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| nenjo::Slug::derive(&aname).to_string());
    let mut request = request;
    if request.slug.is_none() {
        request = request.with_slug(task_slug.to_string());
    }
    let task_started_at = std::time::Instant::now();
    let mut stream = harness.task_stream(request).await?;

    loop {
        tokio::select! {
            event = stream.recv() => {
                match event {
                    Some(HarnessEvent::Turn { event: ev, .. }) => {
                        let agent_duration_ms = if matches!(ev, nenjo::TurnEvent::Done { .. }) {
                            Some(task_started_at.elapsed().as_millis() as u64)
                        } else {
                            None
                        };
                        if let Some(response) = turn_event_to_task_step_response(
                            &ev,
                            &TaskTurnEventContext {
                                execution_run_id,
                                task_id: Some(task_id),
                                agent: Some(StepAgent {
                                    agent: agent_slug.clone(),
                                    agent_name: Some(aname.clone()),
                                    agent_color: manifest
                                        .agents
                                        .iter()
                                        .find(|a| a.id == agent_id)
                                        .and_then(|a| a.color.clone()),
                                }),
                                routine_step: None,
                                agent_duration_ms,
                                emit_done: true,
                                summarize_outputs: false,
                            },
                        ) {
                            let _ = ctx.response_sink.send(response);
                        }
                    }
                    Some(HarnessEvent::DomainEntered { .. }) | Some(HarnessEvent::Routine { .. }) => {}
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                stream.cancel();
                break;
            }
        }
    }

    let outcome = if !cancel.is_cancelled() {
        let output = stream.output().await?;
        TaskExecutionOutcome::success(output.input_tokens, output.output_tokens)
    } else {
        TaskExecutionOutcome::failed("Cancelled", 0, 0)
    };
    Ok(outcome)
}

fn send_task_completed<S, W>(
    ctx: &TaskCommandContext<S, W>,
    eid: &str,
    tid: &Option<String>,
    outcome: &TaskExecutionOutcome,
) where
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let _ = ctx.response_sink.send(Response::TaskCompleted {
        execution_run_id: eid.to_string(),
        task_id: tid.clone(),
        success: outcome.success,
        error: outcome.error.clone(),
        merge_error: None,
        total_input_tokens: outcome.total_input_tokens,
        total_output_tokens: outcome.total_output_tokens,
    });
}

/// Send `TaskCompleted` (failed) to the platform.
///
/// Used for early termination when the task cannot proceed before the normal
/// execution/finalization path is reached.
fn send_task_failed<S, W>(
    ctx: &TaskCommandContext<S, W>,
    eid: &str,
    tid: &Option<String>,
    error: &str,
) where
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    send_task_completed(ctx, eid, tid, &TaskExecutionOutcome::failed(error, 0, 0));
}

// ---------------------------------------------------------------------------
// Git worktree lifecycle
// ---------------------------------------------------------------------------

/// Remove a repo's lock entry when no other task is using it.
///
/// The map holds one `Arc` and the caller holds another. If the strong count is
/// exactly 2, no other task is sharing this lock, so we can safely evict it.
fn evict_git_lock(
    locks: &GitLocks,
    repo_dir: &std::path::Path,
    lock: &std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    if std::sync::Arc::strong_count(lock) <= 2 {
        locks.remove(repo_dir);
    }
}
