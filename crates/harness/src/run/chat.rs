//! Platform-free chat execution orchestration.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::Utc;
use nenjo::hooks::{ActiveHookScope, HookRuntime};
use nenjo::memory::MemoryScope;
use nenjo::provider::ToolFactory as _;
use nenjo_models::ChatMessage;
use nenjo_sessions::{
    ChatSessionUpsert, DomainSessionUpsert, DomainState, ExecutionPhase, SessionKind,
    SessionLeaseGrant, SessionOwnerKind, SessionRefs, SessionRuntimeEvent, SessionStatus,
    SessionTranscriptAppend, SessionTranscriptEventPayload, SessionTranscriptRecord,
    SessionTransition, SessionUpsert, TranscriptQuery, TranscriptState,
};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::events::HarnessEvent;
use crate::execution_context::{project_slug, summarize_turn_event};
use crate::handle::HarnessExecutionHandle;
use crate::registry::{ActiveExecution, ExecutionKind};
use crate::request::ChatRequest;
use crate::session::{
    TurnEventContext, chat_message_to_transcript, replay_ability_histories,
    replay_transcript_history, session_runtime_events_from_turn_event,
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
    if let Some((_, previous)) = harness.executions().remove(&request.session_id) {
        previous.cancel.cancel();
    }

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let mut prepared = prepare_chat_execution(harness, request, events_tx).await?;
    let runner = build_chat_runner(harness, &prepared).await?;
    let history = std::mem::take(&mut prepared.history);
    let handle = match prepared.template_override.take() {
        Some(template_override) => {
            runner
                .chat_with_history_template_stream(
                    &prepared.effective_content,
                    history,
                    template_override,
                )
                .await?
        }
        None => {
            runner
                .chat_with_history_stream(&prepared.effective_content, history)
                .await?
        }
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let registry_token = Uuid::new_v4();

    harness.executions().insert(
        prepared.session_id,
        ActiveExecution {
            kind: ExecutionKind::Chat,
            registry_token,
            execution_run_id: None,
            cancel: cancel.clone(),
            pause: None,
            turn_input: Some(handle.turn_input()),
        },
    );

    debug!("spawning chat execution task");

    let join = spawn_chat_execution(
        harness.clone(),
        handle,
        prepared,
        cancel.clone(),
        registry_token,
    );

    Ok(HarnessExecutionHandle::new(events_rx, join, cancel))
}

pub(crate) async fn try_enqueue_chat_message<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    request: &ChatRequest,
) -> crate::Result<bool>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let executions = harness.executions();
    let Some(active) = executions.get(&request.session_id) else {
        return Ok(false);
    };
    if active.kind != ExecutionKind::Chat {
        return Ok(false);
    }
    let Some(turn_input) = active.turn_input.clone() else {
        return Ok(false);
    };
    drop(active);

    let effective_content = if request.message.trim().is_empty() {
        match &request.domain_activation {
            Some(activation) => activation.domain_command.clone(),
            None => request.message.clone(),
        }
    } else {
        request.message.clone()
    };
    if effective_content.trim().is_empty() {
        return Ok(false);
    }

    turn_input
        .send_user_message(request.input_message_id, effective_content.clone())
        .map_err(|error| {
            crate::HarnessError::Other(anyhow!("failed to queue chat message: {error}"))
        })?;

    Ok(true)
}

