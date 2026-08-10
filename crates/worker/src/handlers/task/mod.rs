//! Task execution handlers — with git worktree lifecycle.
mod attachments;
mod runtime;
mod worktree_state;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};
use dashmap::mapref::entry::Entry;
use nenjo_sessions::{ExecutionPhase, SessionStatus};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo::{ProjectLocation, Slug, TaskInput};
use nenjo_events::{Response, StepAgent};

use nenjo_harness::events::HarnessEvent;
use nenjo_harness::registry::{ActiveExecution, ExecutionKind, ExecutionRegistry};
use nenjo_harness::request::TaskRequest;
use nenjo_harness::task_session::{
    RoutineStepSessionRecord, SessionUpsertMode, TaskSessionRecord, record_routine_step_turn_event,
    task_memory_namespace, transition_routine_step_session, transition_task_session,
    update_task_checkpoint, upsert_task_session,
};
use nenjo_harness::{Harness, ProviderRuntime, TaskExecutorOutcome};

use crate::event_bridge::{
    ExecutionAgentTraceContext, ExecutionTaskArtifactsResponse, ExecutionWorkflowStepEventContext,
    TaskTurnEventContext, agent_name, execution_task_artifacts_response,
    execution_workflow_step_response, project_slug, routine_event_to_responses,
    turn_event_to_agent_trace_responses, turn_event_to_workflow_step_response,
};
use crate::handlers::ResponseSender;
use crate::handlers::notification::platform_notification_emitter;
use crate::resource_resolver::PlatformResourceResolver;
use crate::tools::{register_platform_notification_emitter, with_platform_notification_emitter};
use attachments::{TaskExecutionOutcome, build_final_output_attachment, build_handoff_attachments};
pub use runtime::{TaskAttachmentEncoder, TaskCommandContext, TaskWorktreeManager};
use worktree_state::{evict_git_lock, restore_task_git_context, task_worktree_snapshot};

fn remove_active_execution_if_current(
    executions: &ExecutionRegistry,
    task_id: Uuid,
    registry_token: Uuid,
) -> Option<ActiveExecution> {
    match executions.entry(task_id) {
        Entry::Occupied(entry) => {
            if entry.get().registry_token == registry_token {
                Some(entry.remove())
            } else {
                None
            }
        }
        Entry::Vacant(_) => None,
    }
}

pub struct TaskExecuteRequest<'a> {
    pub task_id: Uuid,
    pub project: Option<&'a str>,
    pub target: &'a nenjo_harness::TaskExecutionTarget,
    pub execution_run_id: Uuid,
    pub title: &'a str,
    pub instructions: &'a str,
    pub slug: Option<&'a str>,
    pub labels: &'a [String],
    pub status: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub cancellation: CancellationToken,
}

/// Provider-specific terminal data held until the harness has durably
/// transitioned the task execution.
pub(crate) struct TaskExecutionResult {
    pub(crate) outcome: TaskExecutorOutcome,
    pub(crate) artifacts: Option<Response>,
}

/// Worker integration methods for task execution platform commands.
///
/// The worker owns platform task semantics such as response streaming,
/// git-worktree lifecycle, pause/resume/cancel routing, and checkpoint updates.
/// Actual agent/routine execution still goes through the harness/provider.
#[async_trait::async_trait]
pub(crate) trait WorkerTaskHarnessExt<S, W>
where
    S: ResponseSender + Clone + 'static,
    W: TaskWorktreeManager,
{
    /// Execute a task command and stream platform responses.
    async fn handle_task_execute(
        &self,
        ctx: &TaskCommandContext<S, W>,
        request: TaskExecuteRequest<'_>,
    ) -> Result<TaskExecutionResult>;

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

    /// Consume one durable human resolution idempotently.
    async fn handle_execution_continue(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
        request_id: Uuid,
        resolution_revision: u64,
        cancellation: CancellationToken,
    ) -> Result<TaskExecutionResult>;
}

