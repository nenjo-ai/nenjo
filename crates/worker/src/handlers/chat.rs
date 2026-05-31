//! Chat command handlers.

use anyhow::{Context, Result};
use nenjo_sessions::{
    SessionStatus, SessionTranscriptAppend, SessionTranscriptEventPayload, SessionTransition,
    TranscriptState,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo::Slug;
use nenjo_events::{DomainActivation, Response, StreamEvent};

use nenjo_harness::events::HarnessEvent;
use nenjo_harness::registry::ExecutionKind;
use nenjo_harness::request::ChatRequest;
use nenjo_harness::{Harness, ProviderRuntime};

use crate::event_bridge::{agent_name, summarize_stream_event, turn_event_to_stream_event};
use crate::handlers::ResponseSender;
use crate::resource_resolver::PlatformResourceResolver;

#[derive(Clone)]
pub struct ChatCommandContext<S> {
    pub response_sink: S,
    pub worker_id: String,
}

pub struct ChatCommandRequest<'a> {
    pub message_id: Option<&'a str>,
    pub content: &'a str,
    pub project: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub session_id: Uuid,
    pub domain_session_id: Option<Uuid>,
    pub domain_activation: Option<DomainActivation>,
}

/// Worker integration methods for chat platform commands.
///
/// These methods adapt platform chat events to the platform-agnostic harness
/// chat API, then bridge harness events back into platform responses. Active
/// execution handles are registered by session id so cancellation and session
/// deletion can interrupt in-flight chats.
#[async_trait::async_trait]
pub(crate) trait WorkerChatHarnessExt<S>
where
    S: ResponseSender,
{
    /// Execute one chat message, including optional domain activation.
    async fn handle_chat(
        &self,
        ctx: &ChatCommandContext<S>,
        request: ChatCommandRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static;

    /// Cancel the active chat execution for an agent/project pair.
    async fn handle_chat_cancel(
        &self,
        ctx: &ChatCommandContext<S>,
        project: &str,
        agent: Option<&str>,
    ) -> Result<()>;

    /// Delete a chat session and cancel any active execution for that session.
    async fn handle_session_delete(
        &self,
        ctx: &ChatCommandContext<S>,
        project: &str,
        agent: &str,
        session_id: Uuid,
    ) -> Result<()>;
}

#[async_trait::async_trait]
impl<P, SessionRt, S> WorkerChatHarnessExt<S> for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
{
    async fn handle_chat(
        &self,
        ctx: &ChatCommandContext<S>,
        request: ChatCommandRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static,
    {
        handle_chat_adapter(self, ctx, request).await
    }

    async fn handle_chat_cancel(
        &self,
        ctx: &ChatCommandContext<S>,
        project: &str,
        agent: Option<&str>,
    ) -> Result<()> {
        handle_chat_cancel(self, ctx, project, agent).await
    }

    async fn handle_session_delete(
        &self,
        ctx: &ChatCommandContext<S>,
        project: &str,
        agent: &str,
        session_id: Uuid,
    ) -> Result<()> {
        handle_session_delete(self, ctx, project, agent, session_id).await
    }
}

async fn handle_chat_adapter<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    ctx: &ChatCommandContext<S>,
    request: ChatCommandRequest<'_>,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
{
    let ChatCommandRequest {
        message_id: _,
        content,
        project,
        agent,
        session_id,
        domain_session_id,
        domain_activation,
    } = request;

    let agent_slug = agent
        .map(Slug::parse)
        .transpose()?
        .context("No agent provided for chat")?;
    let manifest = harness.provider().manifest_snapshot();
    let resolver = PlatformResourceResolver::new(&manifest);
    let agent_id = resolver.agent_id(&agent_slug)?;
    let mut chat =
        ChatRequest::new(agent_slug.clone(), content.to_string()).with_session(session_id);
    if let Some(project) = project {
        chat = chat.with_project(Slug::parse(project)?);
    }
    if let Some(domain_session_id) = domain_session_id {
        chat = chat.with_domain_session(domain_session_id);
    }
    if let Some(activation) = domain_activation {
        chat = chat.with_domain_activation(
            activation.domain_session_id,
            activation.domain_command.clone(),
        );
    }

    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
    let aname = agent_name(&manifest, agent_id);
    let mut stream = harness.chat_stream(chat).await?;

    while let Some(event) = stream.recv().await {
        match event {
            HarnessEvent::DomainEntered {
                session_id: domain_session_id,
                domain_name,
            } => {
                let _ = ctx.response_sink.send(Response::AgentResponse {
                    session_id: Some(session_id),
                    payload: StreamEvent::DomainEntered {
                        session_id: domain_session_id,
                        domain_name,
                    },
                });
            }
            HarnessEvent::Turn {
                session_id: event_session_id,
                event: ev,
                ..
            } => {
                if let Some(se) = turn_event_to_stream_event(&ev, &aname) {
                    debug!(
                        stream_event = %summarize_stream_event(&se),
                        agent = %aname,
                        "Chat handler produced stream event"
                    );
                    if let Err(error) = ctx.response_sink.send(Response::AgentResponse {
                        session_id: Some(event_session_id),
                        payload: se,
                    }) {
                        warn!(
                            error = %error,
                            session = %event_session_id,
                            agent = %aname,
                            "Failed to enqueue chat response"
                        );
                    }
                }
            }
            HarnessEvent::Routine { .. } => {}
        }
    }

    debug!(session = %session_id, agent = %aname, "Chat harness event stream closed");
    debug!(session = %session_id, agent = %aname, "Awaiting chat stream output");
    let output = stream.output().await?;
    debug!(
        session = %session_id,
        agent = %aname,
        text_len = output.text.len(),
        "Chat stream output completed"
    );
    Ok(())
}
/// Cancel in-flight chat executions.
///
/// `ChatCancel` carries `project_id` and optionally `agent_id` but not `session_id`.
/// We scan the execution registry and cancel all matching entries.
async fn handle_chat_cancel<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    ctx: &ChatCommandContext<S>,
    project: &str,
    agent: Option<&str>,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
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
                .sessions()
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
                .sessions()
                .transition(SessionTransition {
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
        info!(agent = ?agent, %project, cancelled, "Cancelled chat executions");
    }
    Ok(())
}

/// Delete a chat session's local history.
async fn handle_session_delete<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    _ctx: &ChatCommandContext<S>,
    _project: &str,
    _agent: &str,
    session_id: Uuid,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
{
    let _ = harness.sessions().delete(session_id).await;
    Ok(())
}
