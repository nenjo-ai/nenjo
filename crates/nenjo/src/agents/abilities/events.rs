//! Record compact child transcripts and forward parent-visible ability events.

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::agents::async_ops::{AsyncOpHandle, truncate};
use crate::agents::runner::types::{AsyncOperationTranscriptEvent, TurnEvent};

/// Drain nested events before completion when the parent has an event sink.
///
/// Transcript storage is updated before each event is forwarded. Token deltas
/// remain internal to the child; the parent receives its structured finish result.
pub(super) fn spawn_ability_event_bridge(
    handle: AsyncOpHandle,
    call_id: String,
    parent: Option<mpsc::UnboundedSender<TurnEvent>>,
    mut events: mpsc::UnboundedReceiver<TurnEvent>,
) -> Option<JoinHandle<()>> {
    parent.map(|parent| {
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                bridge_ability_transcript(&handle, &event, Some(parent.clone())).await;
                if let Some(event) = parent_event(event, &call_id) {
                    let _ = parent.send(event);
                }
            }
        })
    })
}

/// Attach missing parent IDs and select the events visible outside this child.
fn parent_event(mut event: TurnEvent, call_id: &str) -> Option<TurnEvent> {
    match &mut event {
        TurnEvent::ToolCallStart {
            parent_tool_name, ..
        }
        | TurnEvent::ToolCallEnd {
            parent_tool_name, ..
        } => {
            parent_tool_name.get_or_insert_with(|| call_id.to_string());
        }
        TurnEvent::ModelRequestStarted { parent_call_id, .. }
        | TurnEvent::ModelRequestCompleted { parent_call_id, .. } => {
            parent_call_id.get_or_insert_with(|| call_id.to_string());
        }
        TurnEvent::AbilityStarted { .. }
        | TurnEvent::AbilityCompleted { .. }
        | TurnEvent::MessageCompacted { .. }
        | TurnEvent::ModelCapacityWaiting { .. }
        | TurnEvent::ModelCapacityAcquired { .. }
        | TurnEvent::AsyncOperationEvent { .. }
        | TurnEvent::AsyncOperationTranscript { .. } => {}
        TurnEvent::AssistantTextDelta { .. }
        | TurnEvent::AssistantReasoningDelta { .. }
        | TurnEvent::TranscriptMessage { .. }
        | TurnEvent::ResourceCapacityWaiting { .. }
        | TurnEvent::ResourceCapacityAcquired { .. }
        | TurnEvent::ProviderRetryScheduled { .. }
        | TurnEvent::HookStarted { .. }
        | TurnEvent::HookActivated { .. }
        | TurnEvent::HookCompleted { .. }
        | TurnEvent::SubAgentEvent { .. }
        | TurnEvent::SubAgentTranscript { .. }
        | TurnEvent::Paused
        | TurnEvent::Resumed
        | TurnEvent::Done { .. } => return None,
    }
    Some(event)
}

