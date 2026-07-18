//! Recovery and snapshot helpers for project task worktrees.

use std::path::Path;

use nenjo::types::GitContext;
use nenjo_harness::{Harness, ProviderRuntime};
use nenjo_sessions::{CheckpointQuery, WorktreeSnapshot};
use tracing::warn;
use uuid::Uuid;

use crate::runtime::GitLocks;

pub(super) async fn restore_task_git_context<P, SessionRt>(
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

pub(super) fn task_worktree_snapshot(
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

/// Remove a repo lock once no other task shares it.
pub(super) fn evict_git_lock(
    locks: &GitLocks,
    repo_dir: &Path,
    lock: &std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    if std::sync::Arc::strong_count(lock) <= 2 {
        locks.remove(repo_dir);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::worktree_listing_contains;

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
}
