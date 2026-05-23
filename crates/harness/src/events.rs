//! Harness-native execution and scheduler events.

use chrono::{DateTime, Utc};
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

/// Events emitted by scheduled cron and heartbeat handles.
#[derive(Debug, Clone)]
pub enum HarnessScheduleEvent {
    Scheduled {
        session_id: Uuid,
        id: Uuid,
        next_run_at: DateTime<Utc>,
    },
    Started {
        session_id: Uuid,
        id: Uuid,
        execution_id: Uuid,
        scheduled_for: DateTime<Utc>,
    },
    Cron {
        session_id: Uuid,
        execution_id: Uuid,
        event: nenjo::RoutineEvent,
    },
    Heartbeat {
        session_id: Uuid,
        execution_id: Uuid,
        event: nenjo::TurnEvent,
    },
    Completed {
        session_id: Uuid,
        id: Uuid,
        execution_id: Uuid,
        success: bool,
        error: Option<String>,
        input_tokens: u64,
        output_tokens: u64,
        completed_at: DateTime<Utc>,
        next_run_at: DateTime<Utc>,
    },
    Failed {
        session_id: Uuid,
        id: Uuid,
        execution_id: Option<Uuid>,
        error: String,
        next_run_at: DateTime<Utc>,
    },
    Stopped {
        session_id: Uuid,
        id: Uuid,
    },
}
