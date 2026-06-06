//! Command handlers — one module per command category.

pub mod chat;
pub mod cron;
pub mod crypto;
pub mod domain;
pub mod heartbeat;
pub mod manifest;
pub mod packages;
pub mod repo;
pub mod task;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::types::GitContext;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use nenjo_events::{Command, Response};

use crate::event_loop::ResponseSender as EventLoopResponseSender;
use crate::handlers::chat::{ChatCommandContext, ChatCommandRequest, WorkerChatHarnessExt};
use crate::handlers::cron::{CronCommandContext, WorkerCronHarnessExt};
use crate::handlers::crypto::{CryptoCommandContext, WorkerCryptoHarnessExt};
use crate::handlers::domain::{DomainCommandContext, WorkerDomainHarnessExt};
use crate::handlers::heartbeat::{HeartbeatCommandContext, WorkerHeartbeatHarnessExt};
use crate::handlers::manifest::{
    ManifestChangedCommand, ManifestCommandContext, WorkerManifestHarnessExt,
};
use crate::handlers::packages::handle_package_graph_changed;
use crate::handlers::repo::{RepoCommandContext, WorkerRepoHarnessExt};
use crate::handlers::task::{
    TaskCommandContext, TaskExecuteRequest, TaskWorktreeManager, WorkerTaskHarnessExt,
};
pub use crate::runtime::CommandContext;
use crate::runtime::WorkerAccountKeyStore;

/// Sends platform responses produced by worker command handlers.
///
/// The worker keeps this trait small so tests, event-bus adapters, and secure
/// envelope senders can plug into the same handler code without pulling
/// transport details into the harness or command logic.
pub trait ResponseSender: Send + Sync {
    /// Deliver one typed platform response to the configured transport.
    fn send(&self, response: Response) -> Result<()>;
}

impl<T> ResponseSender for Arc<T>
where
    T: ResponseSender + ?Sized,
{
    fn send(&self, response: Response) -> Result<()> {
        self.as_ref().send(response)
    }
}

pub(crate) struct WorkerTaskWorktrees {
    workspace_dir: PathBuf,
}

#[async_trait]
impl TaskWorktreeManager for WorkerTaskWorktrees {
    fn repo_dir(&self, project_slug: &str) -> PathBuf {
        self.workspace_dir.join(project_slug).join("repo")
    }

    async fn setup_worktree(
        &self,
        repo_dir: &Path,
        execution_run_id: uuid::Uuid,
        task_slug: &str,
        configured_target: Option<&str>,
    ) -> Result<GitContext> {
        let short_id = &execution_run_id.to_string()[..8];
        let branch = format!("agent/{short_id}/{task_slug}");
        let worktree_dir = repo_dir
            .parent()
            .unwrap_or(repo_dir)
            .join("worktrees")
            .join(format!("{short_id}-{task_slug}"));

        if let Some(parent) = worktree_dir.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let target_branch = match configured_target {
            Some(branch) => branch.to_string(),
            None => default_branch(repo_dir)
                .await
                .unwrap_or_else(|| "main".to_string()),
        };

        let fetch_output = tokio::process::Command::new("git")
            .args(["fetch", "origin", &target_branch])
            .current_dir(repo_dir)
            .output()
            .await
            .map_err(anyhow::Error::from)
            .map_err(|error| error.context("Failed to spawn git fetch"))?;

        if !fetch_output.status.success() {
            let stderr = String::from_utf8_lossy(&fetch_output.stderr);
            warn!(error = %stderr.trim(), "git fetch failed, proceeding with local state");
        }

        if worktree_dir.exists() {
            warn!(path = %worktree_dir.display(), "Stale worktree found, removing before re-creating");
            let _ = tokio::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&worktree_dir)
                .current_dir(repo_dir)
                .output()
                .await;
            let _ = tokio::fs::remove_dir_all(&worktree_dir).await;
        }

        let _ = tokio::process::Command::new("git")
            .args(["branch", "-D", &branch])
            .current_dir(repo_dir)
            .output()
            .await;

        let output = tokio::process::Command::new("git")
            .args(["worktree", "add", "-b", &branch])
            .arg(&worktree_dir)
            .arg(format!("origin/{target_branch}"))
            .current_dir(repo_dir)
            .output()
            .await
            .map_err(anyhow::Error::from)
            .map_err(|error| error.context("Failed to spawn git worktree add"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git worktree add failed: {}", stderr.trim());
        }

        Ok(GitContext {
            branch,
            target_branch,
            work_dir: worktree_dir.to_str().unwrap_or("").to_string(),
            repo_url: get_remote_url(repo_dir).await.unwrap_or_default(),
        })
    }

