use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nenjo_models::ChatMessage;
use nenjo_sessions::{
    ChatSessionUpsert, CheckpointQuery, DomainSessionUpsert, SchedulerSessionUpsert,
    SessionCheckpoint, SessionCheckpointUpdate, SessionKind, SessionLeaseGrant,
    SessionLeaseRequest, SessionOwnerKind, SessionRecord, SessionRefs, SessionRuntime,
    SessionRuntimeEvent, SessionTranscriptAppend, SessionTranscriptChatMessage,
    SessionTranscriptEvent, SessionTranscriptEventPayload, SessionTranscriptRecord,
    SessionTransition, SessionUpsert, SessionWriteOutcome, TaskSessionUpsert, TokenUsage,
    TraceEvent, TracePhase, TranscriptQuery,
};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use crate::{HarnessError, Result};

const PREVIEW_CHAR_LIMIT: usize = 2_000;

/// Compatibility alias for older harness builders.
pub type SessionEventLocks = Arc<DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>;

struct SessionEventBatch {
    grant: SessionLeaseGrant,
    events: Vec<SessionRuntimeEvent>,
    ack: Option<oneshot::Sender<std::result::Result<SessionWriteOutcome, String>>>,
}

/// Handle for the harness-owned session event writer task.
#[derive(Clone)]
pub(crate) struct SessionEventWriter {
    tx: mpsc::UnboundedSender<SessionEventBatch>,
}

impl SessionEventWriter {
    pub(crate) fn spawn<Runtime>(runtime: Arc<Runtime>) -> Self
    where
        Runtime: SessionRuntime + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionEventBatch>();

        tokio::spawn(async move {
            while let Some(batch) = rx.recv().await {
                let outcome = if batch.events.is_empty() {
                    Ok(SessionWriteOutcome { applied: 0 })
                } else {
                    runtime
                        .record_batch(&batch.grant, batch.events)
                        .await
                        .map_err(|error| error.to_string())
                };

                if let Err(error) = &outcome {
                    warn!(
                        error,
                        session_id = %batch.grant.session_id,
                        lease_token = %batch.grant.lease_token,
                        "Failed to record session event batch"
                    );
                }
                if let Some(ack) = batch.ack {
                    let _ = ack.send(outcome);
                }
            }
        });

        Self { tx }
    }

    pub(crate) fn record_events(&self, grant: SessionLeaseGrant, events: Vec<SessionRuntimeEvent>) {
        if events.is_empty() {
            return;
        }

        let session_id = grant.session_id;
        if self
            .tx
            .send(SessionEventBatch {
                grant,
                events,
                ack: None,
            })
            .is_err()
        {
            warn!(
                session_id = %session_id,
                "Failed to enqueue session events because the writer task stopped"
            );
        }
    }

    pub(crate) async fn record_events_and_wait(
        &self,
        grant: SessionLeaseGrant,
        events: Vec<SessionRuntimeEvent>,
    ) -> Result<SessionWriteOutcome> {
        let session_id = grant.session_id;
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .tx
            .send(SessionEventBatch {
                grant,
                events,
                ack: Some(ack_tx),
            })
            .is_err()
        {
            warn!(
                session_id = %session_id,
                "Failed to enqueue session events because the writer task stopped"
            );
            return Err(HarnessError::session_runtime(anyhow::anyhow!(
                "session event writer stopped"
            )));
        }

        ack_rx
            .await
            .map_err(|_| {
                HarnessError::session_runtime(anyhow::anyhow!("session event writer stopped"))
            })?
            .map_err(|error| HarnessError::session_runtime(anyhow::anyhow!(error)))
    }
}

/// Facade around the configured session runtime and queued event writer.
pub struct HarnessSessions<Runtime>
where
    Runtime: SessionRuntime,
{
    runtime: Arc<Runtime>,
    event_writer: SessionEventWriter,
}

impl<Runtime> Clone for HarnessSessions<Runtime>
where
    Runtime: SessionRuntime,
{
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
            event_writer: self.event_writer.clone(),
        }
    }
}