struct PreparedChatExecution {
    session_id: Uuid,
    turn_id: Uuid,
    agent: nenjo::Slug,
    agent_id: Option<Uuid>,
    agent_name: String,
    project: Option<nenjo::Slug>,
    project_slug: String,
    effective_content: String,
    template_override: Option<String>,
    effective_domain_session_id: Option<Uuid>,
    hook_scopes: Vec<ActiveHookScope>,
    hook_transcript_dir: Option<std::path::PathBuf>,
    history: Vec<ChatMessage>,
    ability_histories: std::collections::BTreeMap<String, Vec<ChatMessage>>,
    events_tx: mpsc::UnboundedSender<HarnessEvent>,
    worker_id: String,
    lease: SessionLeaseGrant,
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
        input_message_id,
        agent,
        message,
        project,
        domain_session_id,
        domain_activation,
        template_override,
        hook_scopes,
        hook_transcript_dir,
    } = request;

    let sessions = harness.sessions();
    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
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
    let turn_id = input_message_id.unwrap_or_else(Uuid::new_v4);
    let agent_manifest = provider
        .find_agent_manifest(&agent)
        .ok_or_else(|| anyhow!("agent not found: {}", agent))?;
    let agent_id = None;
    let aname = agent_manifest.name.clone();
    let slug = project_slug(project.as_ref());
    let worker_id = "harness".to_string();
    let lease = sessions
        .acquire_lease(session_id, worker_id.clone(), SessionOwnerKind::Chat)
        .await?;

    if let Some(activation) = &domain_activation {
        debug!(
            agent = %agent,
            session = %session_id,
            domain_session = %activation.domain_session_id,
            "Preparing chat: activating domain"
        );
        let domain_name = match activate_domain_for_chat(
            harness,
            ActivateDomainForChat {
                worker_id: &worker_id,
                agent_slug: &agent,
                domain_command: &activation.domain_command,
                domain_session_id: activation.domain_session_id,
                agent_name: &aname,
                project_slug: &slug,
            },
        )
        .await
        {
            Ok(domain_name) => domain_name,
            Err(error) => {
                let _ = sessions.release_lease(lease).await;
                return Err(crate::HarnessError::Other(error));
            }
        };
        sessions
            .record_events_and_wait(
                lease.clone(),
                vec![SessionRuntimeEvent::TranscriptAppend(
                    SessionTranscriptAppend {
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
                    },
                )],
            )
            .await?;
        debug!(
            agent = %agent,
            session = %session_id,
            domain_session = %activation.domain_session_id,
            "Preparing chat: appended domain activation transcript"
        );
        let _ = events_tx.send(HarnessEvent::DomainEntered {
            session_id: activation.domain_session_id,
            domain_name,
        });
    }

    sessions.record_events(
        lease.clone(),
        vec![
            chat_session_upsert_runtime_event(ChatSessionRecord {
                session_id,
                project: project.clone(),
                agent: agent.clone(),
                project_slug: slug.clone(),
                agent_name: aname.clone(),
                status: SessionStatus::Active,
            }),
            SessionRuntimeEvent::Transition(SessionTransition {
                session_id,
                worker_id: worker_id.clone(),
                phase: Some(ExecutionPhase::CallingModel),
                status: SessionStatus::Active,
            }),
        ],
    );
    debug!(
        agent = %agent,
        session = %session_id,
        "Preparing chat: queued session upsert and calling_model transition"
    );
    let transcript_events = sessions
        .read_transcript(session_id, TranscriptQuery::default())
        .await?;
    debug!(
        agent = %agent,
        session = %session_id,
        transcript_events = transcript_events.len(),
        "Preparing chat: read transcript"
    );
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
        sessions
            .record_events_and_wait(
                lease.clone(),
                vec![SessionRuntimeEvent::TranscriptAppend(
                    SessionTranscriptAppend {
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
                    },
                )],
            )
            .await?;
        debug!(
            agent = %agent,
            session = %session_id,
            domain_session = %dsid,
            "Preparing chat: appended restored domain activation transcript"
        );
        let transcript_events = sessions
            .read_transcript(session_id, TranscriptQuery::default())
            .await?;
        return finish_prepare_chat_execution(
            &sessions,
            lease,
            PreparedChatInput {
                session_id,
                turn_id,
                agent,
                agent_id,
                agent_name: aname,
                project,
                project_slug: slug,
                effective_content,
                template_override,
                effective_domain_session_id,
                hook_scopes,
                hook_transcript_dir,
                transcript_events,
                events_tx,
                worker_id,
            },
        )
        .await;
    }
    finish_prepare_chat_execution(
        &sessions,
        lease,
        PreparedChatInput {
            session_id,
            turn_id,
            agent,
            agent_id,
            agent_name: aname,
            project,
            project_slug: slug,
            effective_content,
            template_override,
            effective_domain_session_id,
            hook_scopes,
            hook_transcript_dir,
            transcript_events,
            events_tx,
            worker_id,
        },
    )
    .await
}

