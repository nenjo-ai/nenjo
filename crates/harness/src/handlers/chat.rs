//! Chat command handlers.

use anyhow::{Context, Result};
use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    ChatSessionUpsert, DomainSessionUpsert, DomainState, ExecutionPhase, SessionKind,
    SessionStatus, SessionTranscriptAppend, SessionTranscriptEventPayload, SessionTransition,
    TranscriptQuery, TranscriptState,
};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use nenjo_events::{DomainActivation, Response, StreamEvent};
use nenjo_models::ChatMessage;
use serde_json::json;

use super::ResponseSender;
use crate::event_bridge::{
    agent_name, project_slug, summarize_stream_event, summarize_turn_event,
    turn_event_to_stream_event,
};
use crate::execution_trace::{
    ExecutionTraceRuntime, ExecutionTraceTarget, ExecutionTraceWriter, TraceAgent,
};
use crate::session::{
    TurnEventContext, chat_message_to_transcript, replay_transcript_history,
    session_runtime_events_from_turn_event, spawn_session_events,
};
use crate::{ActiveExecution, ExecutionKind, Harness, HarnessProvider};

#[derive(Clone)]
pub struct ChatCommandContext<S> {
    pub response_sink: S,
    pub worker_id: String,
}

pub struct ChatRequest<'a> {
    pub message_id: Option<&'a str>,
    pub content: &'a str,
    pub project_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub session_id: Uuid,
    pub domain_session_id: Option<Uuid>,
    pub domain_activation: Option<DomainActivation>,
}

fn chat_trace_ref<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    project_slug: &str,
    agent_name: &str,
    agent_id: Uuid,
    session_id: Uuid,
) -> Option<String>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    harness.execution_traces().trace_ref(
        &ExecutionTraceTarget::Chat {
            session_id,
            project_slug: project_slug.to_string(),
        },
        &TraceAgent {
            id: agent_id,
            name: agent_name.to_string(),
        },
    )
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

fn domain_name_for_command(manifest: &nenjo::manifest::Manifest, domain_command: &str) -> String {
    manifest
        .domains
        .iter()
        .find(|domain| domain.command == domain_command)
        .map(|domain| domain.name.clone())
        .unwrap_or_else(|| domain_command.to_string())
}

struct ChatSessionRecord {
    session_id: Uuid,
    project_id: Option<Uuid>,
    agent_id: Uuid,
    project_slug: String,
    agent_name: String,
    trace_ref: Option<String>,
    status: SessionStatus,
}

#[derive(Clone, Copy)]
enum SessionUpsertMode {
    Await,
    Spawn,
}

async fn upsert_chat_session_record<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    params: ChatSessionRecord,
    mode: SessionUpsertMode,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let ChatSessionRecord {
        session_id,
        project_id,
        agent_id,
        project_slug,
        agent_name,
        trace_ref,
        status,
    } = params;
    let memory_namespace = chat_memory_namespace(&agent_name, &project_slug);
    let upsert = ChatSessionUpsert {
        session_id,
        status,
        project_id,
        agent_id,
        memory_namespace: Some(memory_namespace.clone()),
        trace_ref,
        metadata: json!({
            "source": "worker_chat",
            "agent_name": agent_name,
            "project_slug": project_slug,
        }),
    };

    match mode {
        SessionUpsertMode::Await => {
            if let Err(error) = harness.upsert_chat_session(upsert).await {
                warn!(
                    error = %error,
                    session_id = %session_id,
                    "Failed to upsert chat session"
                );
            }
        }
        SessionUpsertMode::Spawn => {
            let harness = harness.clone();
            tokio::spawn(async move {
                if let Err(error) = harness.upsert_chat_session(upsert).await {
                    warn!(
                        error = %error,
                        session_id = %session_id,
                        "Failed to upsert chat session"
                    );
                }
            });
        }
    }
}

