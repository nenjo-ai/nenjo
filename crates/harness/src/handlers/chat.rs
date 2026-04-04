//! Chat command handlers.

use anyhow::{Context, Result};
use tracing::{info, warn};
use uuid::Uuid;

use nenjo_events::{Response, StreamEvent};
use nenjo_models::ChatMessage;

use super::event_bridge::{agent_name, project_slug, turn_event_to_stream_event};
use crate::harness::{ActiveExecution, CommandContext, ExecutionKind};

/// Handle a chat message — with cancellation and domain session support.
///
/// The execution handle is registered by `session_id` so `ChatCancel` can abort it.
pub async fn handle_chat(
    ctx: &CommandContext,
    message_id: Option<&str>,
    content: &str,
    project_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    session_id: Uuid,
    domain_session_id: Option<Uuid>,
) -> Result<()> {
    // Send delivery receipt immediately so the frontend knows we got the message.
    if let Some(mid) = message_id {
        let _ = ctx.response_tx.send(Response::DeliveryReceipt {
            message_id: mid.to_string(),
        });
    }

    let provider = ctx.provider();
    let manifest = provider.manifest();
    let effective_project_id = project_id.unwrap_or(Uuid::nil());

    let resolved_agent_id =
        agent_id.or_else(|| manifest.agents.iter().find(|a| a.is_system).map(|a| a.id));
    let agent_id = resolved_agent_id.context("No agent found for chat")?;
    let slug = project_slug(manifest, effective_project_id);
    let aname = agent_name(manifest, agent_id);

    let history: Vec<ChatMessage> = ctx
        .chat_history
        .read(&slug, &aname, session_id)
        .unwrap_or_default();

    info!(
        agent = %aname,
        session = %session_id,
        domain_session = ?domain_session_id,
        history_len = history.len(),
        "Routing chat"
    );

    // Cancel any previous execution for this session
    if let Some((_, prev)) = ctx.executions.remove(&session_id) {
        prev.cancel.cancel();
    }

    // Use domain-expanded runner if in an active domain session
    let runner = if let Some(dsid) = domain_session_id {
        match ctx.domains.get_mut(&dsid) {
            Some(mut session) => {
                session.turn_number += 1;
                let instance = session.runner.instance().clone();
                nenjo::AgentRunner::from_instance(
                    instance,
                    session.runner.memory().cloned(),
                    session.runner.memory_scope().cloned(),
                )
            }
            None => {
                // Domain session not found — likely from a previous harness run.
                // Notify the frontend to exit the stale domain so it can re-enter.
                warn!(%dsid, "Domain session not found (stale from previous run?)");
                let _ = ctx.response_tx.send(Response::AgentResponse {
                    payload: StreamEvent::DomainExited {
                        session_id: dsid,
                        artifact_id: None,
                        document_id: None,
                    },
                });
                let _ = ctx.response_tx.send(Response::AgentResponse {
                    payload: StreamEvent::Error {
                        message: "Domain session expired. Please re-enter the domain.".into(),
                    },
                });
                return Ok(());
            }
        }
    } else {
        ctx.provider().agent_by_id(agent_id).await?.build().await?
    };

    // Start streaming execution
    let mut handle = runner.chat_with_history_stream(content, history).await?;

    // Register the execution handle for cancellation (keyed by session_id).
    // We need to move the handle into the registry but also keep streaming from it.
    // Solution: stream events in this task, but register a separate abort mechanism.
    // Since ExecutionHandle::abort() uses JoinHandle::abort() which is &self-safe
    // via the inner Arc, we can't split it. Instead, we'll check the registry
    // periodically. Actually, the simplest approach: don't register the handle itself,
    // just abort via the tokio JoinHandle. But ExecutionHandle owns the JoinHandle.
    //
    // Better approach: use a CancellationToken for the select loop, and abort
    // the handle when the token fires.
    let cancel = tokio_util::sync::CancellationToken::new();
    ctx.executions.insert(
        session_id,
        ActiveExecution {
            kind: ExecutionKind::Chat,
            execution_run_id: None,
            cancel: cancel.clone(),
            pause: None,
        },
    );

    // Stream with cancellation
    loop {
        tokio::select! {
            event = handle.recv() => {
                match event {
                    Some(ev) => {
                        if let Some(se) = turn_event_to_stream_event(&ev, &aname) {
                            let _ = ctx.response_tx.send(Response::AgentResponse { payload: se });
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                info!(agent = %aname, session = %session_id, "Chat execution cancelled");
                handle.abort();
                let _ = ctx.response_tx.send(Response::AgentResponse {
                    payload: StreamEvent::Error { message: "Cancelled".to_string() },
                });
                break;
            }
        }
    }

    // Unregister
    ctx.executions.remove(&session_id);

    // Persist history if not cancelled.
    // Note: we don't send Done here — the turn loop already emits
    // TurnEvent::Done which gets forwarded to the frontend via the
    // event bridge in the streaming loop above.
    if !cancel.is_cancelled() {
        let output = handle.output().await?;
        if !output.messages.is_empty() {
            // Strip system/developer messages — they are rebuilt each turn from
            // the agent's prompt config. Only persist the conversation turns.
            let conversation: Vec<_> = output
                .messages
                .iter()
                .filter(|m| m.role != "system" && m.role != "developer")
                .cloned()
                .collect();
            if !conversation.is_empty() {
                let max_turns = ctx.provider().agent_config().max_history_messages;
                let _ = ctx
                    .chat_history
                    .write(&slug, &aname, session_id, &conversation, max_turns);
            }
        }
    }

    Ok(())
}

/// Cancel in-flight chat executions.
///
/// `ChatCancel` carries `project_id` and optionally `agent_id` but not `session_id`.
/// We scan the execution registry and cancel all matching entries.
pub async fn handle_chat_cancel(
    ctx: &CommandContext,
    project_id: Uuid,
    agent_id: Option<Uuid>,
) -> Result<()> {
    // Collect chat-only keys to cancel.
    let keys_to_cancel: Vec<Uuid> = ctx
        .executions
        .iter()
        .filter(|entry| entry.value().kind == ExecutionKind::Chat)
        .map(|entry| *entry.key())
        .collect();

    let mut cancelled = 0;
    for key in keys_to_cancel {
        if let Some((_, exec)) = ctx.executions.remove(&key) {
            exec.cancel.cancel();
            cancelled += 1;
        }
    }

    if cancelled > 0 {
        info!(agent_id = ?agent_id, %project_id, cancelled, "Cancelled chat executions");
    }
    Ok(())
}

/// Delete a chat session's local history.
pub async fn handle_session_delete(
    ctx: &CommandContext,
    project_id: Uuid,
    agent_id: Uuid,
    session_id: Uuid,
) -> Result<()> {
    let provider = ctx.provider();
    let manifest = provider.manifest();
    let slug = project_slug(manifest, project_id);
    let name = agent_name(manifest, agent_id);
    let _ = ctx.chat_history.delete(&slug, &name, session_id);
    Ok(())
}