struct PreparedChatInput {
    session_id: Uuid,
    turn_id: Uuid,
    agent: nenjo::Slug,
    agent_id: Option<Uuid>,
    agent_name: String,
    project: Option<nenjo::Slug>,
    project_slug: String,
    effective_content: String,
    template_override: Option<String>,
    effective_domain_session_id: Option<Uuid>,
    hook_scopes: Vec<ActiveHookScope>,
    hook_transcript_dir: Option<std::path::PathBuf>,
    transcript_events: Vec<nenjo_sessions::SessionTranscriptEvent>,
    events_tx: mpsc::UnboundedSender<HarnessEvent>,
    worker_id: String,
}

async fn finish_prepare_chat_execution<SessionRt>(
    sessions: &crate::HarnessSessions<SessionRt>,
    lease: SessionLeaseGrant,
    input: PreparedChatInput,
) -> crate::Result<PreparedChatExecution>
where
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let PreparedChatInput {
        session_id,
        turn_id,
        agent,
        agent_id,
        agent_name,
        project,
        project_slug,
        effective_content,
        template_override,
        effective_domain_session_id,
        hook_scopes,
        hook_transcript_dir,
        transcript_events,
        events_tx,
        worker_id,
    } = input;
    let history: Vec<ChatMessage> = replay_transcript_history(&transcript_events);
    let ability_histories = replay_ability_histories(&transcript_events);
    sessions.record_events(
        lease.clone(),
        vec![SessionRuntimeEvent::Transcript(SessionTranscriptRecord {
            session_id,
            turn_id: Some(turn_id),
            payload: SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(&ChatMessage::user(effective_content.clone())),
            },
        })],
    );
    debug!(
        agent = %agent,
        session = %session_id,
        history_len = history.len(),
        "Preparing chat: queued user message transcript"
    );
    info!(
        agent = %agent_name,
        agent_slug = %agent,
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
        agent_name,
        project,
        project_slug,
        effective_content,
        template_override,
        effective_domain_session_id,
        hook_scopes,
        hook_transcript_dir,
        history,
        ability_histories,
        events_tx,
        worker_id,
        lease,
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
    let project_work_dir = (!prepared.project_slug.is_empty()).then(|| {
        provider
            .tool_factory()
            .workspace_dir()
            .join(&prepared.project_slug)
    });
    let hook_workspace_dir = project_work_dir
        .clone()
        .unwrap_or_else(|| provider.tool_factory().workspace_dir());
    let hook_transcript_dir = prepared
        .hook_transcript_dir
        .clone()
        .unwrap_or_else(|| hook_workspace_dir.join(".nenjo").join("hooks"));
    let hook_runtime = Some(Arc::new(HookRuntime::new(
        prepared.session_id,
        hook_workspace_dir,
        hook_transcript_dir,
        prepared.hook_scopes.clone(),
    )));

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
                let agent = session.agent.clone();
                let project = session.project.clone();
                let domain_command = session.domain_command.clone();
                drop(session);
                let rebuilt = harness
                    .rebuild_domain_session(dsid, agent, project, &domain_command)
                    .await?;
                let mut instance = rebuilt.runner.instance().clone();
                instance.set_active_domain_session_id(dsid);
                instance.set_current_session_id(prepared.session_id);
                instance.hydrate_ability_histories(
                    prepared.session_id,
                    prepared.ability_histories.clone(),
                );
                instance.set_hook_runtime(hook_runtime.clone());
                let runner = nenjo::AgentRunner::from_instance(
                    instance,
                    rebuilt.runner.memory().cloned(),
                    rebuilt.runner.memory_scope().cloned(),
                );
                debug!(
                    agent = %prepared.agent,
                    session = %prepared.session_id,
                    domain_session = %dsid,
                    domain = %domain_command,
                    addon_len = runner
                        .instance()
                        .prompt_context()
                        .active_domain
                        .as_ref()
                        .and_then(|domain| domain.manifest.prompt_config.developer_prompt_addon.as_ref())
                        .map(|addon| addon.len())
                        .unwrap_or_default(),
                    "Prepared domain-expanded chat runner"
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
                    &prepared.lease,
                    ChatSessionRecord {
                        session_id: prepared.session_id,
                        project: prepared.project.clone(),
                        agent: prepared.agent.clone(),
                        project_slug: prepared.project_slug.clone(),
                        agent_name: prepared.agent_name.clone(),
                        status: SessionStatus::Failed,
                    },
                )
                .await;
                return Err(crate::HarnessError::InvalidCommand(
                    "Domain session expired. Please re-enter the domain.".to_string(),
                ));
            }
        }
    } else {
        let mut builder = provider
            .agent(&prepared.agent)
            .await
            .map_err(anyhow::Error::from)?;
        if let Some(project_slug) = &prepared.project {
            if let Some(project) = provider.find_project(project_slug) {
                builder = builder.with_project_context(project);
            } else {
                warn!(project = %project_slug, agent = %prepared.agent, "Project not found in manifest for chat session");
            }
        }
        if let Some(work_dir) = project_work_dir.clone() {
            builder = builder.with_work_dir(work_dir);
        }
        let builder = match harness
            .sessions()
            .memory_namespace(prepared.session_id)
            .await?
            .and_then(|namespace| MemoryScope::from_namespace(&namespace))
        {
            Some(scope) => builder.with_memory_scope(scope),
            None => builder,
        };
        let builder = if let Some(hook_runtime) = hook_runtime {
            builder.with_hook_runtime(hook_runtime)
        } else {
            builder
        };
        builder
            .with_tool_current_session_id(prepared.session_id)
            .with_ability_histories(prepared.ability_histories.clone())
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
            agent,
            agent_id,
            agent_name,
            project,
            project_slug,
            events_tx,
            worker_id,
            lease,
            ..
        } = prepared;
        let prepared_agent_for_record = agent;
        let lease_renewal = harness.sessions().spawn_lease_renewer(lease.clone());
        let mut cancellation_requested = false;

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
                                agent_id,
                                agent_name: Some(agent_name.clone()),
                                recorded_at: Utc::now(),
                            };
                            let forwarded = events_tx.send(HarnessEvent::Turn {
                                session_id,
                                turn_id: Some(turn_id),
                                event: ev.clone(),
                            });
                            if forwarded.is_err() {
                                warn!(
                                    agent = %agent_name,
                                    session = %session_id,
                                    "Failed to forward chat turn event because receiver was dropped"
                                );
                            } else {
                                debug!(
                                    agent = %agent_name,
                                    session = %session_id,
                                    "Forwarded chat turn event to harness stream"
                                );
                            }
                            let runtime_events =
                                session_runtime_events_from_turn_event(&session_event_context, &ev);
                            harness
                                .sessions()
                                .record_events(lease.clone(), runtime_events);
                        }
                        None => break,
                    }
                }
                _ = join_cancel.cancelled(), if !cancellation_requested => {
                    warn!(agent = %agent_name, session = %session_id, "Harness chat execution cancelled");
                    handle.cancel();
                    let is_current_execution = harness
                        .executions()
                        .get(&session_id)
                        .is_some_and(|entry| entry.registry_token == registry_token);
                    if is_current_execution {
                        harness.sessions().record_events(
                            lease.clone(),
                            vec![
                                SessionRuntimeEvent::Transition(SessionTransition {
                                    session_id,
                                    worker_id: worker_id.clone(),
                                    phase: Some(ExecutionPhase::Finalizing),
                                    status: SessionStatus::Cancelled,
                                }),
                                chat_session_upsert_runtime_event(ChatSessionRecord {
                                    session_id,
                                    project: project.clone(),
                                    agent: prepared_agent_for_record.clone(),
                                    project_slug: project_slug.clone(),
                                    agent_name: agent_name.clone(),
                                    status: SessionStatus::Cancelled,
                                }),
                            ],
                        );
                    }
                    cancellation_requested = true;
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)), if cancellation_requested => {
                    warn!(agent = %agent_name, session = %session_id, "Harness chat cancellation grace period elapsed; aborting execution");
                    handle.abort();
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
            lease_renewal.cancel();
            if let Err(error) = harness.sessions().flush_events(lease.clone()).await {
                warn!(error = %error, session = %session_id, "Failed to flush cancelled chat session events");
            }
            if let Err(error) = harness.sessions().release_lease(lease).await {
                warn!(error = %error, session = %session_id, "Failed to release cancelled chat session lease");
            }
            return Err(crate::HarnessError::Cancelled);
        }

        lease_renewal.cancel();
        let output = match handle.output().await {
            Ok(output) => output,
            Err(error) => {
                record_session_transition(
                    &harness.sessions(),
                    lease.clone(),
                    SessionTransition {
                        session_id,
                        worker_id: worker_id.clone(),
                        phase: Some(ExecutionPhase::Finalizing),
                        status: SessionStatus::Failed,
                    },
                );
                if let Err(flush_error) = harness.sessions().flush_events(lease.clone()).await {
                    warn!(error = %flush_error, session = %session_id, "Failed to flush failed chat session events");
                }
                if let Err(release_error) = harness.sessions().release_lease(lease).await {
                    warn!(error = %release_error, session = %session_id, "Failed to release failed chat session lease");
                }
                return Err(error.into());
            }
        };
        harness.sessions().record_events(
            lease.clone(),
            vec![
                SessionRuntimeEvent::Transition(SessionTransition {
                    session_id,
                    worker_id: worker_id.clone(),
                    phase: Some(ExecutionPhase::Finalizing),
                    status: SessionStatus::Completed,
                }),
                chat_session_upsert_runtime_event(ChatSessionRecord {
                    session_id,
                    project,
                    agent: prepared_agent_for_record,
                    project_slug,
                    agent_name: agent_name.clone(),
                    status: SessionStatus::Completed,
                }),
            ],
        );
        if let Err(error) = harness.sessions().flush_events(lease.clone()).await {
            warn!(error = %error, session = %session_id, "Failed to flush completed chat session events");
        }
        if let Err(error) = harness.sessions().release_lease(lease).await {
            warn!(error = %error, session = %session_id, "Failed to release chat session lease");
        }

        Ok(output)
    })
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
    project: Option<nenjo::Slug>,
    agent: nenjo::Slug,
    project_slug: String,
    agent_name: String,
    status: SessionStatus,
}

