//! Repository sync handlers — clone, pull, and remove project repos.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nenjo_events::Response;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::event_bridge::project_slug;
use crate::harness::CommandContext;

/// Clone or pull a project repository, then notify the backend via NATS.
pub async fn handle_repo_sync(
    ctx: &CommandContext,
    project_id: Uuid,
    repo_url: &str,
    target_branch: &str,
) -> Result<()> {
    let provider = ctx.provider();
    let manifest = provider.manifest();
    let slug = project_slug(manifest, project_id);
    let repo_dir = ctx.config.workspace_dir.join(&slug).join("repo");
    let git_lock = ctx
        .git_locks
        .entry(repo_dir.clone())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone();

    let guard = match git_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            info!(
                %project_id,
                slug = %slug,
                dir = %repo_dir.display(),
                "Repo sync already in progress; waiting"
            );
            let guard = git_lock.lock().await;
            if repo_dir.join(".git").exists() {
                info!(%project_id, slug = %slug, "Repo sync already complete");
                let _ = ctx.response_tx.send(Response::RepoSyncComplete {
                    project_id,
                    success: true,
                    error: None,
                });
                drop(guard);
                evict_git_lock(&ctx.git_locks, &repo_dir, &git_lock);
                return Ok(());
            }
            guard
        }
    };

    info!(
        %project_id,
        slug = %slug,
        %repo_url,
        %target_branch,
        dir = %repo_dir.display(),
        "Syncing repo"
    );

    let result = if repo_dir.join(".git").exists() {
        git_pull(&repo_dir, target_branch).await
    } else if repo_dir.exists() {
        Err(anyhow::anyhow!(
            "repo directory exists but is not a git repository: {}",
            repo_dir.display()
        ))
    } else {
        git_clone(repo_url, &repo_dir, target_branch).await
    };

    // Notify backend of sync result via NATS response channel
    match &result {
        Ok(()) => {
            info!(%project_id, slug = %slug, "Repo sync complete");
            let _ = ctx.response_tx.send(Response::RepoSyncComplete {
                project_id,
                success: true,
                error: None,
            });
        }
        Err(e) => {
            error!(%project_id, slug = %slug, error = %e, "Repo sync failed");
            let _ = ctx.response_tx.send(Response::RepoSyncComplete {
                project_id,
                success: false,
                error: Some(e.to_string()),
            });
        }
    }

    drop(guard);
    evict_git_lock(&ctx.git_locks, &repo_dir, &git_lock);
    result
}

/// Remove a synced repository and any git worktrees.
pub async fn handle_repo_unsync(ctx: &CommandContext, project_id: Uuid) -> Result<()> {
    let provider = ctx.provider();
    let manifest = provider.manifest();
    let slug = project_slug(manifest, project_id);
    let repo_dir = ctx.config.workspace_dir.join(&slug).join("repo");
    let git_lock = ctx
        .git_locks
        .entry(repo_dir.clone())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = git_lock.lock().await;

    if !repo_dir.exists() {
        info!(%project_id, slug = %slug, "Repo directory doesn't exist, nothing to unsync");
        evict_git_lock(&ctx.git_locks, &repo_dir, &git_lock);
        return Ok(());
    }

    // Clean up any git worktrees first
    if repo_dir.join(".git").exists()
        && let Err(e) = cleanup_worktrees(&repo_dir).await
    {
        warn!(error = %e, "Failed to clean up worktrees, proceeding with removal");
    }

    // Remove the repo directory
    tokio::fs::remove_dir_all(&repo_dir)
        .await
        .with_context(|| format!("Failed to remove repo directory: {}", repo_dir.display()))?;

    info!(%project_id, slug = %slug, "Repo unsynced");
    evict_git_lock(&ctx.git_locks, &repo_dir, &git_lock);
    Ok(())
}

fn evict_git_lock(
    locks: &crate::harness::GitLocks,
    repo_dir: &Path,
    lock: &std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    if std::sync::Arc::strong_count(lock) <= 2 {
        locks.remove(repo_dir);
    }
}

/// Clone a repository to the target directory, checking out `target_branch`.
async fn git_clone(repo_url: &str, target: &Path, target_branch: &str) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let output = tokio::process::Command::new("git")
        .args([
            "clone",
            "--branch",
            target_branch,
            "--no-single-branch",
            repo_url,
        ])
        .arg(target)
        .output()
        .await
        .context("Failed to spawn git clone")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git clone failed: {}", stderr.trim());
    }

    Ok(())
}

/// Pull latest changes in an existing repository, resetting to `target_branch`.
async fn git_pull(repo_dir: &Path, target_branch: &str) -> Result<()> {
    // Use an explicit refspec so the fetch succeeds even if the repo was
    // previously cloned with --single-branch for a different branch.
    let refspec = format!("+refs/heads/{target_branch}:refs/remotes/origin/{target_branch}");
    let fetch = tokio::process::Command::new("git")
        .args(["fetch", "origin", &refspec])
        .current_dir(repo_dir)
        .output()
        .await
        .context("Failed to spawn git fetch")?;

    if !fetch.status.success() {
        let stderr = String::from_utf8_lossy(&fetch.stderr);
        anyhow::bail!("git fetch failed: {}", stderr.trim());
    }

    let reset = tokio::process::Command::new("git")
        .args(["reset", "--hard", &format!("origin/{target_branch}")])
        .current_dir(repo_dir)
        .output()
        .await
        .context("Failed to spawn git reset")?;

    if !reset.status.success() {
        let stderr = String::from_utf8_lossy(&reset.stderr);
        anyhow::bail!("git reset failed: {}", stderr.trim());
    }

    Ok(())
}

/// Remove all git worktrees for a repository.
async fn cleanup_worktrees(repo_dir: &Path) -> Result<()> {
    let output = tokio::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .await?;

    if !output.status.success() {
        return Ok(()); // no worktrees or not a git repo
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let worktree_paths: Vec<PathBuf> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|p| p != repo_dir) // skip the main worktree
        .collect();

    for path in worktree_paths {
        info!(worktree = %path.display(), "Removing worktree");
        let _ = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&path)
            .current_dir(repo_dir)
            .output()
            .await;
    }

    Ok(())
}