#[async_trait::async_trait]
impl<P, SessionRt, S, W> WorkerTaskHarnessExt<S, W> for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
    W: TaskWorktreeManager,
{
    async fn handle_task_execute(
        &self,
        ctx: &TaskCommandContext<S, W>,
        request: TaskExecuteRequest<'_>,
    ) -> Result<TaskExecutionResult> {
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

    async fn handle_execution_continue(
        &self,
        ctx: &TaskCommandContext<S, W>,
        execution_run_id: Uuid,
        request_id: Uuid,
        resolution_revision: u64,
        cancellation: CancellationToken,
    ) -> Result<TaskExecutionResult> {
        handle_execution_continue(
            self,
            ctx,
            execution_run_id,
            request_id,
            resolution_revision,
            cancellation,
        )
        .await
    }
}

async fn handle_task_execute<P, SessionRt, S, W>(
    harness: &Harness<P, SessionRt>,
    ctx: &TaskCommandContext<S, W>,
    request: TaskExecuteRequest<'_>,
) -> Result<TaskExecutionResult>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
    W: TaskWorktreeManager,
{
    let TaskExecuteRequest {
        task_id,
        project,
        target,
        execution_run_id,
        title,
        instructions,
        slug,
        labels,
        status,
        priority,
        cancellation,
    } = request;
    let (routine, agent) = match target {
        nenjo_harness::TaskExecutionTarget::Agent { slug } => (None, Some(slug.as_str())),
        nenjo_harness::TaskExecutionTarget::Routine { slug } => (Some(slug.as_str()), None),
    };

    // A terminal task session is the durable local receipt for a completed
    // execution run. It survives worker restarts and prevents at-least-once
    // command delivery from invoking the model again.
    if let Some(record) = harness.sessions().get(task_id).await?
        && record.execution_run_id == Some(execution_run_id)
    {
        let replay = match record.status {
            SessionStatus::Completed => Some((
                TaskExecutionOutcome::success(0, 0),
                TaskExecutorOutcome::Completed,
            )),
            SessionStatus::Cancelled => Some((
                TaskExecutionOutcome::failed("Cancelled", 0, 0),
                TaskExecutorOutcome::Cancelled,
            )),
            SessionStatus::Failed => Some((
                TaskExecutionOutcome::failed("Previously failed", 0, 0),
                TaskExecutorOutcome::Failed("Previously failed".to_string()),
            )),
            SessionStatus::Pending
            | SessionStatus::Active
            | SessionStatus::Paused
            | SessionStatus::Waiting => None,
        };
        if let Some((outcome, executor_outcome)) = replay {
            return Ok(task_execution_result(
                execution_run_id,
                task_id,
                outcome,
                executor_outcome,
            ));
        }
    }
    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
    let resolver = PlatformResourceResolver::new(&manifest);
    let project = project.map(Slug::parse).transpose()?;
    let project_id = project
        .as_ref()
        .map(|project| resolver.project_id(project))
        .transpose()?;
    let pslug = project_id
        .map(|project_id| project_slug(&manifest, project_id))
        .unwrap_or_default();
    let agent_slug = agent.map(Slug::parse).transpose()?;
    let routine_slug = routine.map(Slug::parse).transpose()?;
    let assigned_agent_id = agent_slug
        .as_ref()
        .map(|slug| resolver.agent_id(slug))
        .transpose()?;
    let task_slug = slug.unwrap_or("task");
    let repo_dir = project.as_ref().map(|_| ctx.worktrees.repo_dir(&pslug));
    let cancel = cancellation;
    let pause = nenjo::agents::runner::types::PauseToken::new();
    let registry_token = Uuid::new_v4();

    let executions = harness.executions();
    if executions
        .iter()
        .any(|active| active.execution_run_id == Some(execution_run_id))
    {
        warn!(%task_id, %execution_run_id, "Ignoring duplicate active execution run");
        let error = "execution run is already active".to_string();
        return Ok(task_execution_result(
            execution_run_id,
            task_id,
            TaskExecutionOutcome::failed(&error, 0, 0),
            TaskExecutorOutcome::Failed(error),
        ));
    }
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
            let error = "task already has an active execution".to_string();
            return Ok(task_execution_result(
                execution_run_id,
                task_id,
                TaskExecutionOutcome::failed(&error, 0, 0),
                TaskExecutorOutcome::Failed(error),
            ));
        }
        Entry::Vacant(entry) => {
            entry.insert(ActiveExecution {
                kind: ExecutionKind::PreparingTask,
                registry_token,
                execution_run_id: Some(execution_run_id),
                cancel: cancel.clone(),
                pause: Some(pause.clone()),
                turn_input: None,
            });
        }
    }

    // Resolve target branch from project settings.
    let target_branch = manifest
        .projects
        .iter()
        .find(|p| {
            Some(crate::resource_resolver::stable_resource_id(
                "project", &p.slug,
            )) == project_id
        })
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
    .await?;
    update_task_checkpoint(
        harness,
        task_id,
        ExecutionPhase::Preparing,
        task_worktree_snapshot(repo_dir.as_deref(), None),
    )
    .await;

    info!(
        agent = ?aname,
        task_id = %task_id,
        execution_run_id = %execution_run_id,
        project = %pslug,
        title = %title,
        "Task execution started"
    );

    // Set up git worktree if the project has a synced repo.
    // If the repo exists but worktree creation fails, the task fails —
    // we don't run tasks against a dirty or shared working tree.
    let workflow_event_context = ExecutionWorkflowStepEventContext {
        execution_run_id,
        task_id: Some(task_id),
        agent: None,
    };
    // Per-repo mutex — git's .git/config lock doesn't support concurrent writes,
    // so parallel worktree add/remove on the same repo must be serialized.
    let git_locks = ctx.git_locks.clone();
    let git_lock = repo_dir.as_ref().map(|repo_dir| {
        git_locks
            .entry(repo_dir.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    });

    let restored_git_ctx = if repo_dir.is_some() {
        restore_task_git_context(harness, task_id).await
    } else {
        None
    };
    let git_ctx = if let Some(wt) = restored_git_ctx {
        info!(branch = %wt.branch, work_dir = %wt.work_dir, "Restored git worktree from task checkpoint");
        let _ = ctx.response_sink.send(execution_workflow_step_response(
            &workflow_event_context,
            "step_completed",
            "worktree_restore",
            "worktree",
            Some(0),
            serde_json::json!({
                "branch": wt.branch,
                "target_branch": wt.target_branch,
            }),
            Some(serde_json::json!({
                "work_dir": wt.work_dir,
            })),
        ));
        Some(wt)
    } else if let Some(repo_dir) = repo_dir.as_ref()
        && repo_dir.join(".git").exists()
    {
        let _ = ctx.response_sink.send(execution_workflow_step_response(
            &workflow_event_context,
            "step_started",
            "worktree_setup",
            "worktree",
            None,
            serde_json::Value::Null,
            None,
        ));

        let start = std::time::Instant::now();
        let setup_result = {
            let lock = git_lock.as_ref().ok_or_else(|| {
                anyhow!(
                    "project git lock was not initialized for {}",
                    repo_dir.display()
                )
            })?;
            let _guard = lock.lock().await;
            ctx.worktrees
                .setup_worktree(repo_dir, execution_run_id, task_slug, target_branch)
                .await
        };
        match setup_result {
            Ok(wt) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                info!(branch = %wt.branch, work_dir = %wt.work_dir, "Created git worktree for task");

                let _ = ctx.response_sink.send(execution_workflow_step_response(
                    &workflow_event_context,
                    "step_completed",
                    "worktree_setup",
                    "worktree",
                    Some(duration_ms),
                    serde_json::json!({
                        "branch": wt.branch,
                        "target_branch": wt.target_branch,
                    }),
                    Some(serde_json::json!({
                        "work_dir": wt.work_dir,
                    })),
                ));

                Some(wt)
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let error_msg = format!("{e:#}");
                warn!(error = %error_msg, "Worktree setup failed");

                let _ = ctx.response_sink.send(execution_workflow_step_response(
                    &workflow_event_context,
                    "step_failed",
                    "worktree_setup",
                    "worktree",
                    Some(duration_ms),
                    serde_json::json!({ "error": "Worktree setup failed" }),
                    Some(serde_json::json!({ "error": &error_msg })),
                ));

                update_task_checkpoint(
                    harness,
                    task_id,
                    ExecutionPhase::Finalizing,
                    task_worktree_snapshot(Some(repo_dir), None),
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
                .await?;
                remove_active_execution_if_current(&harness.executions(), task_id, registry_token);
                return Ok(task_execution_result(
                    execution_run_id,
                    task_id,
                    TaskExecutionOutcome::failed(&error_msg, 0, 0),
                    TaskExecutorOutcome::Failed(error_msg),
                ));
            }
        }
    } else {
        None
    };

    let task = TaskInput {
        project: project.clone(),
        task_id,
        title: title.to_string(),
        instructions: instructions.to_string(),
        labels: labels.to_vec(),
        status: status.map(ToOwned::to_owned),
        priority: priority.map(ToOwned::to_owned),
        slug: Some(task_slug.to_string()),
    };
    let mut request = TaskRequest::from_task_input(&task).with_execution_run(execution_run_id);
    if let Some(location) = git_ctx.clone().map(ProjectLocation::from_git) {
        request = request.with_project_location(location);
    }

    update_task_checkpoint(
        harness,
        task_id,
        ExecutionPhase::CallingModel,
        task_worktree_snapshot(repo_dir.as_deref(), git_ctx.as_ref()),
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

    let result = match target {
        nenjo_harness::TaskExecutionTarget::Routine { .. } => {
            let routine = routine_slug
                .clone()
                .ok_or_else(|| anyhow!("routine target did not include a valid slug"))?;
            execute_routine_task(RoutineTaskExecution {
                shared: execution,
                request: request.clone().with_routine(routine),
            })
            .await
        }
        nenjo_harness::TaskExecutionTarget::Agent { .. } => {
            let agent = agent_slug
                .clone()
                .ok_or_else(|| anyhow!("agent target did not include a valid slug"))?;
            let aid = resolver.agent_id(&agent)?;
            execute_direct_task(DirectTaskExecution {
                shared: execution,
                agent_id: aid,
                request: request.clone().with_agent(agent),
            })
            .await
        }
    };

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(ref e) => {
            warn!(
                task_id = %task_id,
                execution_run_id = %execution_run_id,
                routine = ?routine_slug,
                agent = ?agent_slug,
                work_dir = ?git_ctx.as_ref().map(|git| git.work_dir.as_str()),
                error = %format!("{e:#}"),
                "Task execution failed before terminal outcome"
            );
            TaskExecutionOutcome::failed(format!("{e:#}"), 0, 0)
        }
    };

    if outcome.waiting_for_human {
        remove_active_execution_if_current(&harness.executions(), task_id, registry_token);
        upsert_task_session(
            harness,
            &TaskSessionRecord {
                task_id,
                memory_namespace: task_memory_namespace.as_deref(),
                execution_run_id,
                status: SessionStatus::Waiting,
            },
            routine_slug.as_ref().map(|slug| slug.as_str()),
            &pslug,
            aname.as_deref(),
            agent_slug.as_ref().map(|slug| slug.as_str()),
            SessionUpsertMode::Spawn,
        )
        .await?;
        return Ok(TaskExecutionResult {
            outcome: TaskExecutorOutcome::WaitingForHuman,
            artifacts: None,
        });
    }

    // If execution itself errored (e.g. routine not found, agent build failure),
    // clean up before telling the platform the task is terminal.
    if !outcome.success {
        update_task_checkpoint(
            harness,
            task_id,
            ExecutionPhase::Finalizing,
            task_worktree_snapshot(repo_dir.as_deref(), git_ctx.as_ref()),
        )
        .await;
        remove_active_execution_if_current(&harness.executions(), task_id, registry_token);
        // Still clean up worktree even on failure.
        if let (Some(wt), Some(repo_dir), Some(git_lock)) =
            (git_ctx.as_ref(), repo_dir.as_ref(), git_lock.as_ref())
        {
            let _guard = git_lock.lock().await;
            if let Err(e) = ctx
                .worktrees
                .cleanup_worktree(repo_dir, &wt.work_dir, &wt.branch)
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
        .await?;
        if let (Some(repo_dir), Some(git_lock)) = (repo_dir.as_ref(), git_lock.as_ref()) {
            evict_git_lock(&git_locks, repo_dir, git_lock);
        }
        let error = outcome
            .error
            .clone()
            .unwrap_or_else(|| "task execution failed".to_string());
        return Ok(task_execution_result(
            execution_run_id,
            task_id,
            outcome,
            TaskExecutorOutcome::Failed(error),
        ));
    }

    // Unregister execution
    remove_active_execution_if_current(&harness.executions(), task_id, registry_token);
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
            task_worktree_snapshot(repo_dir.as_deref(), git_ctx.as_ref()),
        )
        .await;
    }

    // Clean up worktree after execution
    if let (Some(wt), Some(repo_dir), Some(git_lock)) =
        (git_ctx.as_ref(), repo_dir.as_ref(), git_lock.as_ref())
        && final_status != SessionStatus::Cancelled
    {
        let _ = ctx.response_sink.send(execution_workflow_step_response(
            &workflow_event_context,
            "step_started",
            "worktree_cleanup",
            "worktree",
            None,
            serde_json::Value::Null,
            None,
        ));

        let start = std::time::Instant::now();
        let cleanup_result: Result<()> = {
            let _guard = git_lock.lock().await;
            ctx.worktrees
                .cleanup_worktree(repo_dir, &wt.work_dir, &wt.branch)
                .await
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        match &cleanup_result {
            Ok(()) => {
                debug!(branch = %wt.branch, "Cleaned up worktree");
                let _ = ctx.response_sink.send(execution_workflow_step_response(
                    &workflow_event_context,
                    "step_completed",
                    "worktree_cleanup",
                    "worktree",
                    Some(duration_ms),
                    serde_json::json!({ "branch": wt.branch }),
                    None,
                ));
            }
            Err(e) => {
                warn!(error = %e, branch = %wt.branch, "Failed to clean up worktree");
                let _ = ctx.response_sink.send(execution_workflow_step_response(
                    &workflow_event_context,
                    "step_failed",
                    "worktree_cleanup",
                    "worktree",
                    Some(duration_ms),
                    serde_json::json!({ "error": "Worktree cleanup failed" }),
                    Some(serde_json::json!({ "error": e.to_string() })),
                ));
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
    .await?;
    if let (Some(repo_dir), Some(git_lock)) = (repo_dir.as_ref(), git_lock.as_ref()) {
        evict_git_lock(&git_locks, repo_dir, git_lock);
    }
    let executor_outcome = if final_status == SessionStatus::Cancelled {
        TaskExecutorOutcome::Cancelled
    } else {
        TaskExecutorOutcome::Completed
    };
    Ok(task_execution_result(
        execution_run_id,
        task_id,
        outcome,
        executor_outcome,
    ))
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
    S: ResponseSender + Clone + 'static,
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
                &ctx.worker_id,
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
    S: ResponseSender + Clone + 'static,
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
                &ctx.worker_id,
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
    S: ResponseSender + Clone + 'static,
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
                &ctx.worker_id,
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

/// Restore and advance a durable human-capable scheduler checkpoint. This
/// transition is committed locally before any downstream scheduling can be
/// admitted, making broker redelivery a no-op for an already consumed
/// revision.
async fn handle_execution_continue<P, SessionRt, S, W>(
    harness: &Harness<P, SessionRt>,
    ctx: &TaskCommandContext<S, W>,
    execution_run_id: Uuid,
    request_id: Uuid,
    resolution_revision: u64,
    cancellation: CancellationToken,
) -> Result<TaskExecutionResult>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
    W: TaskWorktreeManager,
{
    let request_id = nenjo::routines::human_review::HumanRequestId::new(request_id);
    let wire = ctx
        .platform_api
        .fetch_human_resolution(
            execution_run_id,
            request_id.into_uuid(),
            resolution_revision,
        )
        .await
        .map_err(|error| anyhow!("failed to fetch human resolution: {error}"))?;
    if wire.review_id != request_id.into_uuid()
        || wire.execution_id != execution_run_id
        || u64::try_from(wire.version).ok() != Some(resolution_revision)
    {
        bail!("platform review response identity does not match the continuation");
    }
    let encrypted_checkpoint = match wire.checkpoint_payload_id {
        Some(payload_id) => {
            let payload = ctx
                .platform_api
                .fetch_execution_payload(payload_id)
                .await
                .map_err(|error| anyhow!("failed to fetch checkpoint payload: {error}"))?;
            if payload.execution_id != execution_run_id || payload.kind != "checkpoint" {
                bail!("checkpoint payload identity does not match the continuation");
            }
            payload.encrypted
        }
        None => wire
            .encrypted_checkpoint
            .clone()
            .ok_or_else(|| anyhow!("platform review response is missing its checkpoint payload"))?,
    };
    let encrypted_checkpoint: nenjo_events::EncryptedPayload =
        serde_json::from_value(encrypted_checkpoint)
            .context("platform returned an invalid encrypted checkpoint envelope")?;
    if encrypted_checkpoint.object_id != wire.checkpoint_id {
        bail!("checkpoint envelope identity does not match the continuation");
    }
    let remote_checkpoint = ctx
        .attachment_encoder
        .decrypt_attachment(&encrypted_checkpoint)
        .await
        .context("failed to decrypt the organization routine checkpoint")?;
    let remote_checkpoint: nenjo::routines::human_materialization::RoutineCheckpoint =
        serde_json::from_str(&remote_checkpoint)
            .context("platform routine checkpoint is incompatible")?;

    let session = harness
        .sessions()
        .list()
        .await?
        .into_iter()
        .find(|record| record.execution_run_id == Some(execution_run_id));
    let checkpoint = if let Some(session) = &session {
        let local = harness
            .sessions()
            .latest_checkpoint(session.session_id, Default::default())
            .await?
            .and_then(|checkpoint| checkpoint.opaque_state)
            .and_then(|state| {
                serde_json::from_value::<nenjo_harness::task_session::TaskOpaqueState>(state).ok()
            })
            .map(nenjo_harness::task_session::TaskOpaqueState::into_routine_checkpoint)
            .filter(|local| local.execution_run_id == execution_run_id);
        local.unwrap_or(remote_checkpoint)
    } else {
        remote_checkpoint
    };
    if checkpoint.execution_run_id != execution_run_id {
        bail!("continuation execution does not match the checkpoint");
    }
    let task_id = checkpoint.task_id;
    if session
        .as_ref()
        .is_some_and(|session| session.session_id != task_id)
    {
        bail!("continuation task does not match the local session");
    }
    let already_consumed = checkpoint
        .consumed_resolutions
        .get(&request_id)
        .is_some_and(|revision| *revision == resolution_revision);
    if already_consumed {
        debug!(%execution_run_id, request_id = %request_id.into_uuid(), resolution_revision,
            "Replaying durable suspension publication for consumed continuation");
    }
    let routine_slug = checkpoint.routine_slug.clone();
    let manifest = harness.provider().manifest_snapshot();
    let routine = manifest
        .routines
        .iter()
        .find(|routine| routine.slug == routine_slug)
        .ok_or_else(|| anyhow!("routine not found in worker manifest: {routine_slug}"))?;
    let graph_bytes = serde_json::to_vec(routine)?;
    let current_graph_revision = format!("sha256:{:x}", Sha256::digest(graph_bytes));
    let identity = nenjo::routines::human_scheduler::RoutineCheckpointIdentity::new(
        execution_run_id,
        task_id,
        routine_slug.clone(),
        current_graph_revision,
    );
    let mut scheduler = nenjo::routines::human_scheduler::HumanReviewScheduler::restore(
        routine, checkpoint, &identity,
    )?;
    if !already_consumed {
        let decision = serde_json::from_value(wire.decision)
            .context("platform returned an invalid human decision")?;
        scheduler.apply_resolution(
            nenjo::routines::human_materialization::ResolvedHumanRequest {
                request_id,
                resolution_revision,
                decision,
                resolved_at: wire.resolved_at.to_rfc3339(),
            },
        )?;
    }
    let checkpoint = scheduler.checkpoint();
    if session.is_none() {
        let project_slug = checkpoint
            .input
            .project
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        upsert_task_session(
            harness,
            &TaskSessionRecord {
                task_id,
                memory_namespace: None,
                execution_run_id,
                status: SessionStatus::Waiting,
            },
            Some(routine_slug.as_str()),
            &project_slug,
            None,
            None,
            SessionUpsertMode::Await,
        )
        .await?;
    }
    // Publish the consumed revision as the latest organization checkpoint
    // before downstream work is admitted. Redelivery after local state loss
    // restores this checkpoint instead of applying the decision twice.
    let checkpoint_value = serde_json::to_value(&checkpoint)?;
    let checkpoint_json = serde_json::to_string(&checkpoint_value)?;
    let checkpoint_digest = format!("{:x}", Sha256::digest(checkpoint_json.as_bytes()));
    let checkpoint_id = Uuid::new_v5(
        &execution_run_id,
        format!("routine-checkpoint:{checkpoint_digest}").as_bytes(),
    );
    let encrypted_checkpoint = ctx
        .attachment_encoder
        .encrypt_attachment(checkpoint_id, &checkpoint_json)
        .await?;
    ctx.platform_api
        .put_execution_payload(
            checkpoint_id,
            &nenjo_platform::api_client::PutExecutionPayloadRequest {
                execution_id: execution_run_id,
                kind: nenjo_platform::api_client::ExecutionPayloadKind::Checkpoint,
                encrypted: serde_json::to_value(&encrypted_checkpoint)?,
            },
        )
        .await
        .map_err(|error| anyhow!("failed to store consumed checkpoint payload: {error}"))?;
    ctx.platform_api
        .put_execution_checkpoint(
            checkpoint_id,
            &nenjo_platform::api_client::PutExecutionCheckpointRequest {
                execution_id: execution_run_id,
                contract: checkpoint.contract_version.clone(),
                graph_revision: checkpoint.graph_revision.clone(),
                payload_id: checkpoint_id,
                review_ids: checkpoint
                    .pending_requests
                    .iter()
                    .map(|request_id| request_id.into_uuid())
                    .collect(),
            },
        )
        .await
        .map_err(|error| anyhow!("failed to store consumed checkpoint: {error}"))?;
    // Mirror the same state locally after the authoritative platform commit.
    nenjo_harness::task_session::update_task_routine_checkpoint(harness, task_id, &checkpoint)
        .await?;
    let input = checkpoint.input.clone();
    let request = TaskRequest {
        task_id: input.task_id.unwrap_or(task_id),
        project: input.project.clone(),
        title: input.title.clone(),
        instructions: input.instructions.clone(),
        routine: Some(routine_slug.clone()),
        agent: None,
        execution_run_id: Some(execution_run_id),
        slug: input.slug.clone(),
        labels: input.labels.clone(),
        status: input.status.clone(),
        priority: input.priority.clone(),
        project_location: input.git.clone().map(ProjectLocation::from_git),
    };
    let outcome = execute_resumable_human_routine(ResumableRoutineTaskExecution {
        harness,
        command_ctx: ctx,
        request,
        routine_slug,
        execution_run_id,
        task_id,
        cancel: &cancellation,
        checkpoint: Some(checkpoint),
        resolutions: Vec::new(),
    })
    .await?;
    let status = if outcome.waiting_for_human {
        SessionStatus::Waiting
    } else if outcome.success {
        SessionStatus::Completed
    } else {
        SessionStatus::Failed
    };
    transition_task_session(
        harness,
        &ctx.worker_id,
        task_id,
        Some(if outcome.waiting_for_human {
            ExecutionPhase::Waiting
        } else {
            ExecutionPhase::Finalizing
        }),
        status,
    )
    .await;
    if outcome.waiting_for_human {
        return Ok(TaskExecutionResult {
            outcome: TaskExecutorOutcome::WaitingForHuman,
            artifacts: None,
        });
    }
    let executor_outcome = if cancellation.is_cancelled() {
        TaskExecutorOutcome::Cancelled
    } else if outcome.success {
        TaskExecutorOutcome::Completed
    } else {
        TaskExecutorOutcome::Failed(
            outcome
                .error
                .clone()
                .unwrap_or_else(|| "Routine failed".to_string()),
        )
    };
    Ok(task_execution_result(
        execution_run_id,
        task_id,
        outcome,
        executor_outcome,
    ))
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

struct ResumableRoutineTaskExecution<
    'a,
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
    W: TaskWorktreeManager,
> {
    harness: &'a Harness<P, SessionRt>,
    command_ctx: &'a TaskCommandContext<S, W>,
    request: TaskRequest,
    routine_slug: Slug,
    execution_run_id: Uuid,
    task_id: Uuid,
    cancel: &'a CancellationToken,
    checkpoint: Option<nenjo::routines::human_materialization::RoutineCheckpoint>,
    resolutions: Vec<nenjo::routines::human_materialization::ResolvedHumanRequest>,
}

async fn execute_routine_task<P, SessionRt, S, W>(
    exec: RoutineTaskExecution<'_, P, SessionRt, S, W>,
) -> Result<TaskExecutionOutcome>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
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
    let project_slug = request
        .project
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let routine_slug = request.routine.as_ref().map(ToString::to_string);
    let routine = request
        .routine
        .clone()
        .ok_or_else(|| anyhow!("routine task request did not include a routine slug"))?;
    let manifest = harness.provider().manifest_snapshot();
    let total_steps = manifest
        .routines
        .iter()
        .find(|candidate| candidate.slug == routine)
        .map(|candidate| candidate.steps.len())
        .ok_or_else(|| anyhow!("routine not found in worker manifest: {routine}"))?;
    let human_capable = manifest
        .routines
        .iter()
        .find(|candidate| candidate.slug == routine)
        .is_some_and(|candidate| {
            candidate
                .steps
                .iter()
                .any(|step| step.step_type == nenjo::manifest::RoutineStepType::Human)
        });
    if human_capable {
        return execute_resumable_human_routine(ResumableRoutineTaskExecution {
            harness,
            command_ctx: ctx,
            request,
            routine_slug: routine,
            execution_run_id,
            task_id,
            cancel,
            checkpoint: None,
            resolutions: Vec::new(),
        })
        .await;
    }
    let routine_watch = ctx
        .local_execution_watcher
        .start(execution_run_id, routine, total_steps);
    let step_memory_namespace = harness
        .sessions()
        .memory_namespace(task_id)
        .await
        .ok()
        .flatten();
    let notification_emitter = platform_notification_emitter(ctx.response_sink.clone(), task_id);
    let _notification_registration =
        register_platform_notification_emitter(notification_emitter.clone());
    let mut stream =
        with_platform_notification_emitter(notification_emitter, harness.task_stream(request))
            .await?;

    // Accumulate token metrics from step events as they stream through.
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    // Track the current agent_id so step_completed events can carry it.
    let current_agent_id: Option<uuid::Uuid> = None;
    let mut routine_passed = false;
    let mut terminal_handoffs = Vec::new();
    let mut step_names: HashMap<uuid::Uuid, String> = HashMap::new();
    let mut step_sessions_upserted: HashSet<uuid::Uuid> = HashSet::new();
    loop {
        tokio::select! {
            event = stream.recv() => {
                match event {
                    Some(HarnessEvent::Routine { event: ev, .. }) => {
                        routine_watch.publish(&ev);
                        // Track agent identity across step events.
                        if let nenjo::RoutineEvent::StepStarted { step_run_id, step_name, .. } = &ev {
                            step_names.insert(*step_run_id, step_name.clone());
                        }
                        // Track token totals from completed steps
                        if let nenjo::RoutineEvent::StepCompleted { step_run_id, result, .. } = &ev {
                            total_input_tokens += result.input_tokens;
                            total_output_tokens += result.output_tokens;
                            if step_sessions_upserted.contains(step_run_id) {
                                transition_routine_step_session(
                                    harness,
                                    *step_run_id,
                                    SessionStatus::Completed,
                                );
                            }
                        }
                        if let nenjo::RoutineEvent::StepFailed { step_run_id, .. } = &ev
                            && step_sessions_upserted.contains(step_run_id) {
                                transition_routine_step_session(
                                    harness,
                                    *step_run_id,
                                    SessionStatus::Failed,
                                );
                            }
                        if let nenjo::RoutineEvent::Done { result, handoffs, .. } = &ev {
                            routine_passed = result.passed;
                            terminal_handoffs.clone_from(handoffs);
                        }
                        if let nenjo::RoutineEvent::AgentEvent { step_slug, step_run_id, event } = &ev
                            && let Some(step_name) = step_names.get(step_run_id)
                        {
                            let routine_step = routine_slug.as_deref().and_then(|routine_slug| {
                                manifest
                                    .routines
                                    .iter()
                                    .find(|routine| routine.slug.as_str() == routine_slug)
                                    .and_then(|routine| {
                                        routine.steps.iter().find(|step| step.slug == *step_slug)
                                    })
                            });
                            let agent_slug = routine_step.and_then(|step| step.agent.as_ref());
                            let agent_name = agent_slug.and_then(|agent_slug| {
                                manifest
                                    .agents
                                    .iter()
                                    .find(|agent| agent.slug == *agent_slug)
                                    .map(|agent| agent.name.as_str())
                            });
                            let agent_id = agent_slug
                                .map(|agent_slug| {
                                    crate::resource_resolver::stable_resource_id("agent", agent_slug)
                                });
                            let include_upsert = step_sessions_upserted.insert(*step_run_id);
                            record_routine_step_turn_event(
                                harness,
                                &RoutineStepSessionRecord {
                                    parent_task_id: task_id,
                                    step_run_id: *step_run_id,
                                    step_slug: step_slug.as_str(),
                                    step_name,
                                    project_slug: &project_slug,
                                    routine_slug: routine_slug.as_deref(),
                                    execution_run_id,
                                    agent_slug: agent_slug.map(|slug| slug.as_str()),
                                    agent_name,
                                    memory_namespace: step_memory_namespace.as_deref(),
                                },
                                agent_id,
                                event,
                                include_upsert,
                            );
                        }
                        for response in routine_event_to_responses(
                            &ev,
                            execution_run_id,
                            Some(task_id),
                            current_agent_id,
                            &harness.provider().manifest_snapshot(),
                        ) {
                            if let Err(error) = ctx.response_sink.send(response) {
                                warn!(
                                    %execution_run_id,
                                    %task_id,
                                    error = %error,
                                    "Failed to queue routine worker response"
                                );
                            }
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
        let attachments = build_handoff_attachments(ctx, &terminal_handoffs).await?;
        TaskExecutionOutcome::success(total_input_tokens, total_output_tokens)
            .with_attachments(attachments)
    } else {
        TaskExecutionOutcome::failed(output.text, total_input_tokens, total_output_tokens)
    })
}

async fn execute_resumable_human_routine<P, SessionRt, S, W>(
    execution: ResumableRoutineTaskExecution<'_, P, SessionRt, S, W>,
) -> Result<TaskExecutionOutcome>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
    W: TaskWorktreeManager,
{
    use nenjo::routines::human_materialization::{
        HumanMaterializationContext, RoutineExecutionOutcome, ValidatedHumanRequestDraft,
    };
    let ResumableRoutineTaskExecution {
        harness,
        command_ctx: ctx,
        request,
        routine_slug,
        execution_run_id,
        task_id,
        cancel,
        checkpoint,
        resolutions,
    } = execution;

    let manifest = harness.provider().manifest_snapshot();
    let routine_manifest = manifest
        .routines
        .iter()
        .find(|routine| routine.slug == routine_slug)
        .ok_or_else(|| anyhow!("routine not found in worker manifest: {routine_slug}"))?;
    let graph_bytes = serde_json::to_vec(routine_manifest)?;
    let graph_revision = format!("sha256:{:x}", Sha256::digest(graph_bytes));
    let task_input = TaskInput {
        project: request.project.clone(),
        task_id,
        title: request.title.clone(),
        instructions: request.instructions.clone(),
        labels: request.labels.clone(),
        status: request.status.clone(),
        priority: request.priority.clone(),
        slug: request.slug.clone(),
    };
    let mut run = nenjo::RoutineRun::task(task_input).execution_run(execution_run_id);
    if let Some(location) = request.project_location.clone() {
        run = run.project_location(location);
    }
    let identity = nenjo::routines::human_scheduler::RoutineCheckpointIdentity::new(
        execution_run_id,
        task_id,
        routine_slug.clone(),
        graph_revision,
    );
    let mut stream = harness
        .provider()
        .routine(&routine_slug)?
        .run_resumable_stream(run, identity, checkpoint, resolutions)
        .await?;
    let mut total_input_tokens = 0;
    let mut total_output_tokens = 0;
    while let Some(event) = tokio::select! {
        event = stream.recv() => event,
        _ = cancel.cancelled() => { stream.cancel(); None },
    } {
        if let nenjo::RoutineEvent::StepCompleted { result, .. } = &event {
            total_input_tokens += result.input_tokens;
            total_output_tokens += result.output_tokens;
        }
        for response in
            routine_event_to_responses(&event, execution_run_id, Some(task_id), None, &manifest)
        {
            let _ = ctx.response_sink.send(response);
        }
    }
    match stream.output().await? {
        RoutineExecutionOutcome::Completed(result) if result.passed => Ok(
            TaskExecutionOutcome::success(total_input_tokens, total_output_tokens),
        ),
        RoutineExecutionOutcome::Completed(result) => Ok(TaskExecutionOutcome::failed(
            result.output,
            total_input_tokens,
            total_output_tokens,
        )),
        RoutineExecutionOutcome::Failed(failure) => Ok(TaskExecutionOutcome::failed(
            failure.summary,
            total_input_tokens,
            total_output_tokens,
        )),
        RoutineExecutionOutcome::Suspended {
            mut checkpoint,
            drafts,
            ..
        } => {
            let draft_request_ids = drafts
                .iter()
                .map(|draft| draft.request_id)
                .collect::<HashSet<_>>();
            let mut opened = Vec::new();
            for draft in drafts {
                let context = HumanMaterializationContext {
                    execution_run_id,
                    step_slug: draft.step_slug.clone(),
                    request_round: draft.round,
                    task_title: request.title.clone(),
                };
                let materialized =
                    ValidatedHumanRequestDraft::new(draft.spec.clone(), draft.inputs.clone())
                        .prepare(&context)?;
                opened.push((draft, materialized));
            }
            // Keep unpublished drafts only in the worker-local checkpoint so
            // a failed event send can be retried without rerunning agents.
            nenjo_harness::task_session::update_task_routine_checkpoint(
                harness,
                task_id,
                &checkpoint,
            )
            .await?;
            // The platform copy is a continuation checkpoint, not an upload
            // outbox. Workspace-bearing drafts never leave the worker even
            // inside the encrypted envelope.
            let mut platform_checkpoint = checkpoint.clone();
            platform_checkpoint.pending_drafts.clear();
            let checkpoint_value = serde_json::to_value(&platform_checkpoint)?;
            let checkpoint_json = serde_json::to_string(&checkpoint_value)?;
            let checkpoint_digest = format!("{:x}", Sha256::digest(checkpoint_json.as_bytes()));
            let checkpoint_id = Uuid::new_v5(
                &execution_run_id,
                format!("routine-checkpoint:{checkpoint_digest}").as_bytes(),
            );
            let encrypted_checkpoint = ctx
                .attachment_encoder
                .encrypt_attachment(checkpoint_id, &checkpoint_json)
                .await?;
            ctx.platform_api
                .put_execution_payload(
                    checkpoint_id,
                    &nenjo_platform::api_client::PutExecutionPayloadRequest {
                        execution_id: execution_run_id,
                        kind: nenjo_platform::api_client::ExecutionPayloadKind::Checkpoint,
                        encrypted: serde_json::to_value(&encrypted_checkpoint)?,
                    },
                )
                .await
                .map_err(|error| anyhow!("failed to store routine checkpoint payload: {error}"))?;
            let existing_pending = checkpoint
                .pending_requests
                .iter()
                .filter(|request_id| !draft_request_ids.contains(request_id))
                .map(|request_id| request_id.into_uuid())
                .collect::<Vec<_>>();
            ctx.platform_api
                .put_execution_checkpoint(
                    checkpoint_id,
                    &nenjo_platform::api_client::PutExecutionCheckpointRequest {
                        execution_id: execution_run_id,
                        contract: checkpoint.contract_version.clone(),
                        graph_revision: checkpoint.graph_revision.clone(),
                        payload_id: checkpoint_id,
                        review_ids: existing_pending,
                    },
                )
                .await
                .map_err(|error| anyhow!("failed to store routine checkpoint: {error}"))?;
            for (draft, materialized) in opened {
                let encrypted_inputs = ctx
                    .attachment_encoder
                    .encrypt_attachment(
                        materialized.request_id.into_uuid(),
                        &serde_json::to_string(&materialized.inputs)?,
                    )
                    .await?;
                let input_payload_id = materialized.request_id.into_uuid();
                ctx.platform_api
                    .put_execution_payload(
                        input_payload_id,
                        &nenjo_platform::api_client::PutExecutionPayloadRequest {
                            execution_id: execution_run_id,
                            kind: nenjo_platform::api_client::ExecutionPayloadKind::ReviewInputs,
                            encrypted: serde_json::to_value(&encrypted_inputs)?,
                        },
                    )
                    .await
                    .map_err(|error| anyhow!("failed to store review inputs: {error}"))?;
                let artifact_ids =
                    nenjo::routines::handoff_schema::artifact_ids_in_inputs(&materialized.inputs)?;
                let form = materialized_review_form(
                    draft.spec.approval_schema,
                    &materialized.option_snapshot,
                )?;
                ctx.platform_api
                    .put_review(
                        materialized.request_id.into_uuid(),
                        &nenjo_platform::api_client::PutReviewRequest {
                            execution_id: execution_run_id,
                            task_id,
                            step: draft.step_slug.to_string(),
                            round: draft.round,
                            title: materialized.title,
                            inputs: nenjo_platform::api_client::ReviewInputsReference {
                                blob_id: input_payload_id,
                                schemas: materialized
                                    .inputs
                                    .iter()
                                    .map(|input| input.schema.clone())
                                    .collect(),
                            },
                            form,
                            checkpoint_id,
                            artifact_ids,
                            wait_for_review: checkpoint.ready.is_empty()
                                && checkpoint.running.is_empty(),
                        },
                    )
                    .await
                    .map_err(|error| anyhow!("failed to create review: {error}"))?;
            }
            for request_id in draft_request_ids {
                checkpoint.pending_drafts.remove(&request_id);
            }
            nenjo_harness::task_session::update_task_routine_checkpoint(
                harness,
                task_id,
                &checkpoint,
            )
            .await?;
            Ok(TaskExecutionOutcome::waiting(
                total_input_tokens,
                total_output_tokens,
            ))
        }
    }
}

fn materialized_review_form(
    schema: Option<nenjo::routines::human_review::ApprovalSchema>,
    snapshot: &nenjo::routines::human_review::ApprovalOptionSnapshot,
) -> Result<Option<serde_json::Value>> {
    let Some(schema) = schema else {
        return Ok(None);
    };
    let fields = schema
        .fields
        .into_iter()
        .map(|field| {
            let options = snapshot
                .fields
                .get(&field.id)
                .cloned()
                .or(match field.options {
                    nenjo::routines::human_review::ApprovalOptions::Static { values } => {
                        Some(values)
                    }
                    nenjo::routines::human_review::ApprovalOptions::Inputs { .. } => None,
                })
                .ok_or_else(|| {
                    anyhow!(
                        "review form field '{}' is missing its materialized options",
                        field.id
                    )
                })?;
            Ok(serde_json::json!({
                "id": field.id,
                "label": field.label,
                "type": field.field_type,
                "required": field.required,
                "min_items": field.min_items,
                "max_items": field.max_items,
                "options": options,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(serde_json::json!({ "fields": fields })))
}

async fn execute_direct_task<P, SessionRt, S, W>(
    exec: DirectTaskExecution<'_, P, SessionRt, S, W>,
) -> Result<TaskExecutionOutcome>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
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
    let notification_emitter = platform_notification_emitter(ctx.response_sink.clone(), task_id);
    let _notification_registration =
        register_platform_notification_emitter(notification_emitter.clone());
    let mut stream =
        with_platform_notification_emitter(notification_emitter, harness.task_stream(request))
            .await?;

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
                        for response in turn_event_to_agent_trace_responses(
                            &ev,
                            &ExecutionAgentTraceContext {
                                execution_run_id,
                                task_id: Some(task_id),
                                agent_name: aname.clone(),
                                trace_run_id: execution_run_id.to_string(),
                                trace_session_id: execution_run_id,
                                routine_step: None,
                            },
                        ) {
                            let _ = ctx.response_sink.send(response);
                        }
                        if matches!(ev, nenjo::TurnEvent::Done { .. }) {
                            let response = turn_event_to_workflow_step_response(
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
                                            .find(|a| crate::resource_resolver::stable_resource_id("agent", &a.slug) == agent_id)
                                            .and_then(|a| a.color.clone()),
                                    }),
                                    routine_step: None,
                                    agent_duration_ms,
                                    emit_done: true,
                                    summarize_outputs: false,
                                },
                            );
                            if let Some(response) = response {
                                let _ = ctx.response_sink.send(response);
                            }
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
        let attachments = build_final_output_attachment(ctx, &output.text).await?;
        TaskExecutionOutcome::success(output.input_tokens, output.output_tokens)
            .with_attachments(attachments)
    } else {
        TaskExecutionOutcome::failed("Cancelled", 0, 0)
    };
    Ok(outcome)
}

fn task_execution_result(
    execution_run_id: Uuid,
    task_id: Uuid,
    outcome: TaskExecutionOutcome,
    executor_outcome: TaskExecutorOutcome,
) -> TaskExecutionResult {
    TaskExecutionResult {
        outcome: executor_outcome,
        artifacts: Some(execution_task_artifacts_response(
            ExecutionTaskArtifactsResponse {
                execution_run_id,
                task_id: Some(task_id),
                total_input_tokens: outcome.total_input_tokens,
                total_output_tokens: outcome.total_output_tokens,
                attachments: outcome.attachments,
            },
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{materialized_review_form, remove_active_execution_if_current};
    use dashmap::DashMap;
    use nenjo_harness::registry::{ActiveExecution, ExecutionKind, ExecutionRegistry};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    #[test]
    fn active_execution_remove_requires_current_registry_token() {
        let executions: ExecutionRegistry = Arc::new(DashMap::new());
        let task_id = Uuid::new_v4();
        let execution_run_id = Uuid::new_v4();
        let current_token = Uuid::new_v4();
        let stale_token = Uuid::new_v4();

        executions.insert(
            task_id,
            ActiveExecution {
                kind: ExecutionKind::Task,
                registry_token: current_token,
                execution_run_id: Some(execution_run_id),
                cancel: CancellationToken::new(),
                pause: None,
                turn_input: None,
            },
        );

        assert!(
            remove_active_execution_if_current(&executions, task_id, stale_token).is_none(),
            "stale token must not remove an active execution"
        );
        assert!(executions.contains_key(&task_id));

        let removed = remove_active_execution_if_current(&executions, task_id, current_token)
            .expect("current token should remove active execution");
        assert_eq!(removed.registry_token, current_token);
        assert!(!executions.contains_key(&task_id));
    }

    #[test]
    fn review_form_contains_only_materialized_options() {
        let schema = serde_json::from_value(serde_json::json!({
            "fields": [{
                "id": "component",
                "label": "Component",
                "type": "single_select",
                "required": true,
                "options": {
                    "type": "inputs",
                    "inputs": [{
                        "input": "draft",
                        "pointer": "/components",
                        "value": "/id",
                        "label": "/name"
                    }]
                }
            }]
        }))
        .unwrap();
        let snapshot = serde_json::from_value(serde_json::json!({
            "fields": {
                "component": [{ "value": "api", "label": "API" }]
            }
        }))
        .unwrap();

        let form = materialized_review_form(Some(schema), &snapshot)
            .unwrap()
            .unwrap();

        assert_eq!(
            form["fields"][0]["options"],
            serde_json::json!([{ "value": "api", "label": "API" }])
        );
        assert!(form["fields"][0]["options"].get("type").is_none());
    }
}