async fn restore_domain_session<P, SessionRt, TraceRt, StoreRt, McpRt, S>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    _ctx: &ChatCommandContext<S>,
    session_id: Uuid,
) -> Result<bool>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
{
    let Some(persisted) = harness.get_session(session_id).await? else {
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
    let project_id = persisted.project_id.unwrap_or_else(Uuid::nil);

    let session = harness
        .rebuild_domain_session(
            persisted.session_id,
            agent_id,
            project_id,
            &domain.domain_command,
        )
        .await?;

    harness.domains().insert(persisted.session_id, session);

    Ok(true)
}

struct ActivateDomainForChat<'a> {
    worker_id: &'a str,
    project_id: Uuid,
    agent_id: Uuid,
    domain_command: &'a str,
    domain_session_id: Uuid,
    agent_name: &'a str,
    project_slug: &'a str,
}

async fn activate_domain_for_chat<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    params: ActivateDomainForChat<'_>,
) -> Result<String>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let ActivateDomainForChat {
        worker_id,
        project_id,
        agent_id,
        domain_command,
        domain_session_id,
        agent_name,
        project_slug,
    } = params;

    // Persist the domain session metadata here, but let the chat execution path
    // build the domain-expanded runner once with the final chat session id.
    let domain_name = domain_name_for_command(harness.provider().manifest(), domain_command);

    let _ = harness
        .upsert_domain_session(DomainSessionUpsert {
            session_id: domain_session_id,
            status: SessionStatus::Active,
            project_id: if project_id.is_nil() {
                None
            } else {
                Some(project_id)
            },
            agent_id,
            worker_id: worker_id.to_string(),
            memory_namespace: Some(chat_memory_namespace(agent_name, project_slug)),
            domain: Some(DomainState {
                domain_command: domain_command.to_string(),
            }),
        })
        .await;

    Ok(domain_name)
}

async fn active_domain_command<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    domain_session_id: Uuid,
) -> Option<String>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    if let Some(session) = harness.domains().get(&domain_session_id) {
        return Some(session.domain_command.clone());
    }

    harness
        .get_session(domain_session_id)
        .await
        .ok()
        .flatten()
        .and_then(|record| record.domain.map(|domain| domain.domain_command))
}

/// Handle a chat message — with cancellation and domain session support.
///
/// The execution handle is registered by `session_id` so `ChatCancel` can abort it.
impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    pub async fn handle_chat<S>(
        &self,
        ctx: &ChatCommandContext<S>,
        request: ChatRequest<'_>,
    ) -> Result<()>
    where
        S: ResponseSender + Clone + 'static,
    {
        handle_chat(self, ctx, request).await
    }

    pub async fn handle_chat_cancel<S>(
        &self,
        ctx: &ChatCommandContext<S>,
        project_id: Uuid,
        agent_id: Option<Uuid>,
    ) -> Result<()>
    where
        S: ResponseSender,
    {
        handle_chat_cancel(self, ctx, project_id, agent_id).await
    }

    pub async fn handle_session_delete<S>(
        &self,
        ctx: &ChatCommandContext<S>,
        project_id: Uuid,
        agent_id: Uuid,
        session_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
    {
        handle_session_delete(self, ctx, project_id, agent_id, session_id).await
    }
}

