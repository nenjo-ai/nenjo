//! Task execution handlers — with git worktree lifecycle.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    ExecutionPhase, SessionCheckpoint, SessionKind, SessionRecord, SessionRefs, SessionStatus,
    SessionSummary, WorktreeSnapshot,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo::types::GitContext;
use nenjo_events::{Response, StepAgent};

use super::event_bridge::{agent_name, project_slug, routine_event_to_response};
use crate::harness::execution_trace::{ExecutionTraceRecorder, TaskTraceLocation};
use crate::harness::session::{
    apply_session_memory_scope, lease_for_status, read_json_blob, session_memory_namespace,
    transition_session_state, update_checkpoint_with_worktree,
};
use crate::harness::{ActiveExecution, CommandContext};

fn task_trace_ref(project_slug: &str, task_slug: &str, agent_name: &str, agent_id: Uuid) -> String {
    format!(
        "{project_slug}/execution_traces/{task_slug}/{}_{}.json",
        agent_name, agent_id
    )
}

fn task_checkpoint_ref(project_slug: &str, task_slug: &str, task_id: Uuid) -> String {
    format!("{project_slug}/task_checkpoints/{task_slug}_{task_id}.json")
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

fn restore_task_git_context(ctx: &CommandContext, task_id: Uuid) -> Option<GitContext> {
    let record = ctx.session_store.get(task_id).ok().flatten()?;
    let checkpoint_ref = record.refs.checkpoint_ref?;
    let checkpoint: SessionCheckpoint = read_json_blob(&*ctx.session_content, &checkpoint_ref)
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

fn upsert_task_session(
    ctx: &CommandContext,
    task_id: Uuid,
    project_id: Uuid,
    agent_id: Option<Uuid>,
    memory_namespace: Option<&str>,
    execution_run_id: Uuid,
    trace_ref: Option<String>,
    checkpoint_ref: Option<String>,
    status: SessionStatus,
) {
    let now = Utc::now();
    let mut record = ctx
        .session_store
        .get(task_id)
        .ok()
        .flatten()
        .unwrap_or(SessionRecord {
            session_id: task_id,
            kind: SessionKind::Task,
            status,
            project_id: Some(project_id),
            agent_id,
            task_id: Some(task_id),
            routine_id: None,
            execution_run_id: Some(execution_run_id),
            parent_session_id: None,
            version: 0,
            refs: SessionRefs::default(),
            lease: Default::default(),
            scheduler: None,
            domain: None,
            summary: SessionSummary::default(),
            created_at: now,
            updated_at: now,
            completed_at: None,
        });

    record.kind = SessionKind::Task;
    record.status = status;
    record.project_id = Some(project_id);
    record.agent_id = agent_id;
    record.task_id = Some(task_id);
    record.execution_run_id = Some(execution_run_id);
    record.version += 1;
    record.updated_at = now;
    if let Some(trace_ref) = trace_ref {
        record.refs.trace_ref = Some(trace_ref);
    }
    if let Some(checkpoint_ref) = checkpoint_ref {
        record.refs.checkpoint_ref = Some(checkpoint_ref);
    }
    if let Some(memory_namespace) = memory_namespace {
        record.refs.memory_namespace = Some(memory_namespace.to_string());
    }
    if matches!(
        status,
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
    ) {
        record.completed_at = Some(now);
    }
    record.lease = lease_for_status(
        &*ctx.session_coordinator,
        task_id,
        &ctx.worker_id,
        status,
        &record.lease,
    );

    let _ = ctx.session_store.put(&record);
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

/// Handle a task execution command.
///
/// If the project has a synced git repo, creates a worktree for this task
/// and sets the git context on the Task. Cleans up the worktree after execution.
#[allow(clippy::too_many_arguments)]
pub async fn handle_task_execute(
    ctx: &CommandContext,
    task_id: Uuid,
    project_id: Uuid,
    routine_id: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    execution_run_id: Uuid,
    title: &str,
    description: &str,
    slug: Option<&str>,
    acceptance_criteria: Option<&str>,
    tags: &[String],
    status: Option<&str>,
    priority: Option<&str>,
    task_type: Option<&str>,
    complexity: Option<&str>,
) -> Result<()> {
    let provider = ctx.provider();
    let manifest = provider.manifest();
    let pslug = project_slug(manifest, project_id);
    let task_slug = slug.unwrap_or("task");
    let repo_dir = ctx.config.workspace_dir.join(&pslug).join("repo");
    let checkpoint_ref = task_checkpoint_ref(&pslug, task_slug, task_id);

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
    upsert_task_session(
        ctx,
        task_id,
        project_id,
        assigned_agent_id,
        task_memory_namespace.as_deref(),
        execution_run_id,
        None,
        Some(checkpoint_ref.clone()),
        SessionStatus::Active,
    );
    let _ = update_checkpoint_with_worktree(
        &*ctx.session_store,
        &*ctx.session_content,
        task_id,
        ExecutionPhase::Preparing,
        task_worktree_snapshot(Some(&repo_dir), None),
    );

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
    let git_lock = ctx
        .git_locks
        .entry(repo_dir.clone())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone();

    let restored_git_ctx = restore_task_git_context(ctx, task_id);
    let git_ctx = if let Some(wt) = restored_git_ctx {
        info!(branch = %wt.branch, work_dir = %wt.work_dir, "Restored git worktree from task checkpoint");
        let _ = ctx.response_tx.send(Response::TaskStepEvent {
            execution_run_id: eid.clone(),
            task_id: tid.clone(),
            event_type: "step_completed".to_string(),
            step_name: "worktree_restore".to_string(),
            step_type: "worktree".to_string(),
            duration_ms: Some(0),
            data: serde_json::json!({
                "branch": wt.branch,
                "target_branch": wt.target_branch,
                "work_dir": wt.work_dir,
            }),
            agent: None,
        });
        Some(wt)
    } else if repo_dir.join(".git").exists() {
        let _ = ctx.response_tx.send(Response::TaskStepEvent {
            execution_run_id: eid.clone(),
            task_id: tid.clone(),
            event_type: "step_started".to_string(),
            step_name: "worktree_setup".to_string(),
            step_type: "worktree".to_string(),
            duration_ms: None,
            data: serde_json::Value::Null,
            agent: None,
        });

        let start = std::time::Instant::now();
        let setup_result = {
            let _guard = git_lock.lock().await;
            setup_worktree(&repo_dir, execution_run_id, task_slug, target_branch).await
        };
        match setup_result {
            Ok(wt) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                info!(branch = %wt.branch, work_dir = %wt.work_dir, "Created git worktree for task");

                let _ = ctx.response_tx.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_completed".to_string(),
                    step_name: "worktree_setup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({
                        "branch": wt.branch,
                        "target_branch": wt.target_branch,
                        "work_dir": wt.work_dir,
                    }),
                    agent: None,
                });

                Some(wt)
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let error_msg = format!("{e:#}");
                warn!(error = %error_msg, "Worktree setup failed");

                let _ = ctx.response_tx.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_failed".to_string(),
                    step_name: "worktree_setup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({ "error": &error_msg }),
                    agent: None,
                });

                send_task_failed(ctx, &eid, &tid, &error_msg);
                let _ = update_checkpoint_with_worktree(
                    &*ctx.session_store,
                    &*ctx.session_content,
                    task_id,
                    ExecutionPhase::Finalizing,
                    task_worktree_snapshot(Some(&repo_dir), None),
                );
                upsert_task_session(
                    ctx,
                    task_id,
                    project_id,
                    assigned_agent_id,
                    task_memory_namespace.as_deref(),
                    execution_run_id,
                    None,
                    Some(checkpoint_ref.clone()),
                    SessionStatus::Failed,
                );
                return Ok(());
            }
        }
    } else {
        None
    };

    let task = nenjo::types::TaskType::Task(nenjo::types::Task {
        task_id,
        title: title.to_string(),
        description: description.to_string(),
        acceptance_criteria: acceptance_criteria.map(|s| s.to_string()),
        tags: tags.to_vec(),
        source: "task".to_string(),
        project_id,
        status: status.unwrap_or("").to_string(),
        priority: priority.unwrap_or("").to_string(),
        task_type: task_type.unwrap_or("").to_string(),
        slug: task_slug.to_string(),
        complexity: complexity.unwrap_or("").to_string(),
        git: git_ctx.clone(),
    });

    let cancel = CancellationToken::new();
    let pause = nenjo::agents::runner::types::PauseToken::new();
    ctx.executions.insert(
        task_id,
        ActiveExecution {
            kind: crate::harness::ExecutionKind::Task,
            registry_token: Uuid::new_v4(),
            execution_run_id: Some(execution_run_id),
            cancel: cancel.clone(),
            pause: Some(pause.clone()),
        },
    );
    let _ = update_checkpoint_with_worktree(
        &*ctx.session_store,
        &*ctx.session_content,
        task_id,
        ExecutionPhase::CallingModel,
        task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
    );

    let result = if let Some(rid) = routine_id {
        execute_routine_task(ctx, rid, task, execution_run_id, task_id, &cancel).await
    } else if let Some(aid) = assigned_agent_id {
        execute_direct_task(ctx, aid, task, execution_run_id, task_id, &cancel).await
    } else {
        warn!("TaskExecute without routine_id or assigned_agent_id");
        send_task_failed(ctx, &eid, &tid, "No routine_id or assigned_agent_id");
        Ok(())
    };

    // If execution itself errored (e.g. routine not found, agent build failure),
    // send TaskCompleted failed and clean up.
    if let Err(ref e) = result {
        let error_msg = format!("{e:#}");
        send_task_failed(ctx, &eid, &tid, &error_msg);
        upsert_task_session(
            ctx,
            task_id,
            project_id,
            assigned_agent_id,
            task_memory_namespace.as_deref(),
            execution_run_id,
            None,
            Some(checkpoint_ref.clone()),
            SessionStatus::Failed,
        );
        let _ = update_checkpoint_with_worktree(
            &*ctx.session_store,
            &*ctx.session_content,
            task_id,
            ExecutionPhase::Finalizing,
            task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
        );
        ctx.executions.remove(&task_id);
        // Still clean up worktree even on failure.
        if let Some(ref wt) = git_ctx {
            let _guard = git_lock.lock().await;
            if let Err(e) = cleanup_worktree(&repo_dir, &wt.work_dir, &wt.branch).await {
                warn!(error = %e, branch = %wt.branch, "Failed to clean up worktree");
            }
        }
        evict_git_lock(&ctx.git_locks, &repo_dir, &git_lock);
        return Ok(());
    }

    // Unregister execution
    ctx.executions.remove(&task_id);
    let final_status = if cancel.is_cancelled() {
        SessionStatus::Cancelled
    } else {
        SessionStatus::Completed
    };
    upsert_task_session(
        ctx,
        task_id,
        project_id,
        assigned_agent_id,
        task_memory_namespace.as_deref(),
        execution_run_id,
        None,
        Some(checkpoint_ref.clone()),
        final_status,
    );
    if final_status != SessionStatus::Cancelled {
        let _ = update_checkpoint_with_worktree(
            &*ctx.session_store,
            &*ctx.session_content,
            task_id,
            ExecutionPhase::Finalizing,
            task_worktree_snapshot(Some(&repo_dir), git_ctx.as_ref()),
        );
    }

    // Clean up worktree after execution
    if let Some(ref wt) = git_ctx
        && final_status != SessionStatus::Cancelled
    {
        let _ = ctx.response_tx.send(Response::TaskStepEvent {
            execution_run_id: eid.clone(),
            task_id: tid.clone(),
            event_type: "step_started".to_string(),
            step_name: "worktree_cleanup".to_string(),
            step_type: "worktree".to_string(),
            duration_ms: None,
            data: serde_json::Value::Null,
            agent: None,
        });

        let start = std::time::Instant::now();
        let cleanup_result = {
            let _guard = git_lock.lock().await;
            cleanup_worktree(&repo_dir, &wt.work_dir, &wt.branch).await
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        match &cleanup_result {
            Ok(()) => {
                debug!(branch = %wt.branch, "Cleaned up worktree");
                let _ = ctx.response_tx.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_completed".to_string(),
                    step_name: "worktree_cleanup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({ "branch": wt.branch }),
                    agent: None,
                });
            }
            Err(e) => {
                warn!(error = %e, branch = %wt.branch, "Failed to clean up worktree");
                let _ = ctx.response_tx.send(Response::TaskStepEvent {
                    execution_run_id: eid.clone(),
                    task_id: tid.clone(),
                    event_type: "step_failed".to_string(),
                    step_name: "worktree_cleanup".to_string(),
                    step_type: "worktree".to_string(),
                    duration_ms: Some(duration_ms),
                    data: serde_json::json!({ "error": e.to_string() }),
                    agent: None,
                });
            }
        }
    }

    evict_git_lock(&ctx.git_locks, &repo_dir, &git_lock);
    Ok(())
}

