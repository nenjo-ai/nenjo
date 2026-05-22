//! Platform-free chat execution orchestration.

use anyhow::{Result, anyhow};
use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo_models::ChatMessage;
use nenjo_sessions::{
    ChatSessionUpsert, DomainSessionUpsert, DomainState, ExecutionPhase, SessionKind,
    SessionStatus, SessionTranscriptAppend, SessionTranscriptEventPayload, SessionTransition,
    TranscriptQuery, TranscriptState,
};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::events::HarnessEvent;
use crate::execution_context::{agent_name, project_slug, summarize_turn_event};
use crate::handle::HarnessExecutionHandle;
use crate::registry::{ActiveExecution, ExecutionKind};
use crate::request::{AgentRef, ChatRequest};
use crate::session::{
    TurnEventContext, chat_message_to_transcript, replay_transcript_history,
    session_runtime_events_from_turn_event,
};
use crate::{Harness, ProviderRuntime};

pub(crate) async fn chat_stream<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: ChatRequest,
) -> crate::Result<HarnessExecutionHandle>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let mut prepared = prepare_chat_execution(harness, request, events_tx).await?;
    let runner = build_chat_runner(harness, &prepared).await?;
    let history = std::mem::take(&mut prepared.history);
    let handle = runner
        .chat_with_history_stream(&prepared.effective_content, history)
        .await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let registry_token = Uuid::new_v4();

    if let Some((_, previous)) = harness.executions().remove(&prepared.session_id) {
        previous.cancel.cancel();
    }
    harness.executions().insert(
        prepared.session_id,
        ActiveExecution {
            kind: ExecutionKind::Chat,
            registry_token,
            execution_run_id: None,
            cancel: cancel.clone(),
            pause: None,
        },
    );

    let join = spawn_chat_execution(
        harness.clone(),
        handle,
        prepared,
        cancel.clone(),
        registry_token,
    );

    Ok(HarnessExecutionHandle::new(events_rx, join, cancel))
}

struct PreparedChatExecution {
    session_id: Uuid,
    turn_id: Uuid,
    agent: AgentRef,
    agent_id: Uuid,
    agent_name: String,
    project_id: Option<Uuid>,
    project_slug: String,
    effective_content: String,
    effective_domain_session_id: Option<Uuid>,
    history: Vec<ChatMessage>,
    events_tx: mpsc::UnboundedSender<HarnessEvent>,
    worker_id: String,
}

async fn prepare_chat_execution<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: ChatRequest,
    events_tx: mpsc::UnboundedSender<HarnessEvent>,
) -> crate::Result<PreparedChatExecution>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let ChatRequest {
        session_id,
        agent,
        message,
        project_id,
        domain_session_id,
        domain_activation,
    } = request;

    let sessions = harness.sessions();
    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
    let effective_project_id = project_id.unwrap_or(Uuid::nil());
    let effective_content = if message.trim().is_empty() {
        match &domain_activation {
            Some(activation) => activation.domain_command.clone(),
            None => message.clone(),
        }
    } else {
        message.clone()
    };
    let effective_domain_session_id = domain_activation
        .as_ref()
        .map(|activation| activation.domain_session_id)
        .or(domain_session_id);
    let turn_id = Uuid::new_v4();
    let agent_id = resolve_agent_id(provider.as_ref(), &agent)?;
    let slug = project_slug(&manifest, effective_project_id);
    let aname = agent_name(&manifest, agent_id);
    let worker_id = "harness".to_string();

    if let Some(activation) = &domain_activation {
        let domain_name = activate_domain_for_chat(
            harness,
            ActivateDomainForChat {
                worker_id: &worker_id,
                project_id: effective_project_id,
                agent_id,
                domain_command: &activation.domain_command,
                domain_session_id: activation.domain_session_id,
                agent_name: &aname,
                project_slug: &slug,
            },
        )
        .await?;
        let _ = sessions
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
        let _ = events_tx.send(HarnessEvent::DomainEntered {
            session_id: activation.domain_session_id,
            domain_name,
        });
    }

    upsert_chat_session_record(
        harness,
        ChatSessionRecord {
            session_id,
            project_id,
            agent_id,
            project_slug: slug.clone(),
            agent_name: aname.clone(),
            status: SessionStatus::Active,
        },
        SessionUpsertMode::Await,
    )
    .await;
    let _ = sessions
        .transition(SessionTransition {
            session_id,
            worker_id: worker_id.clone(),
            phase: Some(ExecutionPhase::CallingModel),
            status: SessionStatus::Active,
        })
        .await;
    let mut transcript_events = sessions
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
        let domain_name = domain_name_for_command(&manifest, &domain_command);
        if let Some(event) = sessions
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
    let _ = sessions
        .append_transcript(SessionTranscriptAppend {
            session_id,
            turn_id: Some(turn_id),
            payload: SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(&ChatMessage::user(effective_content.clone())),
            },
            transcript_state: TranscriptState::MidTurn,
        })
        .await?;
    info!(
        agent = %aname,
        agent_id = %agent_id,
        session = %session_id,
        domain_session = ?effective_domain_session_id,
        history_len = history.len(),
        "Harness chat request received"
    );

    Ok(PreparedChatExecution {
        session_id,
        turn_id,
        agent,
        agent_id,
        agent_name: aname,
        project_id,
        project_slug: slug,
        effective_content,
        effective_domain_session_id,
        history,
        events_tx,
        worker_id,
    })
}

