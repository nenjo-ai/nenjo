//! Chat command handlers.

use anyhow::{Context, Result};
use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    ExecutionPhase, SessionKind, SessionRecord, SessionRefs, SessionStatus, SessionSummary,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo_events::{Response, StreamEvent};
use nenjo_models::ChatMessage;

use super::event_bridge::{
    agent_name, project_slug, summarize_stream_event, summarize_turn_event,
    turn_event_to_stream_event,
};
use crate::harness::execution_trace::ExecutionTraceRecorder;
use crate::harness::session::{
    apply_session_memory_scope, lease_for_status, read_json_blob, transition_session_state,
    update_session_status,
};
use crate::harness::{ActiveExecution, CommandContext, DomainSession, ExecutionKind};

fn chat_history_ref(project_slug: &str, agent_name: &str, session_id: Uuid) -> String {
    if project_slug.is_empty() {
        format!("chat_history/{}_{}.json", agent_name, session_id)
    } else {
        format!(
            "{project_slug}/chat_history/{}_{}.json",
            agent_name, session_id
        )
    }
}

fn chat_trace_ref(project_slug: &str, agent_name: &str, session_id: Uuid) -> String {
    if project_slug.is_empty() {
        format!(
            "chat_history/traces/{}_{}.trace.json",
            agent_name, session_id
        )
    } else {
        format!(
            "{project_slug}/chat_history/traces/{}_{}.trace.json",
            agent_name, session_id
        )
    }
}

fn chat_checkpoint_ref(project_slug: &str, agent_name: &str, session_id: Uuid) -> String {
    if project_slug.is_empty() {
        format!(
            "chat_history/checkpoints/{}_{}.checkpoint.json",
            agent_name, session_id
        )
    } else {
        format!(
            "{project_slug}/chat_history/checkpoints/{}_{}.checkpoint.json",
            agent_name, session_id
        )
    }
}

fn chat_memory_namespace(agent_name: &str, project_slug: &str) -> String {
    MemoryScope::new(
        agent_name,
        if project_slug.is_empty() {
            None
        } else {
            Some(project_slug)
        },
    )
    .project
}

struct ChatSessionUpsert {
    session_id: Uuid,
    project_id: Uuid,
    agent_id: Uuid,
    project_slug: String,
    agent_name: String,
    history_ref: String,
    trace_ref: String,
    checkpoint_ref: String,
    status: SessionStatus,
}

fn upsert_chat_session(ctx: &CommandContext, params: ChatSessionUpsert) {
    let ChatSessionUpsert {
        session_id,
        project_id,
        agent_id,
        project_slug,
        agent_name,
        history_ref,
        trace_ref,
        checkpoint_ref,
        status,
    } = params;
    let now = Utc::now();
    let mut record = ctx
        .session_store
        .get(session_id)
        .ok()
        .flatten()
        .unwrap_or(SessionRecord {
            session_id,
            kind: SessionKind::Chat,
            status,
            project_id: Some(project_id),
            agent_id: Some(agent_id),
            task_id: None,
            routine_id: None,
            execution_run_id: None,
            parent_session_id: None,
            version: 0,
            refs: SessionRefs::default(),
            lease: Default::default(),
            scheduler: None,
            domain: None,
            summary: SessionSummary::default(),
            created_at: now,
            updated_at: now,
            completed_at: None,
        });

    record.kind = SessionKind::Chat;
    record.status = status;
    record.project_id = Some(project_id);
    record.agent_id = Some(agent_id);
    record.version += 1;
    record.updated_at = now;
    record.refs.history_ref = Some(history_ref);
    record.refs.trace_ref = Some(trace_ref);
    record.refs.checkpoint_ref = Some(checkpoint_ref);
    record.refs.memory_namespace = Some(chat_memory_namespace(&agent_name, &project_slug));
    if matches!(
        status,
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
    ) {
        record.completed_at = Some(now);
    }
    record.lease = lease_for_status(
        &*ctx.session_coordinator,
        session_id,
        &ctx.worker_id,
        status,
        &record.lease,
    );
    let _ = ctx.session_store.put(&record);
}

