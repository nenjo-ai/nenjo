//! Command handlers — one module per command category.

pub mod chat;
pub mod cron;
pub mod domain;
pub mod event_bridge;
pub mod manifest;
pub mod repo;
pub mod task;

use anyhow::Result;

use nenjo_events::{Command, Response};

pub use crate::harness::CommandContext;

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
            chat::handle_chat(
                &ctx,
                id.as_deref(),
                &content,
                project_id,
                agent_id,
                session_id,
                domain_session_id,
            )
            .await
        }

        Command::ChatCancel {
            project_id,
            agent_id,
        } => chat::handle_chat_cancel(&ctx, project_id, agent_id).await,

        Command::ChatSessionDelete {
            project_id,
            agent_id,
            session_id,
        } => chat::handle_session_delete(&ctx, project_id, agent_id, session_id).await,

        Command::ChatDomainEnter {
            project_id,
            agent_id,
            domain_command,
            session_id,
        } => {
            domain::handle_domain_enter(&ctx, project_id, agent_id, &domain_command, session_id)
                .await
        }

        Command::ChatDomainExit {
            project_id,
            agent_id,
            domain_session_id,
        } => domain::handle_domain_exit(&ctx, project_id, agent_id, domain_session_id).await,

        Command::TaskExecute {
            task_id,
            project_id,
            routine_id,
            assigned_agent_id,
            execution_run_id,
            title,
            description,
            slug,
            acceptance_criteria,
            tags,
            status,
            priority,
            task_type,
            complexity,
        } => {
            task::handle_task_execute(
                &ctx,
                task_id,
                project_id,
                routine_id,
                assigned_agent_id,
                execution_run_id,
                &title,
                description.as_deref().unwrap_or(""),
                slug.as_deref(),
                acceptance_criteria.as_deref(),
                &tags,
                status.as_deref(),
                priority.as_deref(),
                task_type.as_deref(),
                complexity.as_deref(),
            )
            .await
        }

        Command::ExecutionCancel { execution_run_id } => {
            task::handle_execution_cancel(&ctx, execution_run_id).await
        }

        Command::ExecutionPause { execution_run_id } => {
            task::handle_execution_pause(&ctx, execution_run_id).await
        }

        Command::ExecutionResume { execution_run_id } => {
            task::handle_execution_resume(&ctx, execution_run_id).await
        }

        Command::CronEnable {
            assignment_id,
            routine_id,
            project_id,
            schedule,
        } => cron::handle_cron_enable(&ctx, assignment_id, routine_id, project_id, &schedule).await,

        Command::CronDisable { assignment_id } => {
            cron::handle_cron_disable(&ctx, assignment_id).await
        }

        Command::CronTrigger {
            routine_id,
            project_id,
            ..
        } => cron::handle_cron_trigger(&ctx, routine_id, project_id).await,

        Command::RepoSync {
            project_id,
            repo_url,
        } => repo::handle_repo_sync(&ctx, project_id, &repo_url).await,

        Command::RepoUnsync { project_id } => repo::handle_repo_unsync(&ctx, project_id).await,

        Command::WorkerPing => {
            let _ = ctx.response_tx.send(Response::WorkerPong);
            Ok(())
        }

        Command::ManifestChanged {
            resource_type,
            resource_id,
            action,
            project_id,
            payload,
        } => {
            manifest::handle_manifest_changed(
                &ctx,
                resource_type,
                resource_id,
                action,
                project_id,
                payload,
            )
            .await
        }
    }
}
