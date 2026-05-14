use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use async_trait::async_trait;
use nenjo::types::GitContext;
use uuid::Uuid;

#[derive(Clone)]
pub struct TaskCommandContext<S, W> {
    pub response_sink: S,
    pub worker_id: String,
    pub worktrees: W,
}

#[async_trait]
pub trait TaskWorktreeManager: Send + Sync {
    fn repo_dir(&self, project_slug: &str) -> PathBuf;

    async fn setup_worktree(
        &self,
        repo_dir: &Path,
        execution_run_id: Uuid,
        task_slug: &str,
        configured_target: Option<&str>,
    ) -> Result<GitContext>;

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