    async fn cleanup_worktree(
        &self,
        repo_dir: &Path,
        worktree_path: &str,
        branch: &str,
    ) -> Result<()> {
        let output = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force", worktree_path])
            .current_dir(repo_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(error = %stderr.trim(), "git worktree remove failed");
        }

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
}

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

/// Route an incoming command to the appropriate handler.
pub async fn route_command(command: Command, ctx: CommandContext) -> Result<()> {
    match command {
        Command::ChatMessage {
            id,
            content,
            project,
            agent,
            target_type,
            target,
            session_id,
            domain_session_id,
            domain_activation,
            ..
        } => {
            ctx.harness
                .handle_chat(
                    &ctx.chat_context(),
                    ChatCommandRequest {
                        message_id: id.as_deref(),
                        content: &content,
                        project: project.as_deref(),
                        agent: agent.as_deref(),
                        target_type: target_type.as_deref(),
                        target: target.as_deref(),
                        session_id,
                        domain_session_id,
                        domain_activation,
                    },
                )
                .await
        }

        Command::ChatCancel { project, agent } => {
            ctx.harness
                .handle_chat_cancel(&ctx.chat_context(), &project, agent.as_deref())
                .await
        }

        Command::ChatSessionDelete {
            project,
            agent,
            session_id,
        } => {
            ctx.harness
                .handle_session_delete(&ctx.chat_context(), &project, &agent, session_id)
                .await
        }

        Command::ChatDomainExit {
            project: _,
            agent,
            domain_session_id,
            chat_session_id,
        } => {
            ctx.harness
                .handle_domain_exit(
                    &domain_context(&ctx),
                    &agent,
                    domain_session_id,
                    chat_session_id,
                )
                .await
        }

        Command::TaskExecute {
            task_id,
            project,
            routine,
            agent,
            execution_run_id,
            payload,
            ..
        } => {
            let payload = payload.ok_or_else(|| {
                anyhow::anyhow!("task.execute missing payload after command decode")
            })?;
            ctx.harness
                .handle_task_execute(
                    &ctx.task_context(),
                    TaskExecuteRequest {
                        task_id,
                        project: &project,
                        routine: routine.as_deref(),
                        agent: agent.as_deref(),
                        execution_run_id,
                        title: &payload.title,
                        description: payload.description.as_deref().unwrap_or(""),
                        slug: payload.slug.as_deref(),
                        acceptance_criteria: payload.acceptance_criteria.as_deref(),
                        tags: &payload.tags,
                        status: payload.status.as_deref(),
                        priority: payload.priority.as_deref(),
                        task_type: payload.task_type.as_deref(),
                        complexity: payload.complexity.as_deref(),
                    },
                )
                .await
        }

        Command::ExecutionCancel { execution_run_id } => {
            ctx.harness
                .handle_execution_cancel(&ctx.task_context(), execution_run_id)
                .await
        }

        Command::ExecutionPause { execution_run_id } => {
            ctx.harness
                .handle_execution_pause(&ctx.task_context(), execution_run_id)
                .await
        }

        Command::ExecutionResume { execution_run_id } => {
            ctx.harness
                .handle_execution_resume(&ctx.task_context(), execution_run_id)
                .await
        }

        Command::CronEnable {
            routine,
            project,
            schedule,
            timezone,
        } => {
            ctx.harness
                .handle_cron_enable(
                    &ctx.cron_context(),
                    &routine,
                    project.as_deref(),
                    &schedule,
                    timezone.as_deref(),
                    None,
                )
                .await
        }

        Command::CronDisable { routine } => {
            ctx.harness
                .handle_cron_disable(&ctx.cron_context(), &routine)
                .await
        }

        Command::CronTrigger {
            routine, project, ..
        } => {
            ctx.harness
                .handle_cron_trigger(&ctx.cron_context(), &routine, project.as_deref())
                .await
        }

        Command::AgentHeartbeatEnable {
            agent,
            interval,
            timezone,
        } => {
            ctx.harness
                .handle_agent_heartbeat_enable(
                    &ctx.heartbeat_context(),
                    &agent,
                    &interval,
                    timezone.as_deref(),
                    None,
                )
                .await
        }

        Command::AgentHeartbeatDisable { agent } => {
            ctx.harness
                .handle_agent_heartbeat_disable(&ctx.heartbeat_context(), &agent)
                .await
        }

        Command::AgentHeartbeatTrigger { agent } => {
            ctx.harness
                .handle_agent_heartbeat_trigger(&ctx.heartbeat_context(), &agent)
                .await
        }

        Command::RepoSync {
            project,
            repo_url,
            target_branch,
        } => {
            ctx.harness
                .handle_repo_sync(&repo_context(&ctx), &project, &repo_url, &target_branch)
                .await
        }

        Command::RepoUnsync { project } => {
            ctx.harness
                .handle_repo_unsync(&repo_context(&ctx), &project)
                .await
        }

        Command::WorkerPing => {
            let _ = ctx.response_tx.send(Response::WorkerPong);
            Ok(())
        }

        Command::WorkerAccountKeyUpdated { wrapped_ack } => {
            ctx.harness
                .handle_worker_account_key_updated(&ctx.crypto_context(), wrapped_ack)
                .await
        }

        Command::ManifestChanged {
            resource_type,
            resource,
            action,
            project,
            payload,
            encrypted_payload,
        } => ctx
            .harness
            .handle_manifest_changed(
                &ctx.manifest_context(),
                ManifestChangedCommand {
                    resource_type,
                    resource: nenjo::Slug::parse(resource)?,
                    action,
                    project: project.map(nenjo::Slug::parse).transpose()?,
                    payload,
                    encrypted_payload,
                },
            )
            .await
            .map_err(Into::into),

        Command::PackageGraphChanged { packages } => {
            handle_package_graph_changed(&ctx, packages).await
        }
    }
}

fn domain_context(ctx: &CommandContext) -> DomainCommandContext {
    DomainCommandContext {
        worker_id: ctx.worker_name.clone(),
    }
}

impl CommandContext {
    pub(crate) fn chat_context(&self) -> ChatCommandContext<EventLoopResponseSender> {
        ChatCommandContext {
            response_sink: self.response_tx.clone(),
            worker_id: self.worker_name.clone(),
        }
    }

