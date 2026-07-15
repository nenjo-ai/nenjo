//! Command handlers — one module per command category.

pub mod chat;
pub mod cron;
pub mod crypto;
pub mod domain;
pub mod heartbeat;
pub mod manifest;
mod notification;
pub mod packages;
pub mod repo;
pub mod task;
mod voice_input;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::types::GitContext;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use nenjo_events::{Command, EncryptedPayload, ResourceType, Response};
use serde_json::Value;

use crate::crypto::decrypt_text_with_provider;
use crate::event_loop::ResponseSender as EventLoopResponseSender;
use crate::handlers::chat::{
    ChatCommandContext, ChatCommandRequest, ChatSlashCommandRequest, WorkerChatHarnessExt,
};
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
use crate::handlers::voice_input::{VoiceInputTranscribeRequest, handle_voice_input_transcribe};
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
                        template_override: None,
                        hook_scopes: Vec::new(),
                    },
                )
                .await
        }

        Command::ChatCommand {
            id,
            command,
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
                .handle_chat_command(
                    &ctx.chat_context(),
                    ChatSlashCommandRequest {
                        message_id: id.as_deref(),
                        command: &command,
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

        Command::ChatCancel { agent, session_id } => {
            ctx.harness
                .handle_chat_cancel(&ctx.chat_context(), agent.as_deref(), session_id)
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

        Command::VoiceInputTranscribe {
            job_id,
            session_id,
            audio,
            provider,
            model,
            base_url,
            language,
            ..
        } => {
            handle_voice_input_transcribe(
                &ctx,
                VoiceInputTranscribeRequest {
                    job_id,
                    session_id,
                    audio,
                    provider: &provider,
                    model: &model,
                    base_url: base_url.as_deref(),
                    language: language.as_deref(),
                },
            )
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
            encrypted_payload: _,
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
            task,
            encrypted_task: _,
        } => {
            ctx.harness
                .handle_cron_enable(
                    &ctx.cron_context(),
                    crate::handlers::cron::CronEnableRequest {
                        routine: &routine,
                        project: project.as_deref(),
                        schedule: &schedule,
                        timezone: timezone.as_deref(),
                        task_content: task,
                        start_at: None,
                    },
                )
                .await
        }

        Command::CronDisable { routine } => {
            ctx.harness
                .handle_cron_disable(&ctx.cron_context(), &routine)
                .await
        }

        Command::CronTrigger {
            routine,
            project,
            task,
            encrypted_task: _,
        } => {
            ctx.harness
                .handle_cron_trigger(&ctx.cron_context(), &routine, project.as_deref(), task)
                .await
        }

        Command::AgentHeartbeatEnable {
            agent,
            interval,
            timezone,
            instructions,
            encrypted_instructions: _,
        } => {
            ctx.harness
                .handle_agent_heartbeat_enable(
                    &ctx.heartbeat_context(),
                    &agent,
                    &interval,
                    timezone.as_deref(),
                    instructions.map(|content| content.instructions),
                    None,
                )
                .await
        }

        Command::AgentHeartbeatDisable { agent } => {
            ctx.harness
                .handle_agent_heartbeat_disable(&ctx.heartbeat_context(), &agent)
                .await
        }

        Command::AgentHeartbeatTrigger {
            agent,
            instructions,
            encrypted_instructions: _,
        } => {
            ctx.harness
                .handle_agent_heartbeat_trigger(
                    &ctx.heartbeat_context(),
                    &agent,
                    instructions.map(|content| content.instructions),
                )
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
            schema: _,
            resource_id,
            resource_type,
            resource,
            action,
            project,
            payload,
            encrypted_payload,
        } => {
            let payload = materialize_manifest_changed_payload(
                &ctx,
                resource_type,
                payload,
                encrypted_payload.as_ref(),
            )
            .await?;
            ctx.harness
                .handle_manifest_changed(
                    &ctx.manifest_context(),
                    ManifestChangedCommand {
                        resource_id,
                        resource_type,
                        resource: nenjo::Slug::parse(resource)?,
                        action,
                        project: project.map(nenjo::Slug::parse).transpose()?,
                        payload,
                        encrypted_payload: None,
                    },
                )
                .await
                .map_err(Into::into)
        }

        Command::PackageGraphChanged { packages } => {
            handle_package_graph_changed(&ctx, packages).await
        }
    }
}

async fn materialize_manifest_changed_payload(
    ctx: &CommandContext,
    resource_type: ResourceType,
    payload: Option<Value>,
    encrypted_payload: Option<&EncryptedPayload>,
) -> Result<Option<Value>> {
    let Some(encrypted_payload) = encrypted_payload else {
        return materialize_nested_manifest_payloads(ctx, resource_type, payload).await;
    };

    let plaintext = decrypt_text_with_provider(&ctx.auth_provider, encrypted_payload).await?;
    let payload = materialize_nested_manifest_payloads(ctx, resource_type, payload).await?;
    Ok(Some(decrypted_manifest_payload_value(
        payload,
        encrypted_payload,
        plaintext,
    )))
}

async fn materialize_nested_manifest_payloads(
    ctx: &CommandContext,
    resource_type: ResourceType,
    payload: Option<Value>,
) -> Result<Option<Value>> {
    let Some(mut payload) = payload else {
        return Ok(None);
    };
    if resource_type == ResourceType::Routine {
        decrypt_nested_routine_step_instructions(ctx, &mut payload).await?;
    }
    Ok(Some(payload))
}

async fn decrypt_nested_routine_step_instructions(
    ctx: &CommandContext,
    payload: &mut Value,
) -> Result<()> {
    let Some(data) = inline_payload_data_mut(payload) else {
        return Ok(());
    };
    let Some(steps) = data.get_mut("steps").and_then(Value::as_array_mut) else {
        return Ok(());
    };

    for step in steps {
        let Some(payload_value) = step.get("encrypted_payload").cloned() else {
            continue;
        };
        let Ok(encrypted_payload) = serde_json::from_value::<EncryptedPayload>(payload_value)
        else {
            warn!("Failed to parse nested routine step encrypted payload");
            continue;
        };
        if encrypted_payload.object_type != "routine.step.instructions" {
            continue;
        }
        let plaintext = decrypt_text_with_provider(&ctx.auth_provider, &encrypted_payload).await?;
        let instructions = serde_json::from_str::<Value>(&plaintext)
            .ok()
            .and_then(|value| {
                value
                    .get("instructions")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or(plaintext);
        let Some(step_object) = step.as_object_mut() else {
            continue;
        };
        let config = step_object
            .entry("config")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(config_object) = config.as_object_mut() {
            config_object.insert("instructions".to_string(), Value::String(instructions));
        }
    }

    Ok(())
}

fn inline_payload_data_mut(payload: &mut Value) -> Option<&mut Value> {
    if payload
        .get("__nenjo_decrypted_manifest_payload")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return payload
            .get_mut("inline_payload")
            .and_then(inline_payload_data_mut);
    }
    if payload.get("schema").is_some() && payload.get("data").is_some() {
        return payload.get_mut("data");
    }
    Some(payload)
}

fn decrypted_manifest_payload_value(
    inline_payload: Option<Value>,
    encrypted_payload: &EncryptedPayload,
    plaintext: String,
) -> Value {
    let decrypted_payload = serde_json::from_str(&plaintext).unwrap_or(Value::String(plaintext));
    serde_json::json!({
        "__nenjo_decrypted_manifest_payload": true,
        "object_type": encrypted_payload.object_type,
        "object_id": encrypted_payload.object_id,
        "inline_payload": inline_payload,
        "decrypted_payload": decrypted_payload,
    })
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
            state_dir: self.config.state_dir.clone(),
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
        Arc<crate::bootstrap::WorkerManifestCache>,
        std::sync::Arc<crate::external_mcp::ExternalMcpPool>,
    > {
        ManifestCommandContext {
            client: self.api.clone(),
            store: self.manifest_cache.clone(),
            bootstrap_cache: Some(self.manifest_cache.clone()),
            mcp: Some(self.external_mcp.clone()),
            change_lock: self.manifest_change_lock.clone(),
        }
    }
}

fn repo_context(
    ctx: &CommandContext,
) -> RepoCommandContext<EventLoopResponseSender, repo::WorkerRepoRuntime> {
    RepoCommandContext {
        org_response_sink: ctx.org_response_tx.clone(),
        repo_runtime: repo::WorkerRepoRuntime {
            workspace_dir: ctx.config.workspace_dir.clone(),
            git_locks: ctx.git_locks.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn encrypted_payload(object_id: Uuid) -> EncryptedPayload {
        EncryptedPayload {
            account_id: Uuid::new_v4(),
            encryption_scope: Some("org".to_string()),
            object_id,
            object_type: "manifest.command.content".to_string(),
            algorithm: "aes-256-gcm".to_string(),
            key_version: 1,
            nonce: "nonce".to_string(),
            ciphertext: "ciphertext".to_string(),
        }
    }

    #[test]
    fn decrypted_manifest_payload_preserves_inline_metadata_and_json_string_content() {
        let object_id = Uuid::new_v4();
        let encrypted_payload = encrypted_payload(object_id);
        let inline_payload = serde_json::json!({
            "schema": "manifest.resource.v1",
            "data": {
                "id": object_id,
                "name": "design",
                "command": "/design",
                "content": ""
            }
        });

        let payload = decrypted_manifest_payload_value(
            Some(inline_payload.clone()),
            &encrypted_payload,
            serde_json::json!("Use the design command").to_string(),
        );

        assert_eq!(payload["__nenjo_decrypted_manifest_payload"], true);
        assert_eq!(payload["object_type"], "manifest.command.content");
        assert_eq!(payload["object_id"], serde_json::json!(object_id));
        assert_eq!(payload["inline_payload"], inline_payload);
        assert_eq!(payload["decrypted_payload"], "Use the design command");
    }

    #[test]
    fn decrypted_manifest_payload_accepts_legacy_raw_text_content() {
        let object_id = Uuid::new_v4();
        let encrypted_payload = encrypted_payload(object_id);

        let payload =
            decrypted_manifest_payload_value(None, &encrypted_payload, "raw command body".into());

        assert!(payload["inline_payload"].is_null());
        assert_eq!(payload["decrypted_payload"], "raw command body");
    }
}