impl<Runtime> HarnessSessions<Runtime>
where
    Runtime: SessionRuntime,
{
    pub(crate) fn new(runtime: Arc<Runtime>, event_writer: SessionEventWriter) -> Self {
        Self {
            runtime,
            event_writer,
        }
    }

    pub async fn acquire_lease(
        &self,
        session_id: Uuid,
        worker_id: impl Into<String>,
        owner_kind: SessionOwnerKind,
    ) -> Result<SessionLeaseGrant> {
        self.runtime
            .acquire_session_lease(SessionLeaseRequest {
                session_id,
                worker_id: worker_id.into(),
                owner_kind,
                ttl: Duration::from_secs(120),
            })
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn release_lease(&self, grant: SessionLeaseGrant) -> Result<()> {
        self.runtime
            .release_session_lease(grant)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn renew_lease(&self, grant: &SessionLeaseGrant) -> Result<bool> {
        self.runtime
            .renew_session_lease(grant, Duration::from_secs(120))
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub fn spawn_lease_renewer(&self, grant: SessionLeaseGrant) -> CancellationToken
    where
        Runtime: 'static,
    {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let sessions = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {
                        match sessions.renew_lease(&grant).await {
                            Ok(true) => {}
                            Ok(false) => {
                                warn!(
                                    session_id = %grant.session_id,
                                    worker_id = %grant.worker_id,
                                    lease_token = %grant.lease_token,
                                    "Session lease renewal lost ownership"
                                );
                                break;
                            }
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    session_id = %grant.session_id,
                                    worker_id = %grant.worker_id,
                                    lease_token = %grant.lease_token,
                                    "Failed to renew session lease"
                                );
                                break;
                            }
                        }
                    }
                }
            }
        });
        cancel
    }

    pub async fn record_batch(
        &self,
        grant: &SessionLeaseGrant,
        events: Vec<SessionRuntimeEvent>,
    ) -> Result<SessionWriteOutcome> {
        self.runtime
            .record_batch(grant, events)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn record(&self, event: SessionRuntimeEvent) -> Result<()> {
        let session_id = event.session_id();
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Chat)
            .await?;
        let result = self.record_batch(&grant, vec![event]).await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(())
    }

    pub async fn get(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        self.runtime
            .get_session(session_id)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn list(&self) -> Result<Vec<SessionRecord>> {
        self.runtime
            .list_sessions()
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn delete(&self, session_id: Uuid) -> Result<()> {
        self.runtime
            .delete_session(session_id)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn read_transcript(
        &self,
        session_id: Uuid,
        query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>> {
        self.runtime
            .read_transcript(session_id, query)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn append_transcript(
        &self,
        append: SessionTranscriptAppend,
    ) -> Result<Option<SessionTranscriptEvent>> {
        let session_id = append.session_id;
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Chat)
            .await?;
        let result = self
            .record_batch(&grant, vec![SessionRuntimeEvent::TranscriptAppend(append)])
            .await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(None)
    }

    pub async fn latest_checkpoint(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        self.runtime
            .load_latest_checkpoint(session_id, query)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn update_checkpoint(&self, update: SessionCheckpointUpdate) -> Result<bool> {
        let session_id = update.session_id;
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Task)
            .await?;
        let result = self
            .record_batch(&grant, vec![SessionRuntimeEvent::CheckpointUpdate(update)])
            .await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(true)
    }

    pub async fn transition(&self, transition: SessionTransition) -> Result<bool> {
        let session_id = transition.session_id;
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Task)
            .await?;
        let result = self
            .record_batch(&grant, vec![SessionRuntimeEvent::Transition(transition)])
            .await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(true)
    }

    pub async fn upsert_scheduler(&self, upsert: SchedulerSessionUpsert) -> Result<bool> {
        let session_id = upsert.session_id;
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Cron)
            .await?;
        let result = self
            .record_batch(&grant, vec![SessionRuntimeEvent::SchedulerUpsert(upsert)])
            .await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(true)
    }

    pub async fn upsert_chat(&self, upsert: ChatSessionUpsert) -> Result<bool> {
        let session_id = upsert.session_id;
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Chat)
            .await?;
        let result = self
            .record_batch(&grant, vec![chat_session_upsert_event(upsert)])
            .await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(true)
    }

    pub async fn upsert_task(&self, upsert: TaskSessionUpsert) -> Result<bool> {
        let session_id = upsert.task_id;
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Task)
            .await?;
        let result = self
            .record_batch(&grant, vec![task_session_upsert_event(upsert)])
            .await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(true)
    }

    pub async fn upsert_domain(&self, upsert: DomainSessionUpsert) -> Result<bool> {
        let session_id = upsert.session_id;
        let grant = self
            .acquire_lease(session_id, "harness", SessionOwnerKind::Domain)
            .await?;
        let result = self
            .record_batch(&grant, vec![SessionRuntimeEvent::DomainUpsert(upsert)])
            .await;
        let release_result = self.release_lease(grant).await;
        result?;
        release_result?;
        Ok(true)
    }

    pub async fn memory_namespace(&self, session_id: Uuid) -> Result<Option<String>> {
        self.runtime
            .session_memory_namespace(session_id)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub fn record_events(&self, grant: SessionLeaseGrant, events: Vec<SessionRuntimeEvent>) {
        self.event_writer.record_events(grant, events);
    }

    pub async fn record_events_and_wait(
        &self,
        grant: SessionLeaseGrant,
        events: Vec<SessionRuntimeEvent>,
    ) -> Result<SessionWriteOutcome> {
        self.event_writer
            .record_events_and_wait(grant, events)
            .await
    }

    pub async fn flush_events(&self, grant: SessionLeaseGrant) -> Result<()> {
        self.record_events_and_wait(grant, Vec::new()).await?;
        Ok(())
    }

    pub fn record_events_best_effort(
        &self,
        session_id: Uuid,
        owner_kind: SessionOwnerKind,
        events: Vec<SessionRuntimeEvent>,
    ) where
        Runtime: 'static,
    {
        if events.is_empty() {
            return;
        }
        let sessions = self.clone();
        tokio::spawn(async move {
            let Ok(grant) = sessions
                .acquire_lease(session_id, "harness", owner_kind)
                .await
            else {
                warn!(session_id = %session_id, "Skipping session events because lease could not be acquired");
                return;
            };
            let result = sessions.record_events_and_wait(grant.clone(), events).await;
            if let Err(error) = result {
                warn!(error = %error, session_id = %session_id, "Failed to record best-effort session events");
            }
            if let Err(error) = sessions.release_lease(grant).await {
                warn!(error = %error, session_id = %session_id, "Failed to release best-effort session lease");
            }
        });
    }
}

