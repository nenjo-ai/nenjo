//! Task execution handlers — with git worktree lifecycle.
mod attachments;
mod runtime;
mod worktree_state;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use dashmap::mapref::entry::Entry;
use nenjo_sessions::{ExecutionPhase, SessionStatus};
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
    pub(crate) artifacts: Response,
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
        nenjo_harness::TaskExecutionTarget::Agent(agent) => (None, Some(agent.as_str())),
        nenjo_harness::TaskExecutionTarget::Routine(routine) => (Some(routine.as_str()), None),
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
    let routine_id = routine_slug
        .as_ref()
        .map(|slug| resolver.routine_id(slug))
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
    .await;
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
        routine_id = ?routine_id,
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
                .await;
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
        nenjo_harness::TaskExecutionTarget::Routine(_) => {
            let routine = routine_slug
                .clone()
                .ok_or_else(|| anyhow!("routine target did not include a valid slug"))?;
            resolver.routine_id(&routine)?;
            execute_routine_task(RoutineTaskExecution {
                shared: execution,
                request: request.clone().with_routine(routine),
            })
            .await
        }
        nenjo_harness::TaskExecutionTarget::Agent(_) => {
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
        .await;
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
    .await;
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
        let routine_id = routine_slug
            .as_deref()
            .map(Slug::parse)
            .transpose()?
            .as_ref()
            .map(|slug| crate::resource_resolver::stable_resource_id("routine", slug));
        let attachments = build_handoff_attachments(ctx, routine_id, &terminal_handoffs).await?;
        TaskExecutionOutcome::success(total_input_tokens, total_output_tokens)
            .with_attachments(attachments)
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
        artifacts: execution_task_artifacts_response(ExecutionTaskArtifactsResponse {
            execution_run_id,
            task_id: Some(task_id),
            total_input_tokens: outcome.total_input_tokens,
            total_output_tokens: outcome.total_output_tokens,
            attachments: outcome.attachments,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::remove_active_execution_if_current;
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
}