async fn upsert_chat_session_record<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    lease: &SessionLeaseGrant,
    params: ChatSessionRecord,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    harness.sessions().record_events(
        lease.clone(),
        vec![chat_session_upsert_runtime_event(params)],
    );
}

fn chat_session_upsert_runtime_event(params: ChatSessionRecord) -> SessionRuntimeEvent {
    let ChatSessionRecord {
        session_id,
        project,
        agent,
        project_slug,
        agent_name,
        status,
    } = params;
    let memory_namespace = chat_memory_namespace(&agent_name, &project_slug);
    let upsert = chat_session_upsert(
        session_id,
        status,
        project.as_ref().map(ToString::to_string),
        agent.to_string(),
        memory_namespace,
        agent_name,
        project_slug,
    );

    SessionRuntimeEvent::SessionUpsert(session_upsert_from_chat(upsert))
}

fn chat_session_upsert(
    session_id: Uuid,
    status: SessionStatus,
    project: Option<String>,
    agent: String,
    memory_namespace: String,
    agent_name: String,
    project_slug: String,
) -> ChatSessionUpsert {
    ChatSessionUpsert {
        session_id,
        status,
        project,
        agent: agent.clone(),
        memory_namespace: Some(memory_namespace),
        metadata: json!({
            "source": "harness_chat",
            "agent_name": agent_name,
            "agent_slug": agent,
            "project_slug": project_slug,
        }),
    }
}