async fn build_chat_runner<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    prepared: &PreparedChatExecution,
) -> crate::Result<nenjo::AgentRunner<P>>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();

    let runner = if let Some(dsid) = prepared.effective_domain_session_id {
        if !harness.domains().contains_key(&dsid) {
            match restore_domain_session(harness, dsid).await {
                Ok(true) => {
                    info!(%dsid, "Restored persisted domain session on demand");
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(%dsid, error = %error, "Failed to restore persisted domain session");
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
                instance.set_active_domain_session_id(prepared.session_id);
                let runner = nenjo::AgentRunner::from_instance(
                    instance,
                    rebuilt.runner.memory().cloned(),
                    rebuilt.runner.memory_scope().cloned(),
                );
                harness.domains().insert(dsid, rebuilt);
                runner
            }
            None => {
                let _ = harness
                    .sessions()
                    .transition(SessionTransition {
                        session_id: dsid,
                        worker_id: prepared.worker_id.clone(),
                        phase: None,
                        status: SessionStatus::Failed,
                    })
                    .await;
                upsert_chat_session_record(
                    harness,
                    ChatSessionRecord {
                        session_id: prepared.session_id,
                        project_id: prepared.project_id,
                        agent_id: prepared.agent_id,
                        project_slug: prepared.project_slug.clone(),
                        agent_name: prepared.agent_name.clone(),
                        status: SessionStatus::Failed,
                    },
                    SessionUpsertMode::Spawn,
                )
                .await;
                return Err(crate::HarnessError::InvalidCommand(
                    "Domain session expired. Please re-enter the domain.".to_string(),
                ));
            }
        }
    } else {
        let mut builder = match &prepared.agent {
            AgentRef::Id(agent_id) => provider
                .build_agent_by_id(*agent_id)
                .await
                .map_err(anyhow::Error::from)?,
            AgentRef::Name(agent_name) => provider
                .build_agent_by_name(agent_name)
                .await
                .map_err(anyhow::Error::from)?,
        };
        if let Some(project_id) = prepared.project_id {
            if let Some(project) = manifest
                .projects
                .iter()
                .find(|project| project.id == project_id)
            {
                builder = builder.with_project_context(project);
            } else {
                warn!(%project_id, agent_id = %prepared.agent_id, "Project not found in manifest for chat session");
            }
        }
        match harness
            .sessions()
            .memory_namespace(prepared.session_id)
            .await?
            .and_then(|namespace| MemoryScope::from_namespace(&namespace))
        {
            Some(scope) => builder.with_memory_scope(scope),
            None => builder,
        }
        .build()
        .await
        .map_err(anyhow::Error::from)?
    };

    Ok(runner)
}