pub fn chat_session_upsert_event(upsert: ChatSessionUpsert) -> SessionRuntimeEvent {
    SessionRuntimeEvent::SessionUpsert(SessionUpsert {
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
    })
}

pub fn task_session_upsert_event(upsert: TaskSessionUpsert) -> SessionRuntimeEvent {
    SessionRuntimeEvent::SessionUpsert(SessionUpsert {
        session_id: upsert.task_id,
        kind: SessionKind::Task,
        status: upsert.status,
        agent: upsert.agent,
        project: Some(upsert.project),
        task_id: Some(upsert.task_id),
        routine: upsert.routine,
        execution_run_id: Some(upsert.execution_run_id),
        parent_session_id: None,
        lease: None,
        memory_namespace: upsert.memory_namespace.clone(),
        refs: SessionRefs {
            memory_namespace: upsert.memory_namespace,
            ..Default::default()
        },
        metadata: upsert.metadata,
    })
}

pub fn chat_message_to_transcript(message: &ChatMessage) -> SessionTranscriptChatMessage {
    SessionTranscriptChatMessage {
        role: message.role.clone(),
        content: message.content.clone(),
    }
}

pub fn transcript_message_to_chat(message: SessionTranscriptChatMessage) -> ChatMessage {
    ChatMessage {
        role: message.role,
        content: message.content,
    }
}

fn domain_activated_to_chat(domain_command: &str, domain_name: &str) -> ChatMessage {
    ChatMessage::developer(format!(
        "Domain activated: {domain_command} ({domain_name}). The user explicitly activated this domain at this point in the conversation. Continue with this domain's guidance, capabilities, and permissions active."
    ))
}

fn domain_deactivated_to_chat(domain_command: &str, domain_name: &str) -> ChatMessage {
    ChatMessage::developer(format!(
        "Domain deactivated: {domain_command} ({domain_name}). The user explicitly exited this domain at this point in the conversation. Continue without this domain's expanded permissions active."
    ))
}