fn session_upsert_from_chat(upsert: ChatSessionUpsert) -> SessionUpsert {
    SessionUpsert {
        session_id: upsert.session_id,
        kind: SessionKind::Chat,
        status: upsert.status,
        agent: Some(upsert.agent),
        project: upsert.project,
        task_id: None,
        routine: None,
        execution_run_id: None,
        parent_session_id: None,
        lease: None,
        memory_namespace: upsert.memory_namespace.clone(),
        refs: SessionRefs {
            memory_namespace: upsert.memory_namespace,
            ..Default::default()
        },
        metadata: upsert.metadata,
    }
}

fn record_session_transition<SessionRt>(
    sessions: &crate::HarnessSessions<SessionRt>,
    lease: SessionLeaseGrant,
    transition: SessionTransition,
) where
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    sessions.record_events(lease, vec![SessionRuntimeEvent::Transition(transition)]);
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
    let Some(agent_slug) = persisted
        .metadata
        .get("agent_slug")
        .and_then(|value| value.as_str())
        .map(nenjo::Slug::parse)
        .transpose()?
    else {
        return Err(anyhow!("domain session missing agent_slug"));
    };
    let project_slug = persisted
        .metadata
        .get("project_slug")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(nenjo::Slug::parse)
        .transpose()?;

    let session = harness
        .rebuild_domain_session(
            persisted.session_id,
            agent_slug,
            project_slug,
            &domain.domain_command,
        )
        .await?;

    harness.domains().insert(persisted.session_id, session);

    Ok(true)
}