fn spawn_chat_execution<P, SessionRt>(
    harness: Harness<P, SessionRt>,
    mut handle: nenjo::ExecutionHandle,
    prepared: PreparedChatExecution,
    cancel: tokio_util::sync::CancellationToken,
    registry_token: Uuid,
) -> tokio::task::JoinHandle<crate::Result<nenjo::TurnOutput>>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let join_cancel = cancel.clone();
    tokio::spawn(async move {
        let PreparedChatExecution {
            session_id,
            turn_id,
            agent_id,
            agent_name,
            project_id,
            project_slug,
            events_tx,
            worker_id,
            ..
        } = prepared;

        loop {
            tokio::select! {
                event = handle.recv() => {
                    match event {
                        Some(ev) => {
                            debug!(
                                event = %summarize_turn_event(&ev),
                                agent = %agent_name,
                                "Harness chat received turn event"
                            );
                            let session_event_context = TurnEventContext {
                                session_id,
                                turn_id: Some(turn_id),
                                agent_id: Some(agent_id),
                                agent_name: Some(agent_name.clone()),
                                recorded_at: Utc::now(),
                            };
                            let runtime_events =
                                session_runtime_events_from_turn_event(&session_event_context, &ev);
                            let _ = events_tx.send(HarnessEvent::Turn(ev));
                            harness
                                .sessions()
                                .record_events(runtime_events, session_id);
                        }
                        None => break,
                    }
                }
                _ = join_cancel.cancelled() => {
                    warn!(agent = %agent_name, session = %session_id, "Harness chat execution cancelled");
                    handle.abort();
                    let is_current_execution = harness
                        .executions()
                        .get(&session_id)
                        .is_some_and(|entry| entry.registry_token == registry_token);
                    if is_current_execution {
                        let _ = harness.sessions().transition(SessionTransition {
                            session_id,
                            worker_id: worker_id.clone(),
                            phase: Some(ExecutionPhase::Finalizing),
                            status: SessionStatus::Cancelled,
                        }).await;
                        upsert_chat_session_record(
                            &harness,
                            ChatSessionRecord {
                                session_id,
                                project_id,
                                agent_id,
                                project_slug: project_slug.clone(),
                                agent_name: agent_name.clone(),
                                status: SessionStatus::Cancelled,
                            },
                            SessionUpsertMode::Spawn,
                        )
                        .await;
                    }
                    break;
                }
            }
        }
        if harness
            .executions()
            .get(&session_id)
            .is_some_and(|entry| entry.registry_token == registry_token)
        {
            harness.executions().remove(&session_id);
        }

        if join_cancel.is_cancelled() {
            return Err(crate::HarnessError::Other(anyhow!("Cancelled")));
        }

        let output = handle.output().await?;
        let _ = harness
            .sessions()
            .transition(SessionTransition {
                session_id,
                worker_id: worker_id.clone(),
                phase: Some(ExecutionPhase::Finalizing),
                status: SessionStatus::Completed,
            })
            .await;
        upsert_chat_session_record(
            &harness,
            ChatSessionRecord {
                session_id,
                project_id,
                agent_id,
                project_slug,
                agent_name: agent_name.clone(),
                status: SessionStatus::Completed,
            },
            SessionUpsertMode::Spawn,
        )
        .await;

        Ok(output)
    })
}

fn resolve_agent_id<P>(provider: &P, agent: &AgentRef) -> Result<Uuid>
where
    P: ProviderRuntime,
{
    match agent {
        AgentRef::Id(agent_id) => Ok(*agent_id),
        AgentRef::Name(agent_name) => provider
            .find_agent_manifest(agent_name)
            .map(|agent| agent.id)
            .ok_or_else(|| anyhow!("agent not found: {agent_name}")),
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
    status: SessionStatus,
}

#[derive(Clone, Copy)]
enum SessionUpsertMode {
    Await,
    Spawn,
}

async fn upsert_chat_session_record<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    params: ChatSessionRecord,
    mode: SessionUpsertMode,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let ChatSessionRecord {
        session_id,
        project_id,
        agent_id,
        project_slug,
        agent_name,
        status,
    } = params;
    let memory_namespace = chat_memory_namespace(&agent_name, &project_slug);
    let upsert = ChatSessionUpsert {
        session_id,
        status,
        project_id,
        agent_id,
        memory_namespace: Some(memory_namespace.clone()),
        metadata: json!({
            "source": "harness_chat",
            "agent_name": agent_name,
            "project_slug": project_slug,
        }),
    };

    match mode {
        SessionUpsertMode::Await => {
            if let Err(error) = harness.sessions().upsert_chat(upsert).await {
                warn!(
                    error = %error,
                    session_id = %session_id,
                    "Failed to upsert chat session"
                );
            }
        }
        SessionUpsertMode::Spawn => {
            let sessions = harness.sessions();
            tokio::spawn(async move {
                if let Err(error) = sessions.upsert_chat(upsert).await {
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

async fn restore_domain_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    session_id: Uuid,
) -> Result<bool>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let Some(persisted) = harness.sessions().get(session_id).await? else {
        return Ok(false);
    };
    if persisted.kind != SessionKind::Domain {
        return Ok(false);
    }
    let Some(domain) = persisted.domain else {
        return Ok(false);
    };
    let Some(agent_id) = persisted.agent_id else {
        return Err(anyhow!("domain session missing agent_id"));
    };
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

async fn activate_domain_for_chat<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    params: ActivateDomainForChat<'_>,
) -> Result<String>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
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

    let manifest = harness.provider().manifest_snapshot();
    let domain_name = domain_name_for_command(&manifest, domain_command);

    let _ = harness
        .sessions()
        .upsert_domain(DomainSessionUpsert {
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

async fn active_domain_command<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    domain_session_id: Uuid,
) -> Option<String>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    if let Some(session) = harness.domains().get(&domain_session_id) {
        return Some(session.domain_command.clone());
    }

    harness
        .sessions()
        .get(domain_session_id)
        .await
        .ok()
        .flatten()
        .and_then(|record| record.domain.map(|domain| domain.domain_command))
}
