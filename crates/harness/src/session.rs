use chrono::{DateTime, Utc};
use nenjo_models::ChatMessage;
use nenjo_sessions::{
    SessionRuntimeEvent, SessionTranscriptChatMessage, SessionTranscriptEvent,
    SessionTranscriptEventPayload, SessionTranscriptRecord, TokenUsage, TraceEvent, TracePhase,
};
use tracing::warn;
use uuid::Uuid;

use crate::execution_trace::ExecutionTraceRuntime;
use crate::{Harness, HarnessProvider};

const PREVIEW_CHAR_LIMIT: usize = 2_000;

pub fn spawn_session_events<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    events: Vec<SessionRuntimeEvent>,
    session_id: Uuid,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    if events.is_empty() {
        return;
    }
    let harness = harness.clone();
    tokio::spawn(async move {
        let locks = harness.session_event_locks();
        let lock = locks
            .entry(session_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        for event in events {
            if let Err(error) = harness.record_session_event(event).await {
                warn!(
                    error = %error,
                    session_id = %session_id,
                    "Failed to record session event"
                );
            }
        }
    });
}

pub fn transcript_ref(session_id: Uuid) -> String {
    format!("transcripts/{session_id}.jsonl")
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

pub fn replay_transcript_history(events: &[SessionTranscriptEvent]) -> Vec<ChatMessage> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            SessionTranscriptEventPayload::ChatMessage { message } => {
                Some(transcript_message_to_chat(message.clone()))
            }
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
            ..
        } => vec![trace_event(
            context,
            TracePhase::AbilityStarted,
            Some(ability_tool_name.clone()),
            None,
            Some(preview(task_input)),
            serde_json::json!({ "ability_name": ability_name }),
            TokenUsage::default(),
        )],
        nenjo::TurnEvent::DelegationStarted {
            delegate_tool_name,
            target_agent_name,
            target_agent_id,
            task_input,
            ..
        } => vec![trace_event(
            context,
            TracePhase::DelegationStarted,
            Some(delegate_tool_name.clone()),
            None,
            Some(preview(task_input)),
            serde_json::json!({
                "target_agent_name": target_agent_name,
                "target_agent_id": target_agent_id,
            }),
            TokenUsage::default(),
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
                        "parent_tool_name": parent_tool_name,
                        "tool_call_id": call.tool_call_id,
                        "text_preview": call.text_preview,
                    }),
                    TokenUsage::default(),
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
                "parent_tool_name": parent_tool_name,
                "tool_call_id": tool_call_id,
                "tool_args_preview": preview(tool_args),
                "error_preview": result.error.as_deref().map(preview),
            }),
            TokenUsage::default(),
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
            serde_json::json!({ "ability_name": ability_name }),
            TokenUsage::default(),
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
            serde_json::json!({
                "target_agent_name": target_agent_name,
                "target_agent_id": target_agent_id,
            }),
            TokenUsage::default(),
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
            TokenUsage::default(),
        )],
        nenjo::TurnEvent::TranscriptMessage { message } => vec![trace_event(
            context,
            TracePhase::PromptRendered,
            None,
            None,
            Some(preview(&message.content)),
            serde_json::json!({ "message_role": message.role }),
            TokenUsage::default(),
        )],
        nenjo::TurnEvent::Paused => vec![trace_event(
            context,
            TracePhase::Paused,
            None,
            None,
            None,
            serde_json::Value::Null,
            TokenUsage::default(),
        )],
        nenjo::TurnEvent::Resumed => vec![trace_event(
            context,
            TracePhase::Resumed,
            None,
            None,
            None,
            serde_json::Value::Null,
            TokenUsage::default(),
        )],
        nenjo::TurnEvent::Done { output } => vec![trace_event(
            context,
            TracePhase::Completed,
            None,
            Some(true),
            Some(preview(&output.text)),
            serde_json::json!({ "tool_calls": output.tool_calls }),
            TokenUsage {
                input_tokens: output.input_tokens,
                output_tokens: output.output_tokens,
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
    usage: TokenUsage,
) -> TraceEvent {
    TraceEvent {
        session_id: context.session_id,
        turn_id: context.turn_id,
        recorded_at: context.recorded_at,
        phase,
        agent_id: context.agent_id,
        agent_name: context.agent_name.clone(),
        tool_name,
        success,
        usage,
        preview,
        metadata,
    }
}

fn preview(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars().take(PREVIEW_CHAR_LIMIT) {
        out.push(ch);
    }
    out
}
