//! Task execution handlers — with git worktree lifecycle.

use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo::types::GitContext;
use nenjo_events::Response;

use super::event_bridge::{
    agent_name, project_slug, routine_event_to_response, turn_event_to_stream_event,
};
use crate::harness::{ActiveExecution, CommandContext};

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

    // Set up git worktree if the project has a synced repo.
    // If the repo exists but worktree creation fails, the task fails —
    // we don't run tasks against a dirty or shared working tree.
    let git_ctx = if repo_dir.join(".git").exists() {
        let wt = setup_worktree(&repo_dir, &pslug, execution_run_id, task_slug).await?;
        info!(branch = %wt.branch, work_dir = %wt.work_dir, "Created git worktree for task");
        Some(wt)
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
            cancel: cancel.clone(),
            pause: Some(pause.clone()),
        },
    );

    let result = if let Some(rid) = routine_id {
        execute_routine_task(ctx, rid, task, execution_run_id, task_id, &cancel).await
    } else if let Some(aid) = assigned_agent_id {
        execute_direct_task(ctx, aid, task, execution_run_id, task_id, &cancel).await
    } else {
        warn!("TaskExecute without routine_id or assigned_agent_id");
        Ok(())
    };

    // Unregister execution
    ctx.executions.remove(&task_id);

    // Clean up worktree after execution
    if let Some(ref wt) = git_ctx {
        if let Err(e) = cleanup_worktree(&repo_dir, &wt.work_dir, &wt.branch).await {
            warn!(error = %e, branch = %wt.branch, "Failed to clean up worktree");
        } else {
            debug!(branch = %wt.branch, "Cleaned up worktree");
        }
    }

    result
}

/// Cancel a running execution by execution_run_id.
pub async fn handle_execution_cancel(ctx: &CommandContext, execution_run_id: Uuid) -> Result<()> {
    if let Some((_, exec)) = ctx.executions.remove(&execution_run_id) {
        exec.cancel.cancel();
        info!(%execution_run_id, "Cancelled active task execution");
    }
    Ok(())
}

/// Pause a running execution. The agent stops before the next LLM call.
pub async fn handle_execution_pause(ctx: &CommandContext, execution_run_id: Uuid) -> Result<()> {
    if let Some(exec) = ctx.executions.get(&execution_run_id) {
        if let Some(ref pt) = exec.pause {
            pt.pause();
            info!(%execution_run_id, "Paused execution");
        }
    }
    Ok(())
}

/// Resume a paused execution.
pub async fn handle_execution_resume(ctx: &CommandContext, execution_run_id: Uuid) -> Result<()> {
    if let Some(exec) = ctx.executions.get(&execution_run_id) {
        if let Some(ref pt) = exec.pause {
            pt.resume();
            info!(%execution_run_id, "Resumed execution");
        }
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
    let mut handle = ctx
        .provider()
        .routine_by_id(routine_id)?
        .run_stream(task)
        .await?;

    // Accumulate token metrics from step events as they stream through.
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    loop {
        tokio::select! {
            event = handle.recv() => {
                match event {
                    Some(ev) => {
                        // Track token totals from completed steps
                        if let nenjo::RoutineEvent::StepCompleted { result, .. } = &ev {
                            total_input_tokens += result.input_tokens;
                            total_output_tokens += result.output_tokens;
                        }
                        if let Some(r) = routine_event_to_response(&ev, execution_run_id, Some(task_id)) {
                            let _ = ctx.response_tx.send(r);
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                handle.cancel();
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
    });

    // Log execution completion with token metrics
    let _ = ctx.response_tx.send(Response::ExecutionCompleted {
        id: execution_run_id,
        success: result.passed,
        error: None,
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
    let runner = ctx.provider().agent_by_id(agent_id).await?.build()?;
    let provider = ctx.provider();
    let aname = agent_name(provider.manifest(), agent_id);

    let mut handle = runner.task_stream(task).await?;

    loop {
        tokio::select! {
            event = handle.recv() => {
                match event {
                    Some(ev) => {
                        if let Some(se) = turn_event_to_stream_event(&ev, &aname) {
                            let _ = ctx.response_tx.send(
                                Response::AgentResponse { payload: se }
                            );
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                handle.abort();
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
    });

    // Log execution completion with token metrics
    let _ = ctx.response_tx.send(Response::ExecutionCompleted {
        id: execution_run_id,
        success,
        error: None,
        total_input_tokens,
        total_output_tokens,
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Git worktree lifecycle
// ---------------------------------------------------------------------------

/// Create a git worktree for a task execution.
///
/// Branch name: `agent/{short_id}/{task_slug}`
/// Worktree path: `{workspace_dir}/{project_slug}/worktrees/{task_slug}/{short_id}`
async fn setup_worktree(
    repo_dir: &PathBuf,
    _project_slug: &str,
    execution_run_id: Uuid,
    task_slug: &str,
) -> Result<GitContext> {
    let short_id = &execution_run_id.to_string()[..8];
    let branch = format!("agent/{short_id}/{task_slug}");
    let worktree_dir = repo_dir
        .parent()
        .unwrap_or(repo_dir)
        .join("worktrees")
        .join(task_slug)
        .join(short_id);

    // Ensure worktree parent dir exists
    if let Some(parent) = worktree_dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Determine default branch to base the worktree on
    let target_branch = default_branch(repo_dir)
        .await
        .unwrap_or_else(|| "main".to_string());

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
async fn cleanup_worktree(repo_dir: &PathBuf, worktree_path: &str, branch: &str) -> Result<()> {
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
async fn default_branch(repo_dir: &PathBuf) -> Option<String> {
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
async fn get_remote_url(repo_dir: &PathBuf) -> Option<String> {
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
