//! Harness-native execution events.
use uuid::Uuid;

/// Events emitted by harness execution streams.
#[derive(Debug, Clone)]
pub enum HarnessEvent {
    /// A domain session became active for this chat stream.
    DomainEntered {
        session_id: Uuid,
        domain_name: String,
    },
    /// A raw provider turn event after harness session/trace processing.
    Turn {
        session_id: Uuid,
        turn_id: Option<Uuid>,
        event: nenjo::TurnEvent,
    },
    /// A routine event after harness session/trace processing.
    Routine {
        session_id: Uuid,
        execution_run_id: Uuid,
        event: nenjo::RoutineEvent,
    },
}

/// Session-aware events returned by typed harness chat handles.
// Keep turn events inline: boxing this variant would add a heap allocation for
// every streamed token after the event has already left the harness channel.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessChatEvent<Delta = String> {
    /// A domain session became active for this chat stream.
    DomainEntered {
        session_id: Uuid,
        domain_name: String,
    },
    /// A chat turn event filtered by the selected delivery mode.
    Turn {
        session_id: Uuid,
        turn_id: Option<Uuid>,
        event: nenjo::TurnEvent<Delta>,
    },
}
