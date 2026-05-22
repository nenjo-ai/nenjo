use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nenjo_models::ChatMessage;
use nenjo_sessions::{
    ChatSessionUpsert, CheckpointQuery, DomainSessionUpsert, SchedulerSessionUpsert,
    SessionCheckpoint, SessionCheckpointUpdate, SessionRecord, SessionRuntime, SessionRuntimeEvent,
    SessionTranscriptAppend, SessionTranscriptChatMessage, SessionTranscriptEvent,
    SessionTranscriptEventPayload, SessionTranscriptRecord, SessionTransition, TaskSessionUpsert,
    TokenUsage, TraceEvent, TracePhase, TranscriptQuery,
};
use tracing::warn;
use uuid::Uuid;

use crate::{HarnessError, Result};

const PREVIEW_CHAR_LIMIT: usize = 2_000;

/// Per-session mutexes used to preserve runtime event ordering in detached writers.
pub type SessionEventLocks = Arc<DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>;

/// Facade around the configured session runtime and ordering locks.
pub struct HarnessSessions<Runtime>
where
    Runtime: SessionRuntime,
{
    runtime: Arc<Runtime>,
    event_locks: SessionEventLocks,
}

impl<Runtime> Clone for HarnessSessions<Runtime>
where
    Runtime: SessionRuntime,
{
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
            event_locks: self.event_locks.clone(),
        }
    }
}

