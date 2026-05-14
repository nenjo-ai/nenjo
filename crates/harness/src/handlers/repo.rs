//! Repository sync handlers.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo_events::Response;
use tracing::{error, info};
use uuid::Uuid;

use super::ResponseSender;
use crate::event_bridge::project_slug;
use crate::execution_trace::ExecutionTraceRuntime;
use crate::{Harness, HarnessProvider};

#[async_trait]
pub trait RepoRuntime: Send + Sync {
    async fn sync_repo(
        &self,
        project_id: Uuid,
        project_slug: &str,
        repo_url: &str,
        target_branch: &str,
    ) -> Result<()>;

    async fn unsync_repo(&self, project_id: Uuid, project_slug: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct RepoCommandContext<S, R> {
    pub response_sink: S,
    pub repo_runtime: R,
}

impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    /// Clone or pull a project repository.
    pub async fn handle_repo_sync<S, R>(
        &self,
        ctx: &RepoCommandContext<S, R>,
        project_id: Uuid,
        repo_url: &str,
        target_branch: &str,
    ) -> Result<()>
    where
        S: ResponseSender,
        R: RepoRuntime,
    {
        let provider = self.provider();
        let manifest = provider.manifest();
        let slug = project_slug(manifest, project_id);

        info!(
            %project_id,
            slug = %slug,
            %repo_url,
            %target_branch,
            "Syncing repo"
        );

        let result = ctx
            .repo_runtime
            .sync_repo(project_id, &slug, repo_url, target_branch)
            .await;

        match &result {
            Ok(()) => {
                info!(%project_id, slug = %slug, "Repo sync complete");
                let _ = ctx.response_sink.send(Response::RepoSyncComplete {
                    project_id,
                    success: true,
                    error: None,
                });
            }
            Err(error) => {
                error!(%project_id, slug = %slug, error = %error, "Repo sync failed");
                let _ = ctx.response_sink.send(Response::RepoSyncComplete {
                    project_id,
                    success: false,
                    error: Some(error.to_string()),
                });
            }
        }

        result
    }

    /// Remove a synced repository and any git worktrees.
    pub async fn handle_repo_unsync<S, R>(
        &self,
        ctx: &RepoCommandContext<S, R>,
        project_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
        R: RepoRuntime,
    {
        let provider = self.provider();
        let manifest = provider.manifest();
        let slug = project_slug(manifest, project_id);
        let result = ctx.repo_runtime.unsync_repo(project_id, &slug).await;
        match &result {
            Ok(()) => {
                info!(%project_id, slug = %slug, "Repo unsynced");
            }
            Err(error) => {
                error!(%project_id, slug = %slug, error = %error, "Repo unsync failed");
            }
        }
        result
    }
}

#[async_trait]
impl<T> RepoRuntime for Arc<T>
where
    T: RepoRuntime + ?Sized,
{
    async fn sync_repo(
        &self,
        project_id: Uuid,
        project_slug: &str,
        repo_url: &str,
        target_branch: &str,
    ) -> Result<()> {
        self.as_ref()
            .sync_repo(project_id, project_slug, repo_url, target_branch)
            .await
    }

    async fn unsync_repo(&self, project_id: Uuid, project_slug: &str) -> Result<()> {
        self.as_ref().unsync_repo(project_id, project_slug).await
    }
}
