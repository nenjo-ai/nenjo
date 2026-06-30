//! Task execution handlers — with git worktree lifecycle.
mod runtime;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::Utc;
use dashmap::mapref::entry::Entry;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    CheckpointQuery, ExecutionPhase, SessionCheckpointUpdate, SessionKind, SessionOwnerKind,
    SessionRefs, SessionRuntimeEvent, SessionStatus, SessionTransition, SessionUpsert,
    TaskSessionUpsert, WorktreeSnapshot,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo::types::GitContext;
use nenjo::{ProjectLocation, Slug, TaskInput};
use nenjo_events::StepAgent;
use serde_json::json;

use nenjo_harness::events::HarnessEvent;
use nenjo_harness::registry::{ActiveExecution, ExecutionKind, ExecutionRegistry};
use nenjo_harness::request::TaskRequest;
use nenjo_harness::session::{TurnEventContext, session_runtime_events_from_turn_event};
use nenjo_harness::{Harness, ProviderRuntime};

use crate::event_bridge::{
    ExecutionAgentTraceContext, ExecutionWorkflowStepEventContext, TaskTurnEventContext,
    agent_name, execution_task_completed_response, execution_workflow_step_response, project_slug,
    routine_event_to_responses, turn_event_to_agent_trace_responses,
    turn_event_to_workflow_step_response,
};
use crate::handlers::ResponseSender;
use crate::handlers::notification::platform_notification_emitter;
use crate::resource_resolver::PlatformResourceResolver;
use crate::runtime::GitLocks;
use crate::tools::{register_platform_notification_emitter, with_platform_notification_emitter};
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

    if !registered_worktree(
        Path::new(&worktree.repo_dir),
        Path::new(&worktree.work_dir),
        &worktree.branch,
    )
    .await
    {
        warn!(
            repo_dir = %worktree.repo_dir,
            work_dir = %worktree.work_dir,
            branch = %worktree.branch,
            "Ignoring task checkpoint with stale or unregistered git worktree"
        );
        return None;
    }

    let repo_url = repo_remote_url(Path::new(&worktree.repo_dir))
        .await
        .unwrap_or_default();

    Some(GitContext {
        branch: worktree.branch,
        target_branch: worktree.target_branch.unwrap_or_else(|| "main".to_string()),
        work_dir: worktree.work_dir,
        repo_url,
    })
}

async fn registered_worktree(repo_dir: &Path, work_dir: &Path, branch: &str) -> bool {
    let output = match tokio::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };

    let listing = String::from_utf8_lossy(&output.stdout);
    let work_dir = work_dir
        .canonicalize()
        .unwrap_or_else(|_| work_dir.to_path_buf());

    worktree_listing_contains(&listing, &work_dir, branch)
}

fn worktree_listing_contains(listing: &str, work_dir: &Path, branch: &str) -> bool {
    let branch_ref = format!("refs/heads/{branch}");

    listing.split("\n\n").any(|entry| {
        let mut entry_worktree = None;
        let mut entry_branch = None;

        for line in entry.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                entry_worktree = Some(Path::new(path));
            } else if let Some(branch) = line.strip_prefix("branch ") {
                entry_branch = Some(branch);
            }
        }

        let Some(entry_worktree) = entry_worktree else {
            return false;
        };

        let entry_worktree = entry_worktree
            .canonicalize()
            .unwrap_or_else(|_| entry_worktree.to_path_buf());

        entry_worktree == work_dir && entry_branch == Some(branch_ref.as_str())
    })
}

async fn repo_remote_url(repo_dir: &Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(repo_dir)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
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

struct RoutineStepSessionRecord<'a> {
    parent_task_id: Uuid,
    step_run_id: Uuid,
    step_slug: &'a str,
    step_name: &'a str,
    project_slug: &'a str,
    routine_slug: Option<&'a str>,
    execution_run_id: Uuid,
    agent_slug: Option<&'a str>,
    agent_name: Option<&'a str>,
    memory_namespace: Option<&'a str>,
}

fn routine_step_session_upsert_event(params: &RoutineStepSessionRecord<'_>) -> SessionRuntimeEvent {
    SessionRuntimeEvent::SessionUpsert(SessionUpsert {
        session_id: params.step_run_id,
        kind: SessionKind::Task,
        status: SessionStatus::Active,
        agent: params.agent_slug.map(ToOwned::to_owned),
        project: Some(params.project_slug.to_string()),
        task_id: Some(params.parent_task_id),
        routine: params.routine_slug.map(ToOwned::to_owned),
        execution_run_id: Some(params.execution_run_id),
        parent_session_id: Some(params.parent_task_id),
        lease: None,
        memory_namespace: params.memory_namespace.map(ToOwned::to_owned),
        refs: SessionRefs {
            memory_namespace: params.memory_namespace.map(ToOwned::to_owned),
            ..Default::default()
        },
        metadata: json!({
            "source": "worker_routine_step",
            "project_slug": params.project_slug,
            "routine_slug": params.routine_slug,
            "parent_task_id": params.parent_task_id,
            "step_slug": params.step_slug,
            "step_run_id": params.step_run_id,
            "step_name": params.step_name,
            "agent_slug": params.agent_slug,
            "agent_name": params.agent_name,
        }),
    })
}

fn record_routine_step_turn_event<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    params: &RoutineStepSessionRecord<'_>,
    agent_id: Option<Uuid>,
    event: &nenjo::TurnEvent,
    include_upsert: bool,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let context = TurnEventContext {
        session_id: params.step_run_id,
        turn_id: None,
        agent_id,
        agent_name: params.agent_name.map(ToOwned::to_owned),
        recorded_at: Utc::now(),
    };
    let mut events = Vec::new();
    if include_upsert {
        events.push(routine_step_session_upsert_event(params));
    }
    events.extend(session_runtime_events_from_turn_event(&context, event));
    harness.sessions().record_events_best_effort(
        params.step_run_id,
        SessionOwnerKind::Task,
        events,
    );
}