async fn restore_domain_session(ctx: &CommandContext, session_id: Uuid) -> Result<bool> {
    let Some(persisted) = ctx.session_store.get(session_id)? else {
        return Ok(false);
    };
    if persisted.kind != SessionKind::Domain {
        return Ok(false);
    }
    let Some(domain) = persisted.domain else {
        return Ok(false);
    };
    let agent_id = persisted
        .agent_id
        .context("domain session missing agent_id")?;
    let project_id = persisted
        .project_id
        .context("domain session missing project_id")?;

    let provider = ctx.provider();
    let base_runner = provider.agent_by_id(agent_id).await?.build().await?;
    let domain_runner = base_runner.domain_expansion(&domain.domain_command).await?;

    let mut instance = domain_runner.instance().clone();
    if let Some(ref mut active_domain) = instance.prompt_context.active_domain {
        active_domain.session_id = persisted.session_id;
        active_domain.turn_number = domain.turn_number;
    }

    let restored_runner = nenjo::AgentRunner::from_instance(
        instance,
        domain_runner.memory().cloned(),
        domain_runner.memory_scope().cloned(),
    );

    ctx.domains.insert(
        persisted.session_id,
        DomainSession {
            runner: restored_runner,
            agent_id,
            project_id,
            domain_command: domain.domain_command,
            turn_number: domain.turn_number,
        },
    );

    Ok(true)
}

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
    let history_ref = chat_history_ref(&slug, &aname, session_id);
    let trace_ref = chat_trace_ref(&slug, &aname, session_id);
    let checkpoint_ref = chat_checkpoint_ref(&slug, &aname, session_id);
    upsert_chat_session(
        ctx,
        ChatSessionUpsert {
            session_id,
            project_id: effective_project_id,
            agent_id,
            project_slug: slug.clone(),
            agent_name: aname.clone(),
            history_ref: history_ref.clone(),
            trace_ref,
            checkpoint_ref: checkpoint_ref.clone(),
            status: SessionStatus::Active,
        },
    );
    let _ = transition_session_state(
        &*ctx.session_store,
        &*ctx.session_content,
        &*ctx.session_coordinator,
        session_id,
        &ctx.worker_id,
        Some(ExecutionPhase::CallingModel),
        SessionStatus::Active,
    );

    let history: Vec<ChatMessage> =
        read_json_blob(&*ctx.session_content, &history_ref)?.unwrap_or_default();
    let mut trace_recorder = ExecutionTraceRecorder::for_chat_with_store(
        &ctx.config.workspace_dir,
        &slug,
        &aname,
        agent_id,
        session_id,
        ctx.session_content.clone(),
    );

    info!(
        agent = %aname,
        agent_id = %agent_id,
        session = %session_id,
        domain_session = ?domain_session_id,
        history_len = history.len(),
        "Chat request received"
    );

    // Cancel any previous execution for this session
    if let Some((_, prev)) = ctx.executions.remove(&session_id) {
        prev.cancel.cancel();
    }

    // Use domain-expanded runner if in an active domain session
    let runner = if let Some(dsid) = domain_session_id {
        if !ctx.domains.contains_key(&dsid) {
            match restore_domain_session(ctx, dsid).await {
                Ok(true) => {
                    info!(%dsid, "Restored persisted domain session on demand");
                }
                Ok(false) => {}
                Err(e) => {
                    warn!(%dsid, error = %e, "Failed to restore persisted domain session");
                }
            }
        }

        match ctx.domains.get_mut(&dsid) {
            Some(mut session) => {
                session.turn_number += 1;
                if let Ok(Some(mut record)) = ctx.session_store.get(dsid) {
                    record.version += 1;
                    record.updated_at = Utc::now();
                    record.status = SessionStatus::Active;
                    if let Some(ref mut domain) = record.domain {
                        domain.turn_number = session.turn_number;
                    }
                    let _ = ctx.session_store.put(&record);
                }
                let instance = session.runner.instance().clone();
                nenjo::AgentRunner::from_instance(
                    instance,
                    session.runner.memory().cloned(),
                    session.runner.memory_scope().cloned(),
                )
            }
            None => {
                // Domain session still could not be restored, so it is genuinely stale.
                warn!(%dsid, "Domain session not found after restore attempt");
                let _ = update_session_status(
                    &*ctx.session_store,
                    &*ctx.session_coordinator,
                    dsid,
                    &ctx.worker_id,
                    SessionStatus::Failed,
                );
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
        apply_session_memory_scope(
            ctx.provider().agent_by_id(agent_id).await?,
            &*ctx.session_store,
            session_id,
        )
        .build()
        .await?
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
            registry_token: Uuid::new_v4(),
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
                        debug!(
                            event = %summarize_turn_event(&ev),
                            agent = %aname,
                            "Chat handler received turn event"
                        );
                        let _ = trace_recorder.record(&ev);
                        if let Some(se) = turn_event_to_stream_event(&ev, &aname) {
                            let se = match se {
                                StreamEvent::Done { final_output, .. } => StreamEvent::Done {
                                    final_output,
                                    project_id,
                                    agent_id: Some(agent_id),
                                    session_id: Some(session_id),
                                },
                                other => other,
                            };
                            debug!(
                                stream_event = %summarize_stream_event(&se),
                                agent = %aname,
                                "Chat handler sending stream event"
                            );
                            let _ = ctx.response_tx.send(Response::AgentResponse { payload: se });
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                info!(agent = %aname, session = %session_id, "Chat execution cancelled");
                handle.abort();
                let _ = trace_recorder.finalize_with_error("Cancelled");
                let _ = transition_session_state(
                    &*ctx.session_store,
                    &*ctx.session_content,
                    &*ctx.session_coordinator,
                    session_id,
                    &ctx.worker_id,
                    Some(ExecutionPhase::Finalizing),
                    SessionStatus::Cancelled,
                );
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
                let _ = crate::harness::session::write_json_blob(
                    &*ctx.session_content,
                    &history_ref,
                    &conversation,
                );
            }
        }
        let _ = transition_session_state(
            &*ctx.session_store,
            &*ctx.session_content,
            &*ctx.session_coordinator,
            session_id,
            &ctx.worker_id,
            Some(ExecutionPhase::Finalizing),
            SessionStatus::Completed,
        );
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
            let _ = update_session_status(
                &*ctx.session_store,
                &*ctx.session_coordinator,
                key,
                &ctx.worker_id,
                SessionStatus::Cancelled,
            );
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
    if let Ok(Some(record)) = ctx.session_store.get(session_id) {
        if let Some(history_ref) = record.refs.history_ref.as_deref() {
            let _ = ctx.session_content.delete_blob(history_ref);
        }
        if let Some(trace_ref) = record.refs.trace_ref.as_deref() {
            let _ = ctx.session_content.delete_blob(trace_ref);
        }
        if let Some(checkpoint_ref) = record.refs.checkpoint_ref.as_deref() {
            let _ = ctx.session_content.delete_blob(checkpoint_ref);
        }
    } else {
        let manifest = provider.manifest();
        let slug = project_slug(manifest, project_id);
        let name = agent_name(manifest, agent_id);
        let history_ref = chat_history_ref(&slug, &name, session_id);
        let _ = ctx.session_content.delete_blob(&history_ref);
    }
    let _ = ctx.session_store.delete(session_id);
    Ok(())
}
