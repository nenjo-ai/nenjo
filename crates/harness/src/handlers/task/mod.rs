//! Task execution handlers — with git worktree lifecycle.
mod runtime;
use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::Utc;
use dashmap::mapref::entry::Entry;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    CheckpointQuery, ExecutionPhase, SessionCheckpointUpdate, SessionStatus, SessionTransition,
    TaskSessionUpsert, WorktreeSnapshot,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo::types::GitContext;
use nenjo::{AgentRun, ProjectLocation, RoutineRun, TaskInput};
use nenjo_events::{Response, StepAgent};
use serde_json::json;

use super::ResponseSender;
use crate::event_bridge::{
    TaskTurnEventContext, agent_name, project_slug, routine_event_to_response,
    turn_event_to_task_step_response,
};
use crate::execution_trace::{
    ExecutionTraceRuntime, ExecutionTraceTarget, ExecutionTraceWriter, TraceAgent,
};
use crate::session::{
    TurnEventContext, session_runtime_events_from_turn_event, spawn_session_events,
};
use crate::{ActiveExecution, ExecutionKind, GitLocks, Harness, HarnessProvider};
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

async fn restore_task_git_context<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    task_id: Uuid,
) -> Option<GitContext>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let record = harness.get_session(task_id).await.ok().flatten()?;
    let _checkpoint_ref = record.refs.checkpoint_ref?;
    let checkpoint = harness
        .load_latest_checkpoint(task_id, CheckpointQuery::default())
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
    project_id: Uuid,
    agent_id: Option<Uuid>,
    memory_namespace: Option<&'a str>,
    execution_run_id: Uuid,
    trace_ref: Option<String>,
    status: SessionStatus,
}

#[derive(Clone, Copy)]
enum SessionUpsertMode {
    Await,
    Spawn,
}