fn transition_routine_step_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    step_run_id: Uuid,
    status: SessionStatus,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    harness.sessions().record_events_best_effort(
        step_run_id,
        SessionOwnerKind::Task,
        vec![SessionRuntimeEvent::Transition(SessionTransition {
            session_id: step_run_id,
            worker_id: "harness".to_string(),
            phase: Some(ExecutionPhase::Finalizing),
            status,
        })],
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
    S: ResponseSender + Clone + 'static,
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
    S: ResponseSender + Clone + 'static,
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
    S: ResponseSender + Clone + 'static,
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
        .find(|p| crate::resource_resolver::stable_resource_id("project", &p.slug) == project_id)
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
    let workflow_event_context = ExecutionWorkflowStepEventContext {
        execution_run_id,
        task_id: Some(task_id),
        agent: None,
    };
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
    } else if repo_dir.join(".git").exists() {
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
            let _guard = git_lock.lock().await;
            ctx.worktrees
                .setup_worktree(&repo_dir, execution_run_id, task_slug, target_branch)
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
                remove_active_execution_if_current(&harness.executions(), task_id, registry_token);
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
            task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
        )
        .await;
        remove_active_execution_if_current(&harness.executions(), task_id, registry_token);
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
            task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
        )
        .await;
    }

    // Clean up worktree after execution
    if let Some(ref wt) = git_ctx
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
                .cleanup_worktree(&repo_dir, &wt.work_dir, &wt.branch)
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
    let project_slug = request.project.to_string();
    let routine_slug = request.routine.as_ref().map(ToString::to_string);
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
    let mut step_names: HashMap<uuid::Uuid, String> = HashMap::new();
    let mut step_sessions_upserted: HashSet<uuid::Uuid> = HashSet::new();
    let manifest = harness.provider().manifest_snapshot();

    loop {
        tokio::select! {
            event = stream.recv() => {
                match event {
                    Some(HarnessEvent::Routine { event: ev, .. }) => {
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
                        if let nenjo::RoutineEvent::Done { result, .. } = &ev {
                            routine_passed = result.passed;
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
    let Some(execution_run_id) = Uuid::parse_str(eid).ok() else {
        return;
    };
    let task_id = tid.as_deref().and_then(|value| Uuid::parse_str(value).ok());
    let _ = ctx.response_sink.send(execution_task_completed_response(
        execution_run_id,
        task_id,
        outcome.success,
        outcome.error.clone(),
        None,
        outcome.total_input_tokens,
        outcome.total_output_tokens,
    ));
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

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use super::{
        RoutineStepSessionRecord, remove_active_execution_if_current,
        routine_step_session_upsert_event, worktree_listing_contains,
    };
    use dashmap::DashMap;
    use nenjo_harness::registry::{ActiveExecution, ExecutionKind, ExecutionRegistry};
    use nenjo_sessions::SessionRuntimeEvent;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    #[test]
    fn worktree_listing_contains_registered_branch() {
        let listing = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/worktrees/abcd-task
HEAD 2222222222222222222222222222222222222222
branch refs/heads/agent/abcd/task
";

        assert!(worktree_listing_contains(
            listing,
            Path::new("/repo/worktrees/abcd-task"),
            "agent/abcd/task"
        ));
    }

    #[test]
    fn worktree_listing_rejects_unregistered_or_wrong_branch() {
        let listing = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/worktrees/abcd-task
HEAD 2222222222222222222222222222222222222222
branch refs/heads/agent/abcd/other
";

        assert!(!worktree_listing_contains(
            listing,
            Path::new("/repo/worktrees/abcd-task"),
            "agent/abcd/task"
        ));
        assert!(!worktree_listing_contains(
            listing,
            Path::new("/repo/worktrees/missing"),
            "agent/abcd/other"
        ));
    }

    #[test]
    fn routine_step_session_uses_step_run_id_with_parent_task_metadata() {
        let parent_task_id = Uuid::new_v4();
        let step_run_id = Uuid::new_v4();
        let execution_run_id = Uuid::new_v4();

        let event = routine_step_session_upsert_event(&RoutineStepSessionRecord {
            parent_task_id,
            step_run_id,
            step_slug: "agent_step",
            step_name: "Agent Step",
            project_slug: "demo",
            routine_slug: Some("daily_routine"),
            execution_run_id,
            agent_slug: Some("nenji"),
            agent_name: Some("Nenji"),
            memory_namespace: Some("demo-memory"),
        });

        let SessionRuntimeEvent::SessionUpsert(upsert) = event else {
            panic!("expected routine step session upsert");
        };
        assert_eq!(upsert.session_id, step_run_id);
        assert_eq!(upsert.task_id, Some(parent_task_id));
        assert_eq!(upsert.parent_session_id, Some(parent_task_id));
        assert_eq!(upsert.execution_run_id, Some(execution_run_id));
        assert_eq!(upsert.agent.as_deref(), Some("nenji"));
        assert_eq!(upsert.routine.as_deref(), Some("daily_routine"));
        assert_eq!(
            upsert.metadata["parent_task_id"],
            parent_task_id.to_string()
        );
        assert_eq!(upsert.metadata["step_run_id"], step_run_id.to_string());
        assert_eq!(upsert.metadata["step_slug"], "agent_step");
    }

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
