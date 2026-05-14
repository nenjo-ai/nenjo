//! Worker-local git implementation for repo commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{info, warn};
use uuid::Uuid;

use nenjo_harness::GitLocks;

#[derive(Clone)]
pub struct WorkerRepoRuntime {
    pub workspace_dir: PathBuf,
    pub git_locks: GitLocks,
}

#[async_trait]
impl nenjo_harness::handlers::repo::RepoRuntime for WorkerRepoRuntime {
    async fn sync_repo(
        &self,
        project_id: Uuid,
        project_slug: &str,
        repo_url: &str,
        target_branch: &str,
    ) -> Result<()> {
        let repo_dir = self.workspace_dir.join(project_slug).join("repo");
        let git_lock = self
            .git_locks
            .entry(repo_dir.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();

        let guard = match git_lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                info!(
                    %project_id,
                    slug = %project_slug,
                    dir = %repo_dir.display(),
                    "Repo sync already in progress; waiting"
                );
                let guard = git_lock.lock().await;
                if repo_dir.join(".git").exists() {
                    info!(%project_id, slug = %project_slug, "Repo sync already complete");
                    drop(guard);
                    evict_git_lock(&self.git_locks, &repo_dir, &git_lock);
                    return Ok(());
                }
                guard
            }
        };

        info!(
            %project_id,
            slug = %project_slug,
            %repo_url,
            %target_branch,
            dir = %repo_dir.display(),
            "Syncing repo on worker"
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

        drop(guard);
        evict_git_lock(&self.git_locks, &repo_dir, &git_lock);
        result
    }

    async fn unsync_repo(&self, project_id: Uuid, project_slug: &str) -> Result<()> {
        let repo_dir = self.workspace_dir.join(project_slug).join("repo");
        let git_lock = self
            .git_locks
            .entry(repo_dir.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = git_lock.lock().await;

        if !repo_dir.exists() {
            info!(%project_id, slug = %project_slug, "Repo directory doesn't exist, nothing to unsync");
            evict_git_lock(&self.git_locks, &repo_dir, &git_lock);
            return Ok(());
        }

        if repo_dir.join(".git").exists()
            && let Err(error) = cleanup_worktrees(&repo_dir).await
        {
            warn!(error = %error, "Failed to clean up worktrees, proceeding with removal");
        }

        tokio::fs::remove_dir_all(&repo_dir)
            .await
            .with_context(|| format!("Failed to remove repo directory: {}", repo_dir.display()))?;

        evict_git_lock(&self.git_locks, &repo_dir, &git_lock);
        Ok(())
    }
}

fn evict_git_lock(locks: &GitLocks, repo_dir: &Path, lock: &Arc<tokio::sync::Mutex<()>>) {
    if Arc::strong_count(lock) <= 2 {
        locks.remove(repo_dir);
    }
}

async fn git_clone(repo_url: &str, target: &Path, target_branch: &str) -> Result<()> {
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

async fn git_pull(repo_dir: &Path, target_branch: &str) -> Result<()> {
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

async fn cleanup_worktrees(repo_dir: &Path) -> Result<()> {
    let output = tokio::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_dir)
        .output()
        .await?;

    if !output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let worktree_paths: Vec<PathBuf> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|path| path != repo_dir)
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