    pub(crate) fn task_context(
        &self,
    ) -> TaskCommandContext<EventLoopResponseSender, WorkerTaskWorktrees> {
        TaskCommandContext {
            response_sink: self.response_tx.clone(),
            worker_id: self.worker_name.clone(),
            worktrees: WorkerTaskWorktrees {
                workspace_dir: self.config.workspace_dir.clone(),
            },
            git_locks: self.git_locks.clone(),
        }
    }

    pub(crate) fn cron_context(&self) -> CronCommandContext<EventLoopResponseSender> {
        CronCommandContext {
            response_sink: self.response_tx.clone(),
            worker_id: self.worker_name.clone(),
        }
    }

    pub(crate) fn heartbeat_context(&self) -> HeartbeatCommandContext<EventLoopResponseSender> {
        HeartbeatCommandContext {
            response_sink: self.response_tx.clone(),
            worker_id: self.worker_name.clone(),
        }
    }

    pub(crate) fn crypto_context(&self) -> CryptoCommandContext<WorkerAccountKeyStore> {
        CryptoCommandContext {
            actor_user_id: self.actor_user_id,
            account_keys: WorkerAccountKeyStore {
                auth_provider: self.auth_provider.clone(),
            },
        }
    }

    pub(crate) fn manifest_context(
        &self,
    ) -> ManifestCommandContext<
        crate::bootstrap::WorkerManifestCache,
        std::sync::Arc<crate::external_mcp::ExternalMcpPool>,
    > {
        ManifestCommandContext {
            client: self.api.clone(),
            store: crate::bootstrap::WorkerManifestCache {
                manifests_dir: self.config.manifests_dir.clone(),
                workspace_dir: self.config.workspace_dir.clone(),
                state_dir: self.config.state_dir.clone(),
                config_dir: self.config.config_dir.clone(),
            },
            mcp: Some(self.external_mcp.clone()),
        }
    }
}

fn repo_context(
    ctx: &CommandContext,
) -> RepoCommandContext<EventLoopResponseSender, repo::WorkerRepoRuntime> {
    RepoCommandContext {
        response_sink: ctx.response_tx.clone(),
        repo_runtime: repo::WorkerRepoRuntime {
            workspace_dir: ctx.config.workspace_dir.clone(),
            git_locks: ctx.git_locks.clone(),
        },
    }
}