/// Cancel all tasks belonging to an execution run.
pub async fn handle_execution_cancel(ctx: &CommandContext, execution_run_id: Uuid) -> Result<()> {
    let mut cancelled = 0u32;
    // Collect keys first to avoid holding DashMap ref during remove.
    let keys: Vec<Uuid> = ctx
        .executions
        .iter()
        .filter(|e| e.execution_run_id == Some(execution_run_id))
        .map(|e| *e.key())
        .collect();
    for key in keys {
        if let Some((_, exec)) = ctx.executions.remove(&key) {
            exec.cancel.cancel();
            let _ = transition_session_state(
                &*ctx.session_store,
                &*ctx.session_content,
                &*ctx.session_coordinator,
                key,
                &ctx.worker_id,
                Some(ExecutionPhase::Waiting),
                SessionStatus::Cancelled,
            );
            cancelled += 1;
        }
    }
    if cancelled > 0 {
        info!(%execution_run_id, cancelled, "Cancelled active task executions");
    }
    Ok(())
}

/// Pause all tasks belonging to an execution run.
pub async fn handle_execution_pause(ctx: &CommandContext, execution_run_id: Uuid) -> Result<()> {
    let mut paused = 0u32;
    for entry in ctx.executions.iter() {
        if entry.execution_run_id == Some(execution_run_id)
            && let Some(ref pt) = entry.pause
        {
            pt.pause();
            let _ = transition_session_state(
                &*ctx.session_store,
                &*ctx.session_content,
                &*ctx.session_coordinator,
                *entry.key(),
                &ctx.worker_id,
                Some(ExecutionPhase::Waiting),
                SessionStatus::Paused,
            );
            paused += 1;
        }
    }
    if paused > 0 {
        info!(%execution_run_id, paused, "Paused task executions");
    }
    Ok(())
}