pub fn replay_transcript_history(events: &[SessionTranscriptEvent]) -> Vec<ChatMessage> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            SessionTranscriptEventPayload::ChatMessage { message } => {
                Some(transcript_message_to_chat(message.clone()))
            }
            SessionTranscriptEventPayload::DomainActivated {
                domain_command,
                domain_name,
                ..
            } => Some(domain_activated_to_chat(domain_command, domain_name)),
            SessionTranscriptEventPayload::DomainDeactivated {
                domain_command,
                domain_name,
                ..
            } => Some(domain_deactivated_to_chat(domain_command, domain_name)),
            SessionTranscriptEventPayload::TurnInterrupted { reason } => Some(
                ChatMessage::developer(format!("Previous turn was interrupted: {reason}")),
            ),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct TurnEventContext {
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
    pub recorded_at: DateTime<Utc>,
    pub agent_id: Option<Uuid>,
    pub agent_name: Option<String>,
}

impl TurnEventContext {
    pub fn new(session_id: Uuid) -> Self {
        Self {
            session_id,
            turn_id: None,
            recorded_at: Utc::now(),
            agent_id: None,
            agent_name: None,
        }
    }
}

pub fn session_runtime_events_from_turn_event(
    context: &TurnEventContext,
    event: &nenjo::TurnEvent,
) -> Vec<SessionRuntimeEvent> {
    let mut events = Vec::new();
    events.extend(
        transcript_payloads_from_turn_event(event)
            .into_iter()
            .map(|payload| {
                SessionRuntimeEvent::Transcript(SessionTranscriptRecord {
                    session_id: context.session_id,
                    turn_id: context.turn_id,
                    payload,
                })
            }),
    );
    events.extend(
        trace_events_from_turn_event(context, event)
            .into_iter()
            .map(SessionRuntimeEvent::Trace),
    );
    events
}

pub fn transcript_payloads_from_turn_event(
    event: &nenjo::TurnEvent,
) -> Vec<SessionTranscriptEventPayload> {
    match event {
        nenjo::TurnEvent::ModelRequestStarted { .. }
        | nenjo::TurnEvent::AssistantTextDelta { .. }
        | nenjo::TurnEvent::ModelRequestCompleted { .. } => Vec::new(),
        nenjo::TurnEvent::AbilityStarted {
            ability_tool_name,
            ability_name,
            task_input,
            ..
        } => vec![SessionTranscriptEventPayload::AbilityStarted {
            ability_tool_name: ability_tool_name.clone(),
            ability_name: ability_name.clone(),
            task_input: preview(task_input),
        }],
        nenjo::TurnEvent::ToolCallStart {
            batch_id: _,
            parent_tool_name,
            calls,
        } => vec![SessionTranscriptEventPayload::ToolCalls {
            parent_tool_name: parent_tool_name.clone(),
            tool_names: calls.iter().map(|call| call.tool_name.clone()).collect(),
            text_preview: calls
                .iter()
                .find_map(|call| call.text_preview.clone())
                .or_else(|| calls.first().map(|call| preview(&call.tool_args))),
        }],
        nenjo::TurnEvent::ToolCallEnd {
            parent_tool_name,
            tool_name,
            result,
            ..
        } => vec![SessionTranscriptEventPayload::ToolResult {
            parent_tool_name: parent_tool_name.clone(),
            tool_name: tool_name.clone(),
            success: result.success,
            output_preview: Some(preview(&result.output)),
            error_preview: result.error.as_deref().map(preview),
        }],
        nenjo::TurnEvent::AbilityCompleted {
            ability_tool_name,
            ability_name,
            success,
            final_output,
            ..
        } => vec![SessionTranscriptEventPayload::AbilityCompleted {
            ability_tool_name: ability_tool_name.clone(),
            ability_name: ability_name.clone(),
            success: *success,
            final_output: preview(final_output),
        }],
        nenjo::TurnEvent::TranscriptMessage { message } => {
            vec![SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(message),
            }]
        }
        nenjo::TurnEvent::AssistantResponse { .. } => Vec::new(),
        nenjo::TurnEvent::Done { output } => vec![SessionTranscriptEventPayload::TurnCompleted {
            final_output: preview(&output.text),
        }],
        nenjo::TurnEvent::HookActivated { .. }
        | nenjo::TurnEvent::HookStarted { .. }
        | nenjo::TurnEvent::HookCompleted { .. }
        | nenjo::TurnEvent::SubAgentEvent { .. }
        | nenjo::TurnEvent::SubAgentTranscript { .. }
        | nenjo::TurnEvent::AsyncOperationEvent { .. }
        | nenjo::TurnEvent::AsyncOperationTranscript { .. } => Vec::new(),
        nenjo::TurnEvent::MessageCompacted { .. }
        | nenjo::TurnEvent::Paused
        | nenjo::TurnEvent::Resumed => Vec::new(),
    }
}