async fn handle_chat<P, SessionRt, TraceRt, StoreRt, McpRt, S>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    ctx: &ChatCommandContext<S>,
    request: ChatRequest<'_>,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender + Clone + 'static,
{
    let ChatRequest {
        message_id: _,
        content,
        project_id,
        agent_id,
        session_id,
        domain_session_id,
        domain_activation,
    } = request;

    let provider = harness.provider();
    let manifest = provider.manifest();
    let effective_project_id = project_id.unwrap_or(Uuid::nil());
    let effective_content = if content.trim().is_empty() {
        match &domain_activation {
            Some(activation) => activation.domain_command.clone(),
            None => content.to_string(),
        }
    } else {
        content.to_string()
    };
    let effective_domain_session_id = domain_activation
        .as_ref()
        .map(|activation| activation.domain_session_id)
        .or(domain_session_id);
    let turn_id = Uuid::new_v4();

    let agent_id = agent_id.context("No agent_id provided for chat")?;
    let slug = project_slug(manifest, effective_project_id);
    let aname = agent_name(manifest, agent_id);
    let trace_target = ExecutionTraceTarget::Chat {
        session_id,
        project_slug: slug.clone(),
    };
    let trace_agent = TraceAgent {
        id: agent_id,
        name: aname.clone(),
    };
    let trace_ref = harness
        .execution_traces()
        .trace_ref(&trace_target, &trace_agent);
    let activated_domain_name = if let Some(activation) = &domain_activation {
        let domain_name = activate_domain_for_chat(
            harness,
            ActivateDomainForChat {
                worker_id: &ctx.worker_id,
                project_id: effective_project_id,
                agent_id,
                domain_command: &activation.domain_command,
                domain_session_id: activation.domain_session_id,
                agent_name: &aname,
                project_slug: &slug,
            },
        )
        .await?;
        let _ = harness
            .append_transcript(SessionTranscriptAppend {
                session_id,
                turn_id: Some(turn_id),
                payload: SessionTranscriptEventPayload::DomainActivated {
                    domain_session_id: activation.domain_session_id,
                    domain_command: activation.domain_command.clone(),
                    domain_name: domain_name.clone(),
                    agent_id,
                    user_message_preview: (!effective_content.trim().is_empty())
                        .then(|| effective_content.clone()),
                },
                transcript_state: TranscriptState::MidTurn,
            })
            .await?;
        let _ = ctx.response_sink.send(Response::AgentResponse {
            session_id: Some(session_id),
            payload: StreamEvent::DomainEntered {
                session_id: activation.domain_session_id,
                domain_name: domain_name.clone(),
            },
        });
        Some(domain_name)
    } else {
        None
    };
    upsert_chat_session_record(
        harness,
        ChatSessionRecord {
            session_id,
            project_id,
            agent_id,
            project_slug: slug.clone(),
            agent_name: aname.clone(),
            trace_ref,
            status: SessionStatus::Active,
        },
        SessionUpsertMode::Await,
    )
    .await;
    let _ = harness
        .transition_session(SessionTransition {
            session_id,
            worker_id: ctx.worker_id.clone(),
            phase: Some(ExecutionPhase::CallingModel),
            status: SessionStatus::Active,
        })
        .await;
    let mut transcript_events = harness
        .read_transcript(session_id, TranscriptQuery::default())
        .await?;
    if let Some(dsid) = effective_domain_session_id
        && !transcript_events.iter().any(|event| {
            matches!(
                &event.payload,
                SessionTranscriptEventPayload::DomainActivated {
                    domain_session_id,
                    ..
                } if *domain_session_id == dsid
            )
        })
        && let Some(domain_command) = active_domain_command(harness, dsid).await
    {
        let domain_name = domain_name_for_command(manifest, &domain_command);
        if let Some(event) = harness
            .append_transcript(SessionTranscriptAppend {
                session_id,
                turn_id: Some(turn_id),
                payload: SessionTranscriptEventPayload::DomainActivated {
                    domain_session_id: dsid,
                    domain_command,
                    domain_name,
                    agent_id,
                    user_message_preview: None,
                },
                transcript_state: TranscriptState::MidTurn,
            })
            .await?
        {
            transcript_events.push(event);
        }
    }
    let history: Vec<ChatMessage> = replay_transcript_history(&transcript_events);
    let _ = harness
        .append_transcript(SessionTranscriptAppend {
            session_id,
            turn_id: Some(turn_id),
            payload: SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(&ChatMessage::user(effective_content.clone())),
            },
            transcript_state: TranscriptState::MidTurn,
        })
        .await?;
    let trace_recorder = harness.execution_traces().writer(trace_target, trace_agent);

    info!(
        agent = %aname,
        agent_id = %agent_id,
        session = %session_id,
        domain_session = ?effective_domain_session_id,
        activated_domain = ?activated_domain_name,
        history_len = history.len(),
        "Chat request received"
    );

    // Cancel any previous execution for this session
    if let Some((_, prev)) = harness.executions().remove(&session_id) {
        prev.cancel.cancel();
    }

    // Use domain-expanded runner if in an active domain session
    let runner = if let Some(dsid) = effective_domain_session_id {
        if !harness.domains().contains_key(&dsid) {
            match restore_domain_session(harness, ctx, dsid).await {
                Ok(true) => {
                    info!(%dsid, "Restored persisted domain session on demand");
                }
                Ok(false) => {}
                Err(e) => {
                    warn!(%dsid, error = %e, "Failed to restore persisted domain session");
                }
            }
        }

        match harness.domains().get_mut(&dsid) {
            Some(session) => {
                let agent_id = session.agent_id;
                let project_id = session.project_id;
                let domain_command = session.domain_command.clone();
                drop(session);
                let rebuilt = harness
                    .rebuild_domain_session(dsid, agent_id, project_id, &domain_command)
                    .await?;
                let mut instance = rebuilt.runner.instance().clone();
                instance.set_active_domain_session_id(session_id);
                let active_domain_name = instance
                    .prompt_context()
                    .active_domain
                    .as_ref()
                    .map(|domain| domain.domain_name.clone());
                let runner = nenjo::AgentRunner::from_instance(
                    instance,
                    rebuilt.runner.memory().cloned(),
                    rebuilt.runner.memory_scope().cloned(),
                );
                debug!(
                    domain_session_id = %dsid,
                    chat_session_id = %session_id,
                    active_domain = ?active_domain_name,
                    "Using domain-expanded chat runner"
                );
                harness.domains().insert(dsid, rebuilt);
                runner
            }
            None => {
                // Domain session still could not be restored, so it is genuinely stale.
                warn!(%dsid, "Domain session not found after restore attempt");
                let _ = harness
                    .transition_session(SessionTransition {
                        session_id: dsid,
                        worker_id: ctx.worker_id.clone(),
                        phase: None,
                        status: SessionStatus::Failed,
                    })
                    .await;
                upsert_chat_session_record(
                    harness,
                    ChatSessionRecord {
                        session_id,
                        project_id,
                        agent_id,
                        project_slug: slug.clone(),
                        agent_name: aname.clone(),
                        trace_ref: chat_trace_ref(harness, &slug, &aname, agent_id, session_id),
                        status: SessionStatus::Failed,
                    },
                    SessionUpsertMode::Spawn,
                )
                .await;
                let _ = ctx.response_sink.send(Response::AgentResponse {
                    session_id: Some(session_id),
                    payload: StreamEvent::DomainExited {
                        session_id: dsid,
                        artifact_id: None,
                        document_id: None,
                    },
                });
                let _ = ctx.response_sink.send(Response::AgentResponse {
                    session_id: Some(session_id),
                    payload: StreamEvent::Error {
                        message: "Domain session expired. Please re-enter the domain.".into(),
                        payload: None,
                        encrypted_payload: None,
                    },
                });
                return Ok(());
            }
        }
    } else {
        let mut builder = provider.build_agent_by_id(agent_id).await?;
        if let Some(project_id) = project_id {
            if let Some(project) = manifest
                .projects
                .iter()
                .find(|project| project.id == project_id)
            {
                builder = builder.with_project_context(project);
            } else {
                warn!(%project_id, %agent_id, "Project not found in manifest for chat session");
            }
        }
        match harness
            .session_memory_namespace(session_id)
            .await?
            .and_then(|namespace| MemoryScope::from_namespace(&namespace))
        {
            Some(scope) => builder.with_memory_scope(scope),
            None => builder,
        }
        .build()
        .await?
    };

    // Start streaming execution
    let mut handle = runner
        .chat_with_history_stream(&effective_content, history)
        .await?;

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
    let registry_token = Uuid::new_v4();
    harness.executions().insert(
        session_id,
        ActiveExecution {
            kind: ExecutionKind::Chat,
            registry_token,
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
                        trace_recorder.record(&ev);
                        let session_event_context = TurnEventContext {
                            session_id,
                            turn_id: Some(turn_id),
                            agent_id: Some(agent_id),
                            agent_name: Some(aname.clone()),
                            recorded_at: Utc::now(),
                        };
                        spawn_session_events(
                            harness,
                            session_runtime_events_from_turn_event(&session_event_context, &ev),
                            session_id,
                        );
                        if let Some(se) = turn_event_to_stream_event(&ev, &aname) {
                            trace!(
                                stream_event = %summarize_stream_event(&se),
                                agent = %aname,
                                "Chat handler produced stream event"
                            );
                            let _ = ctx.response_sink.send(Response::AgentResponse {
                                session_id: Some(session_id),
                                payload: se,
                            });
                        }
                    }
                    None => break,
                }
            }
            _ = cancel.cancelled() => {
                warn!(agent = %aname, session = %session_id, "Chat execution cancelled");
                handle.abort();
                trace_recorder.finalize_with_error("Cancelled");
                let is_current_execution = harness
                    .executions()
                    .get(&session_id)
                    .is_some_and(|entry| entry.registry_token == registry_token);
                if is_current_execution {
                    let _ = harness.transition_session(SessionTransition {
                        session_id,
                        worker_id: ctx.worker_id.clone(),
                        phase: Some(ExecutionPhase::Finalizing),
                        status: SessionStatus::Cancelled,
                    }).await;
                    upsert_chat_session_record(
                        harness,
                        ChatSessionRecord {
                            session_id,
                            project_id,
                            agent_id,
                            project_slug: slug.clone(),
                            agent_name: aname.clone(),
                            trace_ref: chat_trace_ref(harness, &slug, &aname, agent_id, session_id),
                            status: SessionStatus::Cancelled,
                        },
                        SessionUpsertMode::Spawn,
                    )
                    .await;
                    let _ = ctx.response_sink.send(Response::AgentResponse {
                        session_id: Some(session_id),
                        payload: StreamEvent::Error {
                            message: "Cancelled".to_string(),
                            payload: None,
                            encrypted_payload: None,
                        },
                    });
                }
                break;
            }
        }
    }
    trace_recorder.finish().await;

    // Unregister
    if harness
        .executions()
        .get(&session_id)
        .is_some_and(|entry| entry.registry_token == registry_token)
    {
        harness.executions().remove(&session_id);
    }

    if !cancel.is_cancelled() {
        let _ = handle.output().await?;
        let _ = harness
            .transition_session(SessionTransition {
                session_id,
                worker_id: ctx.worker_id.clone(),
                phase: Some(ExecutionPhase::Finalizing),
                status: SessionStatus::Completed,
            })
            .await;
        upsert_chat_session_record(
            harness,
            ChatSessionRecord {
                session_id,
                project_id,
                agent_id,
                project_slug: slug.clone(),
                agent_name: aname.clone(),
                trace_ref: chat_trace_ref(harness, &slug, &aname, agent_id, session_id),
                status: SessionStatus::Completed,
            },
            SessionUpsertMode::Spawn,
        )
        .await;
    }

    Ok(())
}

