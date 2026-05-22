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
    Turn(nenjo::TurnEvent),
    /// A routine event after harness session/trace processing.
    Routine(nenjo::RoutineEvent),
}

/// Events emitted by scheduled cron and heartbeat handles.
#[derive(Debug, Clone)]
pub enum HarnessScheduleEvent {
    Scheduled {
        id: Uuid,
        next_run_at: DateTime<Utc>,
    },
    Started {
        id: Uuid,
        execution_id: Uuid,
        scheduled_for: DateTime<Utc>,
    },
    Cron(nenjo::RoutineEvent),
    Heartbeat(nenjo::TurnEvent),
    Completed {
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
        id: Uuid,
        execution_id: Option<Uuid>,
        error: String,
        next_run_at: DateTime<Utc>,
    },
    Stopped {
        id: Uuid,
    },
}