/// Resume all paused tasks belonging to an execution run.
pub async fn handle_execution_resume(ctx: &CommandContext, execution_run_id: Uuid) -> Result<()> {
    let mut resumed = 0u32;
    for entry in ctx.executions.iter() {
        if entry.execution_run_id == Some(execution_run_id)
            && let Some(ref pt) = entry.pause
        {
            pt.resume();
            let _ = transition_session_state(
                &*ctx.session_store,
                &*ctx.session_content,
                &*ctx.session_coordinator,
                *entry.key(),
                &ctx.worker_id,
                Some(ExecutionPhase::CallingModel),
                SessionStatus::Active,
            );
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

async fn execute_routine_task(
    ctx: &CommandContext,
    routine_id: Uuid,
    task: nenjo::types::TaskType,
    execution_run_id: Uuid,
    task_id: Uuid,
    cancel: &CancellationToken,
) -> Result<()> {
    let provider = ctx.provider();
    let project_slug_value = match &task {
        nenjo::types::TaskType::Task(t) => project_slug(provider.manifest(), t.project_id),
        _ => String::new(),
    };
    let task_slug_value = match &task {
        nenjo::types::TaskType::Task(t) => t.slug.clone(),
        _ => "task".to_string(),
    };
    let mut handle = ctx
        .provider()
        .routine_by_id(routine_id)?
        .with_session_binding(nenjo::routines::SessionBinding {
            session_id: task_id,
            memory_namespace: session_memory_namespace(&*ctx.session_store, task_id),
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
    let mut trace_recorders: std::collections::HashMap<
        (uuid::Uuid, uuid::Uuid),
        ExecutionTraceRecorder,
    > = std::collections::HashMap::new();

    loop {
        tokio::select! {
            event = handle.recv() => {
                match event {
                    Some(ev) => {
                        // Track agent identity across step events.
                        if let nenjo::RoutineEvent::StepStarted { step_id, step_name, agent_id, .. } = &ev {
                            current_agent_id = *agent_id;
                            if let Some(agent_id) = agent_id {
                                step_agents.insert(*step_id, (*agent_id, step_name.clone()));
                            }
                        }
                        // Track token totals from completed steps
                        if let nenjo::RoutineEvent::StepCompleted { result, .. } = &ev {
                            total_input_tokens += result.input_tokens;
                            total_output_tokens += result.output_tokens;
                        }
                        if let nenjo::RoutineEvent::AgentEvent { step_id, event } = &ev
                            && let Some((agent_id, step_name)) = step_agents.get(step_id)
                        {
                            let recorder = trace_recorders.entry((*agent_id, *step_id)).or_insert_with(|| {
                                let agent = provider
                                    .manifest()
                                    .agents
                                    .iter()
                                    .find(|a| a.id == *agent_id);
                                let agent_name = agent
                                    .map(|a| a.name.as_str())
                                    .unwrap_or("agent");
                                ExecutionTraceRecorder::for_task_with_store(
                                    &ctx.config.workspace_dir,
                                    agent_name,
                                    *agent_id,
                                    TaskTraceLocation {
                                        project_slug: &project_slug_value,
                                        task_slug: &task_slug_value,
                                        step_name: Some(step_name.as_str()),
                                        step_id: Some(*step_id),
                                    },
                                    ctx.config.agent.execution_traces,
                                    ctx.session_content.clone(),
                                )
                            });
                            let _ = recorder.record(event);
                        }
                        if let Some(r) = routine_event_to_response(&ev, execution_run_id, Some(task_id), current_agent_id, ctx.provider().manifest()) {
                            let _ = ctx.response_tx.send(r);
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                handle.cancel();
                for recorder in trace_recorders.values_mut() {
                    let _ = recorder.finalize_with_error("Cancelled");
                }
                break;
            }
        }
    }

    let result = handle.output().await?;
    let _ = ctx.response_tx.send(Response::TaskCompleted {
        execution_run_id: execution_run_id.to_string(),
        task_id: Some(task_id.to_string()),
        success: result.passed,
        error: if result.passed {
            None
        } else {
            Some(result.output)
        },
        merge_error: None,
        total_input_tokens,
        total_output_tokens,
    });

    Ok(())
}

async fn execute_direct_task(
    ctx: &CommandContext,
    agent_id: Uuid,
    task: nenjo::types::TaskType,
    execution_run_id: Uuid,
    task_id: Uuid,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut builder = ctx.provider().agent_by_id(agent_id).await?;
    // Scope tools to the git worktree if one was created.
    if let nenjo::types::TaskType::Task(ref t) = task
        && let Some(ref git) = t.git
        && !git.work_dir.is_empty()
    {
        builder = builder.with_work_dir(&git.work_dir);
    }
    let runner = apply_session_memory_scope(builder, &*ctx.session_store, task_id)
        .build()
        .await?;
    let provider = ctx.provider();
    let manifest = provider.manifest().clone();
    let aname = agent_name(&manifest, agent_id);
    let project_slug = match &task {
        nenjo::types::TaskType::Task(t) => project_slug(&manifest, t.project_id),
        _ => String::new(),
    };
    let task_slug = match &task {
        nenjo::types::TaskType::Task(t) => t.slug.clone(),
        _ => "task".to_string(),
    };
    let task_project_id = match &task {
        nenjo::types::TaskType::Task(t) => t.project_id,
        _ => Uuid::nil(),
    };
    let trace_ref = task_trace_ref(&project_slug, &task_slug, &aname, agent_id);
    upsert_task_session(
        ctx,
        task_id,
        task_project_id,
        Some(agent_id),
        task_memory_namespace(Some(&aname), &project_slug).as_deref(),
        execution_run_id,
        Some(trace_ref),
        None,
        SessionStatus::Active,
    );
    let mut trace_recorder = ExecutionTraceRecorder::for_task_with_store(
        &ctx.config.workspace_dir,
        &aname,
        agent_id,
        TaskTraceLocation {
            project_slug: &project_slug,
            task_slug: &task_slug,
            step_name: None,
            step_id: None,
        },
        ctx.config.agent.execution_traces,
        ctx.session_content.clone(),
    );

    let mut handle = runner.task_stream(task).await?;

    // Update the registry with the actual pause token from the execution handle
    // so external pause/resume commands reach the turn loop.
    if let Some(mut entry) = ctx.executions.get_mut(&task_id) {
        entry.pause = Some(handle.pause_token());
    }

    loop {
        tokio::select! {
            event = handle.recv() => {
                match event {
                    Some(ev) => {
                        let _ = trace_recorder.record(&ev);
                        if let Some(response) = direct_task_turn_event_to_response(
                            &ev,
                            execution_run_id,
                            task_id,
                            agent_id,
                            &aname,
                            &manifest,
                        ) {
                            let _ = ctx.response_tx.send(response);
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                handle.abort();
                let _ = trace_recorder.finalize_with_error("Cancelled");
                break;
            }
        }
    }

    let (success, total_input_tokens, total_output_tokens) = if !cancel.is_cancelled() {
        let output = handle.output().await?;
        (true, output.input_tokens, output.output_tokens)
    } else {
        (false, 0, 0)
    };

    let _ = ctx.response_tx.send(Response::TaskCompleted {
        execution_run_id: execution_run_id.to_string(),
        task_id: Some(task_id.to_string()),
        success,
        error: if success {
            None
        } else {
            Some("Cancelled".to_string())
        },
        merge_error: None,
        total_input_tokens,
        total_output_tokens,
    });
    upsert_task_session(
        ctx,
        task_id,
        task_project_id,
        Some(agent_id),
        task_memory_namespace(Some(&aname), &project_slug).as_deref(),
        execution_run_id,
        None,
        None,
        if success {
            SessionStatus::Completed
        } else {
            SessionStatus::Cancelled
        },
    );

    Ok(())
}

fn direct_task_turn_event_to_response(
    event: &nenjo::TurnEvent,
    execution_run_id: Uuid,
    task_id: Uuid,
    agent_id: Uuid,
    agent_name: &str,
    manifest: &nenjo::manifest::Manifest,
) -> Option<Response> {
    let eid = execution_run_id.to_string();
    let tid = Some(task_id.to_string());
    let agent = Some(StepAgent {
        agent_id,
        agent_name: Some(agent_name.to_string()),
        agent_color: manifest
            .agents
            .iter()
            .find(|a| a.id == agent_id)
            .and_then(|a| a.color.clone()),
    });

    match event {
        nenjo::TurnEvent::ToolCallStart { calls, .. } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "step_started".to_string(),
            step_name: calls
                .first()
                .map(|c| c.tool_name.clone())
                .unwrap_or_else(|| "tool_call".to_string()),
            step_type: "tool".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "tool_names": calls.iter().map(|c| c.tool_name.clone()).collect::<Vec<_>>(),
                "text_preview": calls.first().and_then(|c| c.text_preview.clone()),
            }),
            agent,
        }),
        nenjo::TurnEvent::ToolCallEnd {
            tool_name, result, ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: if result.success {
                "step_completed".to_string()
            } else {
                "step_failed".to_string()
            },
            step_name: tool_name.clone(),
            step_type: "tool".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "success": result.success,
                "output_preview": result.output.lines().next().map(str::trim).filter(|s| !s.is_empty()),
                "error": result.error,
            }),
            agent,
        }),
        nenjo::TurnEvent::AbilityStarted {
            ability_name,
            task_input,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "step_started".to_string(),
            step_name: ability_name.clone(),
            step_type: "ability".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "task_preview": task_input,
            }),
            agent,
        }),
        nenjo::TurnEvent::AbilityCompleted {
            ability_name,
            success,
            final_output,
            ..
        } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: if *success {
                "step_completed".to_string()
            } else {
                "step_failed".to_string()
            },
            step_name: ability_name.clone(),
            step_type: "ability".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "success": success,
                "output_preview": final_output.lines().next().map(str::trim).filter(|s| !s.is_empty()),
            }),
            agent,
        }),
        nenjo::TurnEvent::Done { output } => Some(Response::TaskStepEvent {
            execution_run_id: eid,
            task_id: tid,
            event_type: "step_completed".to_string(),
            step_name: "agent_response".to_string(),
            step_type: "agent".to_string(),
            duration_ms: None,
            data: serde_json::json!({
                "output_preview": output.text.lines().next().map(str::trim).filter(|s| !s.is_empty()),
                "input_tokens": output.input_tokens,
                "output_tokens": output.output_tokens,
            }),
            agent,
        }),
        nenjo::TurnEvent::Paused | nenjo::TurnEvent::Resumed => None,
    }
}