/// Cancel in-flight chat executions.
///
/// `ChatCancel` carries `project_id` and optionally `agent_id` but not `session_id`.
/// We scan the execution registry and cancel all matching entries.
async fn handle_chat_cancel<P, SessionRt, TraceRt, StoreRt, McpRt, S>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    ctx: &ChatCommandContext<S>,
    project_id: Uuid,
    agent_id: Option<Uuid>,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
{
    // Collect chat-only keys to cancel.
    let keys_to_cancel: Vec<Uuid> = harness
        .executions()
        .iter()
        .filter(|entry| entry.value().kind == ExecutionKind::Chat)
        .map(|entry| *entry.key())
        .collect();

    let mut cancelled = 0;
    for key in keys_to_cancel {
        if let Some((_, exec)) = harness.executions().remove(&key) {
            exec.cancel.cancel();
            let _ = harness
                .append_transcript(SessionTranscriptAppend {
                    session_id: key,
                    turn_id: None,
                    payload: SessionTranscriptEventPayload::TurnInterrupted {
                        reason: "cancelled by user".to_string(),
                    },
                    transcript_state: TranscriptState::Clean,
                })
                .await;
            let _ = harness
                .transition_session(SessionTransition {
                    session_id: key,
                    worker_id: ctx.worker_id.clone(),
                    phase: None,
                    status: SessionStatus::Cancelled,
                })
                .await;
            cancelled += 1;
        }
    }

    if cancelled > 0 {
        info!(agent_id = ?agent_id, %project_id, cancelled, "Cancelled chat executions");
    }
    Ok(())
}

/// Delete a chat session's local history.
async fn handle_session_delete<P, SessionRt, TraceRt, StoreRt, McpRt, S>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    _ctx: &ChatCommandContext<S>,
    _project_id: Uuid,
    _agent_id: Uuid,
    session_id: Uuid,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
    S: ResponseSender,
{
    let _ = harness.delete_session(session_id).await;
    Ok(())
}