/// Store bounded transcript entries; a nested tool failure remains recoverable.
pub(super) async fn bridge_ability_transcript(
    handle: &AsyncOpHandle,
    event: &TurnEvent,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
) {
    match event {
        TurnEvent::ToolCallStart { calls, .. } => {
            for call in calls {
                handle
                    .transcript(
                        AsyncOperationTranscriptEvent::ToolCall {
                            tool: call.tool_name.clone(),
                            summary: call
                                .text_preview
                                .clone()
                                .unwrap_or_else(|| truncate(&call.tool_args, 240)),
                        },
                        events_tx.clone(),
                    )
                    .await;
            }
        }
        TurnEvent::ToolCallEnd {
            tool_name, result, ..
        } => {
            if !result.success {
                handle
                    .recoverable_tool_error(
                        tool_name.clone(),
                        result
                            .error
                            .clone()
                            .unwrap_or_else(|| result.output.text_content()),
                        events_tx.clone(),
                    )
                    .await;
            }
            handle
                .transcript(
                    AsyncOperationTranscriptEvent::ToolResult {
                        tool: tool_name.clone(),
                        success: result.success,
                        summary: truncate(
                            &result
                                .error
                                .clone()
                                .unwrap_or_else(|| result.output.text_content()),
                            240,
                        ),
                    },
                    events_tx,
                )
                .await;
        }
        TurnEvent::TranscriptMessage { message } => {
            let transcript = match message {
                nenjo_models::ConversationMessage::Chat(chat) => {
                    let summary = truncate(&chat.content, 240);
                    match chat.role {
                        nenjo_models::ChatRole::User => {
                            AsyncOperationTranscriptEvent::Input { summary }
                        }
                        nenjo_models::ChatRole::Assistant => {
                            AsyncOperationTranscriptEvent::AssistantMessage { summary }
                        }
                        nenjo_models::ChatRole::System | nenjo_models::ChatRole::Developer => {
                            return;
                        }
                    }
                }
                nenjo_models::ConversationMessage::AssistantToolCalls { .. }
                | nenjo_models::ConversationMessage::ToolResults(_)
                | nenjo_models::ConversationMessage::ArtifactAnalysis(_)
                | nenjo_models::ConversationMessage::RuntimeContext(_) => return,
            };
            handle.transcript(transcript, events_tx).await;
        }
        TurnEvent::AbilityStarted { .. }
        | TurnEvent::AbilityCompleted { .. }
        | TurnEvent::ModelRequestStarted { .. }
        | TurnEvent::AssistantTextDelta { .. }
        | TurnEvent::AssistantReasoningDelta { .. }
        | TurnEvent::ResourceCapacityWaiting { .. }
        | TurnEvent::ResourceCapacityAcquired { .. }
        | TurnEvent::ModelCapacityWaiting { .. }
        | TurnEvent::ModelCapacityAcquired { .. }
        | TurnEvent::ProviderRetryScheduled { .. }
        | TurnEvent::ModelRequestCompleted { .. }
        | TurnEvent::HookStarted { .. }
        | TurnEvent::HookActivated { .. }
        | TurnEvent::HookCompleted { .. }
        | TurnEvent::SubAgentEvent { .. }
        | TurnEvent::SubAgentTranscript { .. }
        | TurnEvent::AsyncOperationEvent { .. }
        | TurnEvent::AsyncOperationTranscript { .. }
        | TurnEvent::MessageCompacted { .. }
        | TurnEvent::Paused
        | TurnEvent::Resumed
        | TurnEvent::Done { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_ids_are_added_without_overwriting_nested_attribution() {
        for existing in [None, Some("nested-ability".to_string())] {
            let event = TurnEvent::ModelRequestCompleted {
                request_id: "request".into(),
                parent_call_id: existing.clone(),
            };
            let Some(TurnEvent::ModelRequestCompleted { parent_call_id, .. }) =
                parent_event(event, "ability")
            else {
                panic!("model completion was dropped")
            };
            assert_eq!(
                parent_call_id.as_deref(),
                Some(existing.as_deref().unwrap_or("ability"))
            );

            let event = TurnEvent::ToolCallStart {
                batch_id: "batch".into(),
                parent_tool_name: existing.clone(),
                calls: Vec::new(),
            };
            let Some(TurnEvent::ToolCallStart {
                parent_tool_name, ..
            }) = parent_event(event, "ability")
            else {
                panic!("tool start was dropped")
            };
            assert_eq!(
                parent_tool_name.as_deref(),
                Some(existing.as_deref().unwrap_or("ability"))
            );
        }
    }

    #[test]
    fn internal_deltas_stay_in_the_child_while_provider_capacity_is_forwarded() {
        for event in [
            TurnEvent::AssistantTextDelta {
                request_id: "request".into(),
                delta: "text".into(),
            },
            TurnEvent::AssistantReasoningDelta {
                request_id: "request".into(),
                delta: "reasoning".into(),
            },
        ] {
            assert!(parent_event(event, "ability").is_none());
        }
        let event = TurnEvent::ModelCapacityWaiting {
            request_id: "request".into(),
            limit: 2,
        };
        assert!(matches!(parent_event(event, "ability"),
            Some(TurnEvent::ModelCapacityWaiting { request_id, limit: 2 }) if request_id == "request"));
    }
}