struct ActivateDomainForChat<'a> {
    worker_id: &'a str,
    agent_slug: &'a nenjo::Slug,
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
        agent_slug,
        domain_command,
        domain_session_id,
        agent_name,
        project_slug,
    } = params;

    let manifest = harness.provider().manifest_snapshot();
    let domain_name = domain_name_for_command(&manifest, domain_command);
    let project_slug = if project_slug.is_empty() {
        None
    } else {
        Some(nenjo::Slug::parse(project_slug)?)
    };

    let session = harness
        .rebuild_domain_session(
            domain_session_id,
            agent_slug.clone(),
            project_slug.clone(),
            domain_command,
        )
        .await?;
    harness.domains().insert(domain_session_id, session);

    let _ = harness
        .sessions()
        .upsert_domain(DomainSessionUpsert {
            session_id: domain_session_id,
            status: SessionStatus::Active,
            project: project_slug.as_ref().map(ToString::to_string),
            agent: agent_slug.to_string(),
            worker_id: worker_id.to_string(),
            memory_namespace: Some(chat_memory_namespace(
                agent_name,
                project_slug
                    .as_ref()
                    .map(ToString::to_string)
                    .as_deref()
                    .unwrap_or_default(),
            )),
            metadata: json!({
                "source": "harness_domain",
                "agent_name": agent_name,
                "agent_slug": agent_slug.to_string(),
                "project_slug": project_slug.as_ref().map(ToString::to_string).unwrap_or_default(),
            }),
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