impl<Runtime> HarnessSessions<Runtime>
where
    Runtime: SessionRuntime,
{
    pub(crate) fn new(runtime: Arc<Runtime>, event_locks: SessionEventLocks) -> Self {
        Self {
            runtime,
            event_locks,
        }
    }

    pub async fn record(&self, event: SessionRuntimeEvent) -> Result<()> {
        self.runtime
            .record(event)
            .await
            .map_err(HarnessError::session_runtime)
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
        self.runtime
            .append_transcript(append)
            .await
            .map_err(HarnessError::session_runtime)
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
        self.runtime
            .update_checkpoint(update)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn transition(&self, transition: SessionTransition) -> Result<bool> {
        self.runtime
            .transition_session(transition)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn upsert_scheduler(&self, upsert: SchedulerSessionUpsert) -> Result<bool> {
        self.runtime
            .upsert_scheduler_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn upsert_chat(&self, upsert: ChatSessionUpsert) -> Result<bool> {
        self.runtime
            .upsert_chat_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn upsert_task(&self, upsert: TaskSessionUpsert) -> Result<bool> {
        self.runtime
            .upsert_task_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn upsert_domain(&self, upsert: DomainSessionUpsert) -> Result<bool> {
        self.runtime
            .upsert_domain_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    pub async fn memory_namespace(&self, session_id: Uuid) -> Result<Option<String>> {
        self.runtime
            .session_memory_namespace(session_id)
            .await
            .map_err(HarnessError::session_runtime)
    }
}

impl<Runtime> HarnessSessions<Runtime>
where
    Runtime: SessionRuntime + 'static,
{
    pub fn spawn_recorded_events(&self, events: Vec<SessionRuntimeEvent>, session_id: Uuid) {
        if events.is_empty() {
            return;
        }
        let sessions = self.clone();
        tokio::spawn(async move {
            let lock = sessions
                .event_locks
                .entry(session_id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            let _guard = lock.lock().await;
            for event in events {
                if let Err(error) = sessions.record(event).await {
                    warn!(
                        error = %error,
                        session_id = %session_id,
                        "Failed to record session event"
                    );
                }
            }
        });
    }
}

pub fn transcript_ref(session_id: Uuid) -> String {
    format!("transcripts/{session_id}.jsonl")
}

pub fn trace_ref(session_id: Uuid) -> String {
    format!("traces/{session_id}.jsonl")
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
        nenjo::TurnEvent::DelegationStarted {
            delegate_tool_name,
            target_agent_name,
            target_agent_id,
            task_input,
            ..
        } => vec![SessionTranscriptEventPayload::DelegationStarted {
            delegate_tool_name: delegate_tool_name.clone(),
            target_agent_name: target_agent_name.clone(),
            target_agent_id: *target_agent_id,
            task_input: preview(task_input),
        }],
        nenjo::TurnEvent::ToolCallStart {
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
        } => vec![SessionTranscriptEventPayload::AbilityCompleted {
            ability_tool_name: ability_tool_name.clone(),
            ability_name: ability_name.clone(),
            success: *success,
            final_output: preview(final_output),
        }],
        nenjo::TurnEvent::DelegationCompleted {
            delegate_tool_name,
            target_agent_name,
            target_agent_id,
            success,
            final_output,
        } => vec![SessionTranscriptEventPayload::DelegationCompleted {
            delegate_tool_name: delegate_tool_name.clone(),
            target_agent_name: target_agent_name.clone(),
            target_agent_id: *target_agent_id,
            success: *success,
            final_output: preview(final_output),
        }],
        nenjo::TurnEvent::TranscriptMessage { message } => {
            vec![SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(message),
            }]
        }
        nenjo::TurnEvent::Done { output } => vec![SessionTranscriptEventPayload::TurnCompleted {
            final_output: preview(&output.text),
        }],
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
        nenjo::TurnEvent::AbilityStarted {
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
            serde_json::json!({ "caller_history_snapshot": caller_history }),
            TraceEventFields {
                ability_name: Some(ability_name.clone()),
                task_input: Some(task_input.clone()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::DelegationStarted {
            delegate_tool_name,
            target_agent_name,
            target_agent_id,
            task_input,
            caller_history,
        } => vec![trace_event(
            context,
            TracePhase::DelegationStarted,
            Some(delegate_tool_name.clone()),
            None,
            Some(preview(task_input)),
            serde_json::json!({ "caller_history_snapshot": caller_history }),
            TraceEventFields {
                target_agent_id: Some(*target_agent_id),
                target_agent_name: Some(target_agent_name.clone()),
                task_input: Some(task_input.clone()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::ToolCallStart {
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
            parent_tool_name,
            tool_call_id,
            tool_name,
            tool_args,
            result,
        } => vec![trace_event(
            context,
            TracePhase::ToolCompleted,
            Some(tool_name.clone()),
            Some(result.success),
            Some(preview(&result.output)),
            serde_json::json!({
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
            serde_json::Value::Null,
            TraceEventFields {
                ability_name: Some(ability_name.clone()),
                final_output: Some(final_output.clone()),
                ..TraceEventFields::default()
            },
        )],
        nenjo::TurnEvent::DelegationCompleted {
            delegate_tool_name,
            target_agent_name,
            target_agent_id,
            success,
            final_output,
        } => vec![trace_event(
            context,
            TracePhase::DelegationCompleted,
            Some(delegate_tool_name.clone()),
            Some(*success),
            Some(preview(final_output)),
            serde_json::Value::Null,
            TraceEventFields {
                target_agent_id: Some(*target_agent_id),
                target_agent_name: Some(target_agent_name.clone()),
                final_output: Some(final_output.clone()),
                ..TraceEventFields::default()
            },
        )],
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
        let target_agent_id = Uuid::new_v4();
        let context = TurnEventContext::new(session_id);

        let ability = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::AbilityStarted {
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

        let delegation = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::DelegationStarted {
                delegate_tool_name: "delegate_to".to_string(),
                target_agent_name: "specialist".to_string(),
                target_agent_id,
                task_input: "solve this".to_string(),
                caller_history: Vec::new(),
            },
        )
        .remove(0);
        assert_eq!(delegation.target_agent_id, Some(target_agent_id));
        assert_eq!(delegation.target_agent_name.as_deref(), Some("specialist"));

        let tool = trace_events_from_turn_event(
            &context,
            &nenjo::TurnEvent::ToolCallStart {
                parent_tool_name: Some("ability.review".to_string()),
                calls: vec![nenjo::agents::ToolCall {
                    tool_call_id: Some("call-1".to_string()),
                    tool_name: "search".to_string(),
                    tool_args: "{\"q\":\"rust\"}".to_string(),
                    text_preview: Some("rust".to_string()),
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
