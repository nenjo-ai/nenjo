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