pub fn trace_events_from_turn_event(
    context: &TurnEventContext,
    event: &nenjo::TurnEvent,
) -> Vec<TraceEvent> {
    match event {
        nenjo::TurnEvent::ModelRequestStarted { .. }
        | nenjo::TurnEvent::AssistantTextDelta { .. }
        | nenjo::TurnEvent::AssistantResponse { .. }
        | nenjo::TurnEvent::ModelRequestCompleted { .. } => Vec::new(),
        nenjo::TurnEvent::AbilityStarted {
            call_id,
            ability_tool_name,
            ability_name,
            task_input,
            caller_history,
        } => vec![trace_event(
            context,
            TracePhase::AbilityStarted,
            Some(ability_tool_name.clone()),
            None,
            Some(preview(task_input)),
            serde_json::json!({
                "call_id": call_id,
                "caller_history_snapshot": caller_history,
            }),
            TraceEventFields {
                ability_name: Some(ability_name.clone()),
                task_input: Some(task_input.clone()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::ToolCallStart {
            batch_id,
            parent_tool_name,
            calls,
        } => calls
            .iter()
            .map(|call| {
                trace_event(
                    context,
                    TracePhase::ToolStarted,
                    Some(call.tool_name.clone()),
                    None,
                    Some(preview(&call.tool_args)),
                    serde_json::json!({
                        "batch_id": batch_id,
                        "tool_call_id": call.tool_call_id,
                        "text_preview": call.text_preview,
                    }),
                    TraceEventFields {
                        parent_tool_name: parent_tool_name.clone(),
                        tool_args: Some(call.tool_args.clone()),
                        ..TraceEventFields::default()
                    },
                )
            })
            .collect(),
        nenjo::TurnEvent::ToolCallEnd {
            batch_id,
            parent_tool_name,
            tool_call_id,
            tool_name,
            tool_args,
            result,
            ..
        } => vec![trace_event(
            context,
            TracePhase::ToolCompleted,
            Some(tool_name.clone()),
            Some(result.success),
            Some(preview(&result.output)),
            serde_json::json!({
                "batch_id": batch_id,
                "tool_call_id": tool_call_id,
            }),
            TraceEventFields {
                parent_tool_name: parent_tool_name.clone(),
                tool_args: Some(tool_args.clone()),
                error_preview: result.error.as_deref().map(preview),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::AbilityCompleted {
            call_id,
            ability_tool_name,
            ability_name,
            success,
            final_output,
        } => vec![trace_event(
            context,
            TracePhase::AbilityCompleted,
            Some(ability_tool_name.clone()),
            Some(*success),
            Some(preview(final_output)),
            serde_json::json!({ "call_id": call_id }),
            TraceEventFields {
                ability_name: Some(ability_name.clone()),
                final_output: Some(final_output.clone()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::SubAgentEvent {
            slug,
            agent_name,
            kind,
            summary,
            model_visible,
        } => vec![trace_event(
            context,
            TracePhase::SubAgentEvent,
            None,
            None,
            Some(preview(summary)),
            serde_json::json!({
                "slug": slug,
                "agent_name": agent_name,
                "kind": kind,
                "model_visible": model_visible,
            }),
            TraceEventFields {
                target_agent_name: Some(agent_name.clone()),
                final_output: Some(summary.clone()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::SubAgentTranscript {
            slug,
            agent_name,
            event,
        } => vec![trace_event(
            context,
            TracePhase::SubAgentTranscript,
            event.tool_name().map(ToOwned::to_owned),
            event.success(),
            Some(preview(event.summary())),
            serde_json::json!({
                "slug": slug,
                "agent_name": agent_name,
                "kind": event.kind(),
            }),
            TraceEventFields {
                target_agent_name: Some(agent_name.clone()),
                final_output: Some(event.summary().to_string()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::AsyncOperationEvent {
            operation_id,
            kind,
            label,
            parent_operation_id,
            parent_tool_name,
            status,
            signal,
            summary,
            payload,
            model_visible,
        } => vec![trace_event(
            context,
            TracePhase::AsyncOperationEvent,
            parent_tool_name.clone(),
            None,
            summary.as_deref().map(preview),
            serde_json::json!({
                "operation_id": operation_id,
                "kind": kind,
                "label": label,
                "parent_operation_id": parent_operation_id,
                "status": status,
                "signal": signal,
                "payload": payload,
                "model_visible": model_visible,
            }),
            TraceEventFields {
                parent_tool_name: parent_tool_name.clone(),
                final_output: summary.clone(),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::AsyncOperationTranscript {
            operation_id,
            kind,
            label,
            event,
        } => vec![trace_event(
            context,
            TracePhase::AsyncOperationTranscript,
            event.tool_name().map(ToOwned::to_owned),
            event.success(),
            Some(preview(event.summary())),
            serde_json::json!({
                "operation_id": operation_id,
                "kind": kind,
                "label": label,
                "event_kind": event.kind(),
            }),
            TraceEventFields {
                final_output: Some(event.summary().to_string()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::HookActivated { .. }
        | nenjo::TurnEvent::HookStarted { .. }
        | nenjo::TurnEvent::HookCompleted { .. } => Vec::new(),
        nenjo::TurnEvent::MessageCompacted {
            messages_before,
            messages_after,
        } => vec![trace_event(
            context,
            TracePhase::MessageCompacted,
            None,
            None,
            None,
            serde_json::json!({
                "messages_before": messages_before,
                "messages_after": messages_after,
            }),
            TraceEventFields::default(),
        )],
        nenjo::TurnEvent::TranscriptMessage { message } => vec![trace_event(
            context,
            TracePhase::PromptRendered,
            None,
            None,
            Some(preview(&message.content)),
            serde_json::json!({ "message_role": message.role }),
            TraceEventFields::default(),
        )],
        nenjo::TurnEvent::Paused => vec![trace_event(
            context,
            TracePhase::Paused,
            None,
            None,
            None,
            serde_json::Value::Null,
            TraceEventFields::default(),
        )],
        nenjo::TurnEvent::Resumed => vec![trace_event(
            context,
            TracePhase::Resumed,
            None,
            None,
            None,
            serde_json::Value::Null,
            TraceEventFields::default(),
        )],
        nenjo::TurnEvent::Done { output } => vec![trace_event(
            context,
            TracePhase::Completed,
            None,
            Some(true),
            Some(preview(&output.text)),
            serde_json::json!({ "tool_calls": output.tool_calls }),
            TraceEventFields {
                final_output: Some(output.text.clone()),
                usage: TokenUsage {
                    input_tokens: output.input_tokens,
                    output_tokens: output.output_tokens,
                },
                ..TraceEventFields::default()
            },
        )],
    }
}

fn trace_event(
    context: &TurnEventContext,
    phase: TracePhase,
    tool_name: Option<String>,
    success: Option<bool>,
    preview: Option<String>,
    metadata: serde_json::Value,
    rich: TraceEventFields,
) -> TraceEvent {
    TraceEvent {
        session_id: context.session_id,
        turn_id: context.turn_id,
        recorded_at: context.recorded_at,
        phase,
        agent_id: context.agent_id,
        agent_name: context.agent_name.clone(),
        tool_name,
        parent_tool_name: rich.parent_tool_name,
        ability_name: rich.ability_name,
        target_agent_id: rich.target_agent_id,
        target_agent_name: rich.target_agent_name,
        success,
        usage: rich.usage,
        preview,
        task_input: rich.task_input,
        final_output: rich.final_output,
        tool_args: rich.tool_args,
        error_preview: rich.error_preview,
        metadata,
    }
}

#[derive(Default)]
struct TraceEventFields {
    parent_tool_name: Option<String>,
    ability_name: Option<String>,
    target_agent_id: Option<Uuid>,
    target_agent_name: Option<String>,
    task_input: Option<String>,
    final_output: Option<String>,
    tool_args: Option<String>,
    error_preview: Option<String>,
    usage: TokenUsage,
}

fn preview(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars().take(PREVIEW_CHAR_LIMIT) {
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo_sessions::{SessionTranscriptEvent, SessionTranscriptEventPayload};

    #[test]
    fn replay_transcript_history_surfaces_interruption_to_agent() {
        let session_id = Uuid::new_v4();
        let events = vec![SessionTranscriptEvent {
            session_id,
            seq: 1,
            recorded_at: Utc::now(),
            turn_id: None,
            payload: SessionTranscriptEventPayload::TurnInterrupted {
                reason: "cancelled by user".to_string(),
            },
        }];

        let history = replay_transcript_history(&events);

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "developer");
        assert_eq!(
            history[0].content,
            "Previous turn was interrupted: cancelled by user"
        );
    }

    #[test]
    fn trace_events_preserve_rich_execution_fields() {
        let session_id = Uuid::new_v4();
        let context = TurnEventContext::new(session_id);

        let ability = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::AbilityStarted {
                call_id: "ability-call-1".to_string(),
                ability_tool_name: "ability.review".to_string(),
                ability_name: "Review".to_string(),
                task_input: "inspect this".to_string(),
                caller_history: vec![ChatMessage::user("please inspect")],
            },
        )
        .remove(0);
        assert_eq!(ability.ability_name.as_deref(), Some("Review"));
        assert_eq!(ability.task_input.as_deref(), Some("inspect this"));
        assert!(ability.metadata["caller_history_snapshot"].is_array());

        let sub_agent = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::SubAgentEvent {
                slug: "specialist_review".to_string(),
                agent_name: "specialist".to_string(),
                kind: "completed".to_string(),
                summary: "done".to_string(),
                model_visible: false,
            },
        )
        .remove(0);
        assert_eq!(sub_agent.target_agent_name.as_deref(), Some("specialist"));
        assert_eq!(sub_agent.metadata["slug"], "specialist_review");
        assert_eq!(sub_agent.metadata["kind"], "completed");

        let sub_agent_transcript = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::SubAgentTranscript {
                slug: "specialist_review".to_string(),
                agent_name: "specialist".to_string(),
                event: nenjo::SubAgentTranscriptEvent::ToolResult {
                    tool: "search".to_string(),
                    success: true,
                    summary: "found relevant files".to_string(),
                },
            },
        )
        .remove(0);
        assert_eq!(sub_agent_transcript.phase, TracePhase::SubAgentTranscript);
        assert_eq!(
            sub_agent_transcript.target_agent_name.as_deref(),
            Some("specialist")
        );
        assert_eq!(sub_agent_transcript.tool_name.as_deref(), Some("search"));
        assert_eq!(sub_agent_transcript.success, Some(true));
        assert_eq!(sub_agent_transcript.metadata["slug"], "specialist_review");
        assert_eq!(sub_agent_transcript.metadata["kind"], "tool_result");

        let tool = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::ToolCallStart {
                batch_id: "batch-1".to_string(),
                parent_tool_name: Some("ability.review".to_string()),
                calls: vec![nenjo::agents::ToolCall {
                    tool_call_id: Some("call-1".to_string()),
                    tool_name: "search".to_string(),
                    tool_args: "{\"q\":\"rust\"}".to_string(),
                    text_preview: Some("rust".to_string()),
                    metadata: None,
                }],
            },
        )
        .remove(0);
        assert_eq!(tool.parent_tool_name.as_deref(), Some("ability.review"));
        assert_eq!(tool.tool_args.as_deref(), Some("{\"q\":\"rust\"}"));

        let done = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::Done {
                output: nenjo::TurnOutput {
                    task_id: None,
                    text: "finished".to_string(),
                    input_tokens: 3,
                    output_tokens: 4,
                    tool_calls: 1,
                    messages: Vec::new(),
                },
            },
        )
        .remove(0);
        assert_eq!(done.final_output.as_deref(), Some("finished"));
        assert_eq!(done.usage.input_tokens, 3);
        assert_eq!(done.usage.output_tokens, 4);
    }
}