/// Send `TaskCompleted` (failed) to the platform.
///
/// Used for early termination when the task cannot proceed (e.g. worktree
/// setup failure, missing routine/agent).
fn send_task_failed(ctx: &CommandContext, eid: &str, tid: &Option<String>, error: &str) {
    let _ = ctx.response_tx.send(Response::TaskCompleted {
        execution_run_id: eid.to_string(),
        task_id: tid.clone(),
        success: false,
        error: Some(error.to_string()),
        merge_error: None,
        total_input_tokens: 0,
        total_output_tokens: 0,
    });
}

// ---------------------------------------------------------------------------
// Git worktree lifecycle
// ---------------------------------------------------------------------------

/// Remove a repo's lock entry when no other task is using it.
///
/// The map holds one `Arc` and the caller holds another. If the strong count is
/// exactly 2, no other task is sharing this lock, so we can safely evict it.
fn evict_git_lock(
    locks: &crate::harness::GitLocks,
    repo_dir: &std::path::Path,
    lock: &std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    if std::sync::Arc::strong_count(lock) <= 2 {
        locks.remove(repo_dir);
    }
}

/// Create a git worktree for a task execution.
///
/// Branch name: `agent/{short_id}/{task_slug}`
/// Worktree path: `{workspace_dir}/{project_slug}/worktrees/{task_slug}`
///
/// When `configured_target` is set, the worktree is branched from that branch
/// instead of detecting the remote's default HEAD.
async fn setup_worktree(
    repo_dir: &Path,
    execution_run_id: Uuid,
    task_slug: &str,
    configured_target: Option<&str>,
) -> Result<GitContext> {
    let short_id = &execution_run_id.to_string()[..8];
    let branch = format!("agent/{short_id}/{task_slug}");
    let worktree_dir = repo_dir
        .parent()
        .unwrap_or(repo_dir)
        .join("worktrees")
        .join(task_slug);

    // Ensure worktree parent dir exists
    if let Some(parent) = worktree_dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Use configured target branch, or detect default from remote
    let target_branch = match configured_target {
        Some(b) => b.to_string(),
        None => default_branch(repo_dir)
            .await
            .unwrap_or_else(|| "main".to_string()),
    };

    // Fetch latest from origin so the worktree starts from up-to-date state
    let fetch_output = tokio::process::Command::new("git")
        .args(["fetch", "origin", &target_branch])
        .current_dir(repo_dir)
        .output()
        .await
        .context("Failed to spawn git fetch")?;

    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        warn!(error = %stderr.trim(), "git fetch failed, proceeding with local state");
    }

    // Clean up stale worktree/branch from a previous run that wasn't cleaned up
    // (e.g. crash, kill signal, timeout).
    if worktree_dir.exists() {
        warn!(path = %worktree_dir.display(), "Stale worktree found, removing before re-creating");
        let _ = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&worktree_dir)
            .current_dir(repo_dir)
            .output()
            .await;
        // Also try removing the directory if git worktree remove didn't
        let _ = tokio::fs::remove_dir_all(&worktree_dir).await;
    }
    // Delete stale branch if it exists
    let _ = tokio::process::Command::new("git")
        .args(["branch", "-D", &branch])
        .current_dir(repo_dir)
        .output()
        .await;

    // Create the worktree with a new branch
    let output = tokio::process::Command::new("git")
        .args(["worktree", "add", "-b", &branch])
        .arg(&worktree_dir)
        .arg(format!("origin/{target_branch}"))
        .current_dir(repo_dir)
        .output()
        .await
        .context("Failed to spawn git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr.trim());
    }

    // Get the repo remote URL
    let repo_url = get_remote_url(repo_dir).await.unwrap_or_default();

    let work_dir = worktree_dir.to_str().unwrap_or("").to_string();

    Ok(GitContext {
        branch,
        target_branch,
        work_dir,
        repo_url,
    })
}

/// Remove a worktree and delete its branch.
async fn cleanup_worktree(repo_dir: &Path, worktree_path: &str, branch: &str) -> Result<()> {
    // Remove the worktree
    let output = tokio::process::Command::new("git")
        .args(["worktree", "remove", "--force", worktree_path])
        .current_dir(repo_dir)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(error = %stderr.trim(), "git worktree remove failed");
    }

    // Delete the branch
    let output = tokio::process::Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(repo_dir)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!(error = %stderr.trim(), "git branch delete failed (may already be gone)");
    }

    Ok(())
}

/// Get the default branch name from the remote.
async fn default_branch(repo_dir: &Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD", "--short"])
        .current_dir(repo_dir)
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Some(
            branch
                .strip_prefix("origin/")
                .unwrap_or(&branch)
                .to_string(),
        )
    } else {
        None
    }
}

/// Get the remote URL of the repository.
async fn get_remote_url(repo_dir: &Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_dir)
        .output()
        .await
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}