async fn upsert_task_session<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    params: &TaskSessionRecord<'_>,
    routine_id: Option<Uuid>,
    project_slug: &str,
    agent_name: Option<&str>,
    mode: SessionUpsertMode,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let upsert = TaskSessionUpsert {
        task_id: params.task_id,
        status: params.status,
        project_id: params.project_id,
        agent_id: params.agent_id,
        routine_id,
        execution_run_id: params.execution_run_id,
        memory_namespace: params.memory_namespace.map(ToOwned::to_owned),
        trace_ref: params.trace_ref.clone(),
        metadata: json!({
            "source": "worker_task",
            "project_slug": project_slug,
            "agent_name": agent_name,
        }),
    };

    match mode {
        SessionUpsertMode::Await => {
            if let Err(error) = harness.upsert_task_session(upsert).await {
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
                if let Err(error) = harness.upsert_task_session(upsert).await {
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

fn record_task_turn_event<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    task_id: Uuid,
    agent_id: Option<Uuid>,
    agent_name: Option<&str>,
    event: &nenjo::TurnEvent,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let context = TurnEventContext {
        session_id: task_id,
        turn_id: None,
        agent_id,
        agent_name: agent_name.map(ToOwned::to_owned),
        recorded_at: Utc::now(),
    };
    spawn_session_events(
        harness,
        session_runtime_events_from_turn_event(&context, event),
        task_id,
    );
}

async fn update_task_checkpoint<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    task_id: Uuid,
    phase: ExecutionPhase,
    worktree: Option<WorktreeSnapshot>,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    if let Err(error) = harness
        .update_session_checkpoint(SessionCheckpointUpdate {
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

async fn transition_task_session<P, SessionRt, TraceRt, StoreRt, McpRt, S, W>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    ctx: &TaskCommandContext<S, W>,
    task_id: Uuid,
    phase: Option<ExecutionPhase>,
    status: SessionStatus,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let _ = harness
        .transition_session(SessionTransition {
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
    pub project_id: Uuid,
    pub routine_id: Option<Uuid>,
    pub assigned_agent_id: Option<Uuid>,
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

/// Handle a task execution command.
///
/// If the project has a synced git repo, creates a worktree for this task
/// and sets the git context on the Task. Cleans up the worktree after execution.
impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    pub async fn handle_task_execute<S, W>(
        &self,
        ctx: &TaskCommandContext<S, W>,
        request: TaskExecuteRequest<'_>,
    ) -> Result<()>
    where
        S: ResponseSender,
        W: TaskWorktreeManager,
    {
        handle_task_execute(self, ctx, request).await
    }

    pub async fn handle_execution_cancel<S, W>(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
        W: TaskWorktreeManager,
    {
        handle_execution_cancel(self, ctx, execution_run_id).await
    }

    pub async fn handle_execution_pause<S, W>(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
        W: TaskWorktreeManager,
    {
        handle_execution_pause(self, ctx, execution_run_id).await
    }

    pub async fn handle_execution_resume<S, W>(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
        W: TaskWorktreeManager,
    {
        handle_execution_resume(self, ctx, execution_run_id).await
    }
}

async fn handle_task_execute<P, SessionRt, TraceRt, StoreRt, McpRt, S, W>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    ctx: &TaskCommandContext<S, W>,
    request: TaskExecuteRequest<'_>,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let TaskExecuteRequest {
        task_id,
        project_id,
        routine_id,
        assigned_agent_id,
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
    let manifest = provider.manifest();
    let pslug = project_slug(manifest, project_id);
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

    let aname = assigned_agent_id.map(|id| agent_name(manifest, id));
    let task_memory_namespace = task_memory_namespace(aname.as_deref(), &pslug);
    let active_session = TaskSessionRecord {
        task_id,
        project_id,
        agent_id: assigned_agent_id,
        memory_namespace: task_memory_namespace.as_deref(),
        execution_run_id,
        trace_ref: None,
        status: SessionStatus::Active,
    };
    upsert_task_session(
        harness,
        &active_session,
        routine_id,
        &pslug,
        aname.as_deref(),
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
    let git_locks = harness.git_locks();
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
                    project_id,
                    agent_id: assigned_agent_id,
                    memory_namespace: task_memory_namespace.as_deref(),
                    execution_run_id,
                    trace_ref: None,
                    status: SessionStatus::Failed,
                };
                upsert_task_session(
                    harness,
                    &failed_session,
                    routine_id,
                    &pslug,
                    aname.as_deref(),
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
        project_id,
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
    let location = git_ctx
        .clone()
        .map(ProjectLocation::from_git)
        .unwrap_or_default();

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
        project_slug: &pslug,
        task_slug,
        cancel: &cancel,
    };

    let result = if let Some(rid) = routine_id {
        execute_routine_task(RoutineTaskExecution {
            shared: execution,
            routine_id: rid,
            task: RoutineRun::task(task.clone())
                .execution_run(execution_run_id)
                .project_location(location.clone()),
        })
        .await
    } else if let Some(aid) = assigned_agent_id {
        execute_direct_task(DirectTaskExecution {
            shared: execution,
            agent_id: aid,
            task: AgentRun::task(task.clone())
                .execution_run(execution_run_id)
                .project_location(location.clone()),
            project_id,
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
            project_id,
            agent_id: assigned_agent_id,
            memory_namespace: task_memory_namespace.as_deref(),
            execution_run_id,
            trace_ref: None,
            status: SessionStatus::Failed,
        };
        upsert_task_session(
            harness,
            &failed_session,
            routine_id,
            &pslug,
            aname.as_deref(),
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
        project_id,
        agent_id: assigned_agent_id,
        memory_namespace: task_memory_namespace.as_deref(),
        execution_run_id,
        trace_ref: None,
        status: final_status,
    };
    upsert_task_session(
        harness,
        &final_session,
        routine_id,
        &pslug,
        aname.as_deref(),
        SessionUpsertMode::Spawn,
    )
    .await;
    send_task_completed(ctx, &eid, &tid, &outcome);
    evict_git_lock(&git_locks, &repo_dir, &git_lock);
    Ok(())
}

/// Cancel all tasks belonging to an execution run.
async fn handle_execution_cancel<P, SessionRt, TraceRt, StoreRt, McpRt, S, W>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    ctx: &TaskCommandContext<S, W>,
    execution_run_id: Uuid,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
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
async fn handle_execution_pause<P, SessionRt, TraceRt, StoreRt, McpRt, S, W>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    ctx: &TaskCommandContext<S, W>,
    execution_run_id: Uuid,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
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
async fn handle_execution_resume<P, SessionRt, TraceRt, StoreRt, McpRt, S, W>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    ctx: &TaskCommandContext<S, W>,
    execution_run_id: Uuid,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
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
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
> {
    harness: &'a Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    command_ctx: &'a TaskCommandContext<S, W>,
    execution_run_id: Uuid,
    task_id: Uuid,
    project_slug: &'a str,
    task_slug: &'a str,
    cancel: &'a CancellationToken,
}

struct RoutineTaskExecution<
    'a,
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
> {
    shared: TaskExecutionShared<'a, P, SessionRt, TraceRt, StoreRt, McpRt, S, W>,
    routine_id: Uuid,
    task: RoutineRun,
}

struct DirectTaskExecution<
    'a,
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
> {
    shared: TaskExecutionShared<'a, P, SessionRt, TraceRt, StoreRt, McpRt, S, W>,
    agent_id: Uuid,
    task: AgentRun,
    project_id: Uuid,
}

async fn execute_routine_task<P, SessionRt, TraceRt, StoreRt, McpRt, S, W>(
    exec: RoutineTaskExecution<'_, P, SessionRt, TraceRt, StoreRt, McpRt, S, W>,
) -> Result<TaskExecutionOutcome>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let TaskExecutionShared {
        harness,
        command_ctx: ctx,
        execution_run_id,
        task_id,
        project_slug,
        task_slug,
        cancel,
    } = exec.shared;
    let RoutineTaskExecution {
        routine_id, task, ..
    } = exec;
    let provider = harness.provider();
    let mut handle = harness
        .provider()
        .routine_by_id(routine_id)?
        .with_session_binding(nenjo::routines::SessionBinding {
            session_id: task_id,
            memory_namespace: harness.session_memory_namespace(task_id).await?,
        })
        .run_stream(task)
        .await?;

    // Accumulate token metrics from step events as they stream through.
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    // Track the current agent_id so step_completed events can carry it.
    let mut current_agent_id: Option<uuid::Uuid> = None;
    let mut step_agents: std::collections::HashMap<uuid::Uuid, (uuid::Uuid, String)> =
        std::collections::HashMap::new();
    let mut trace_recorders: std::collections::HashMap<(uuid::Uuid, uuid::Uuid), TraceRt::Writer> =
        std::collections::HashMap::new();

    loop {
        tokio::select! {
            event = handle.recv() => {
                match event {
                    Some(ev) => {
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
                        if let nenjo::RoutineEvent::AgentEvent { step_id, step_run_id, event } = &ev
                            && let Some((agent_id, step_name)) = step_agents.get(step_run_id)
                        {
                            let agent_name = provider
                                .manifest()
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
                            let recorder = trace_recorders.entry((*agent_id, *step_run_id)).or_insert_with(|| {
                                harness.execution_traces().writer(
                                    ExecutionTraceTarget::Task {
                                        project_slug: project_slug.to_string(),
                                        task_slug: task_slug.to_string(),
                                        step_name: Some(step_name.clone()),
                                        step_id: Some(*step_id),
                                    },
                                    TraceAgent {
                                        id: *agent_id,
                                        name: agent_name.clone(),
                                    },
                                )
                            });
                            recorder.record(event);
                        }
                        if let Some(r) = routine_event_to_response(&ev, execution_run_id, Some(task_id), current_agent_id, harness.provider().manifest()) {
                            let _ = ctx.response_sink.send(r);
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                handle.cancel();
                for recorder in trace_recorders.values() {
                    recorder.finalize_with_error("Cancelled");
                }
                break;
            }
        }
    }
    for (_, recorder) in trace_recorders {
        recorder.finish().await;
    }

    let result = handle.output().await?;
    Ok(if cancel.is_cancelled() {
        TaskExecutionOutcome::failed("Cancelled", total_input_tokens, total_output_tokens)
    } else if result.passed {
        TaskExecutionOutcome::success(total_input_tokens, total_output_tokens)
    } else {
        TaskExecutionOutcome::failed(result.output, total_input_tokens, total_output_tokens)
    })
}

async fn execute_direct_task<P, SessionRt, TraceRt, StoreRt, McpRt, S, W>(
    exec: DirectTaskExecution<'_, P, SessionRt, TraceRt, StoreRt, McpRt, S, W>,
) -> Result<TaskExecutionOutcome>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let TaskExecutionShared {
        harness,
        command_ctx: ctx,
        execution_run_id,
        task_id,
        project_slug,
        task_slug,
        cancel,
    } = exec.shared;
    let DirectTaskExecution {
        agent_id,
        task,
        project_id: task_project_id,
        ..
    } = exec;
    let provider = harness.provider();
    let manifest = provider.manifest().clone();
    let mut builder = provider.build_agent_by_id(agent_id).await?;
    if let Some(project) = manifest
        .projects
        .iter()
        .find(|project| project.id == task_project_id)
    {
        builder = builder.with_project_context(project);
    } else {
        warn!(project_id = %task_project_id, %agent_id, "Project not found in manifest for direct task");
    }
    if let Some(ref location) = task.execution.project_location
        && let Some(ref work_dir) = location.working_dir
    {
        builder = builder.with_work_dir(work_dir);
    }
    let runner = match harness
        .session_memory_namespace(task_id)
        .await?
        .and_then(|namespace| MemoryScope::from_namespace(&namespace))
    {
        Some(scope) => builder.with_memory_scope(scope),
        None => builder,
    }
    .build()
    .await?;
    let aname = agent_name(&manifest, agent_id);
    let trace_target = ExecutionTraceTarget::Task {
        project_slug: project_slug.to_string(),
        task_slug: task_slug.to_string(),
        step_name: None,
        step_id: None,
    };
    let trace_agent = TraceAgent {
        id: agent_id,
        name: aname.clone(),
    };
    let trace_ref = harness
        .execution_traces()
        .trace_ref(&trace_target, &trace_agent);
    let memory_namespace = task_memory_namespace(Some(&aname), project_slug);
    let active_session = TaskSessionRecord {
        task_id,
        project_id: task_project_id,
        agent_id: Some(agent_id),
        memory_namespace: memory_namespace.as_deref(),
        execution_run_id,
        trace_ref,
        status: SessionStatus::Active,
    };
    upsert_task_session(
        harness,
        &active_session,
        None,
        project_slug,
        Some(&aname),
        SessionUpsertMode::Await,
    )
    .await;
    let trace_recorder = harness.execution_traces().writer(trace_target, trace_agent);

    let task_started_at = std::time::Instant::now();
    let mut handle = runner.run_stream(task).await?;

    // Update the registry with the actual pause token from the execution handle
    // so external pause/resume commands reach the turn loop.
    if let Some(mut entry) = harness.executions().get_mut(&task_id) {
        entry.pause = Some(handle.pause_token());
    }

    loop {
        tokio::select! {
            event = handle.recv() => {
                match event {
                    Some(ev) => {
                        trace_recorder.record(&ev);
                        record_task_turn_event(
                            harness,
                            task_id,
                            Some(agent_id),
                            Some(&aname),
                            &ev,
                        );
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
                                    agent_id,
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
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                handle.abort();
                trace_recorder.finalize_with_error("Cancelled");
                break;
            }
        }
    }
    trace_recorder.finish().await;

    let outcome = if !cancel.is_cancelled() {
        let output = handle.output().await?;
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
