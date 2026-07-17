use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use async_trait::async_trait;
use nenjo::types::GitContext;
use nenjo_events::EncryptedPayload;
use uuid::Uuid;

use crate::runtime::GitLocks;
use nenjo::LocalRoutineExecutionWatcher;

#[derive(Clone)]
pub struct TaskCommandContext<S, W> {
    pub response_sink: S,
    pub worker_id: String,
    pub worktrees: W,
    pub git_locks: GitLocks,
    pub attachment_encoder: Arc<dyn TaskAttachmentEncoder>,
    pub(crate) local_execution_watcher: LocalRoutineExecutionWatcher,
}

#[async_trait]
/// Encrypts one task attachment using its UUID as AEAD object identity.
pub trait TaskAttachmentEncoder: Send + Sync {
    async fn encrypt_attachment(
        &self,
        attachment_id: Uuid,
        plaintext: &str,
    ) -> Result<EncryptedPayload>;
}

#[async_trait]
/// Manages git worktrees used by worker-owned task execution.
///
/// The harness only runs agent tasks. The worker owns repository layout,
/// branch naming, worktree creation, and cleanup because those operations are
/// host-specific and may need filesystem locking or platform policy.
pub trait TaskWorktreeManager: Send + Sync {
    /// Return the canonical repository directory for a project slug.
    fn repo_dir(&self, project_slug: &str) -> PathBuf;

    /// Create or restore a task-specific worktree and return the git context
    /// that should be passed into the agent task run.
    async fn setup_worktree(
        &self,
        repo_dir: &Path,
        execution_run_id: Uuid,
        task_slug: &str,
        configured_target: Option<&str>,
    ) -> Result<GitContext>;

    /// Clean up a task worktree after execution is complete or cancelled.
    async fn cleanup_worktree(
        &self,
        repo_dir: &Path,
        worktree_dir: &str,
        branch: &str,
    ) -> Result<()>;
}

#[async_trait]
impl<T> TaskWorktreeManager for Arc<T>
where
    T: TaskWorktreeManager + ?Sized,
{
    fn repo_dir(&self, project_slug: &str) -> PathBuf {
        self.as_ref().repo_dir(project_slug)
    }

    async fn setup_worktree(
        &self,
        repo_dir: &Path,
        execution_run_id: Uuid,
        task_slug: &str,
        configured_target: Option<&str>,
    ) -> Result<GitContext> {
        self.as_ref()
            .setup_worktree(repo_dir, execution_run_id, task_slug, configured_target)
            .await
    }

    async fn cleanup_worktree(
        &self,
        repo_dir: &Path,
        worktree_dir: &str,
        branch: &str,
    ) -> Result<()> {
        self.as_ref()
            .cleanup_worktree(repo_dir, worktree_dir, branch)
            .await
    }
}
