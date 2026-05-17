//! Command handlers — one module per command category.

pub mod repo;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::types::GitContext;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use nenjo_events::{Command, Response};
use nenjo_harness::handlers::{
    chat::{ChatCommandContext, ChatRequest},
    cron::CronCommandContext,
    crypto::CryptoCommandContext,
    domain::DomainCommandContext,
    heartbeat::HeartbeatCommandContext,
    repo::RepoCommandContext,
    task::{TaskCommandContext, TaskExecuteRequest, TaskWorktreeManager},
};

use crate::event_loop::ResponseSender;
pub use crate::runtime::CommandContext;
use crate::runtime::WorkerAccountKeyStore;

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
            .join(task_slug);

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
            project_id,
            agent_id,
            session_id,
            domain_session_id,
            ..
        } => {
            ctx.harness
                .handle_chat(
                    &ctx.chat_context(),
                    ChatRequest {
                        message_id: id.as_deref(),
                        content: &content,
                        project_id,
                        agent_id,
                        session_id,
                        domain_session_id,
                    },
                )
                .await
        }

        Command::ChatCancel {
            project_id,
            agent_id,
        } => {
            ctx.harness
                .handle_chat_cancel(&ctx.chat_context(), project_id, agent_id)
                .await
        }

        Command::ChatSessionDelete {
            project_id,
            agent_id,
            session_id,
        } => {
            ctx.harness
                .handle_session_delete(&ctx.chat_context(), project_id, agent_id, session_id)
                .await
        }

        Command::ChatDomainEnter {
            project_id,
            agent_id,
            domain_command,
            session_id,
        } => {
            ctx.harness
                .handle_domain_enter(
                    &domain_context(&ctx),
                    project_id,
                    agent_id,
                    &domain_command,
                    session_id,
                )
                .await
        }

        Command::ChatDomainExit {
            project_id,
            agent_id,
            domain_session_id,
        } => {
            let _ = project_id;
            ctx.harness
                .handle_domain_exit(&domain_context(&ctx), agent_id, domain_session_id)
                .await
        }

        Command::TaskExecute {
            task_id,
            project_id,
            routine_id,
            assigned_agent_id,
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
                        project_id,
                        routine_id,
                        assigned_agent_id,
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
            routine_id,
            project_id,
            schedule,
            timezone,
        } => {
            ctx.harness
                .handle_cron_enable(
                    &ctx.cron_context(),
                    routine_id,
                    project_id,
                    &schedule,
                    timezone.as_deref(),
                    None,
                )
                .await
        }

        Command::CronDisable { routine_id } => {
            ctx.harness
                .handle_cron_disable(&ctx.cron_context(), routine_id)
                .await
        }

        Command::CronTrigger {
            routine_id,
            project_id,
            ..
        } => {
            ctx.harness
                .handle_cron_trigger(&ctx.cron_context(), routine_id, project_id)
                .await
        }

        Command::AgentHeartbeatEnable {
            agent_id,
            interval,
            timezone,
        } => {
            ctx.harness
                .handle_agent_heartbeat_enable(
                    &ctx.heartbeat_context(),
                    agent_id,
                    &interval,
                    timezone.as_deref(),
                    None,
                )
                .await
        }

        Command::AgentHeartbeatDisable { agent_id } => {
            ctx.harness
                .handle_agent_heartbeat_disable(&ctx.heartbeat_context(), agent_id)
                .await
        }

        Command::AgentHeartbeatTrigger { agent_id } => {
            ctx.harness
                .handle_agent_heartbeat_trigger(&ctx.heartbeat_context(), agent_id)
                .await
        }

        Command::RepoSync {
            project_id,
            repo_url,
            target_branch,
        } => {
            ctx.harness
                .handle_repo_sync(&repo_context(&ctx), project_id, &repo_url, &target_branch)
                .await
        }

        Command::RepoUnsync { project_id } => {
            ctx.harness
                .handle_repo_unsync(&repo_context(&ctx), project_id)
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
            resource_id,
            action,
            project_id,
            payload,
            encrypted_payload,
            encrypted_payloads: _,
        } => ctx
            .harness
            .handle_manifest_changed(
                resource_type,
                resource_id,
                action,
                project_id,
                payload,
                encrypted_payload,
            )
            .await
            .map_err(Into::into),
    }
}

fn domain_context(ctx: &CommandContext) -> DomainCommandContext<ResponseSender> {
    DomainCommandContext {
        response_sink: ctx.response_tx.clone(),
        worker_id: ctx.worker_name.clone(),
    }
}

impl CommandContext {
    pub(crate) fn chat_context(&self) -> ChatCommandContext<ResponseSender> {
        ChatCommandContext {
            response_sink: self.response_tx.clone(),
            worker_id: self.worker_name.clone(),
        }
    }

    pub(crate) fn task_context(&self) -> TaskCommandContext<ResponseSender, WorkerTaskWorktrees> {
        TaskCommandContext {
            response_sink: self.response_tx.clone(),
            worker_id: self.worker_name.clone(),
            worktrees: WorkerTaskWorktrees {
                workspace_dir: self.config.workspace_dir.clone(),
            },
        }
    }

    pub(crate) fn cron_context(&self) -> CronCommandContext<ResponseSender> {
        CronCommandContext {
            response_sink: self.response_tx.clone(),
            worker_id: self.worker_name.clone(),
        }
    }

    pub(crate) fn heartbeat_context(&self) -> HeartbeatCommandContext<ResponseSender> {
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
}

fn repo_context(
    ctx: &CommandContext,
) -> RepoCommandContext<ResponseSender, repo::WorkerRepoRuntime> {
    RepoCommandContext {
        response_sink: ctx.response_tx.clone(),
        repo_runtime: repo::WorkerRepoRuntime {
            workspace_dir: ctx.config.workspace_dir.clone(),
            git_locks: ctx.git_locks.clone(),
        },
    }
}
