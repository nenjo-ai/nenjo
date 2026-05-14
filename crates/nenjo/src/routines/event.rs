//! Routine execution events.

use uuid::Uuid;

use crate::agents::runner::types::TurnEvent;
use crate::routines::StepResult;

/// Events emitted during routine execution.
#[derive(Debug, Clone)]
pub enum RoutineEvent {
    /// A step is about to execute.
    StepStarted {
        /// Step manifest ID.
        step_id: Uuid,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Human-readable step name.
        step_name: String,
        /// Step type label.
        step_type: String,
        /// Agent ID for agent-backed steps.
        agent_id: Option<Uuid>,
    },
    /// A turn-loop event from an agent or gate step.
    AgentEvent {
        /// Step manifest ID.
        step_id: Uuid,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Agent turn event emitted during the step.
        event: TurnEvent,
    },
    /// A step completed successfully.
    StepCompleted {
        /// Step manifest ID.
        step_id: Uuid,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Step output.
        result: StepResult,
        /// Step duration in milliseconds.
        duration_ms: u64,
    },
    /// A step failed.
    StepFailed {
        /// Step manifest ID.
        step_id: Uuid,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Error message.
        error: String,
        /// Step duration in milliseconds.
        duration_ms: u64,
    },
    /// The entire routine finished.
    Done {
        /// Task ID associated with the run, when available.
        task_id: Option<Uuid>,
        /// Final routine result.
        result: StepResult,
    },
    /// A cron cycle is starting.
    CronCycleStarted {
        /// One-based cron cycle number.
        cycle: u32,
    },
    /// A cron cycle completed with a result.
    CronCycleCompleted {
        /// One-based cron cycle number.
        cycle: u32,
        /// Final result for the cycle.
        result: StepResult,
        /// Total input tokens across all steps in this cycle.
        total_input_tokens: u64,
        /// Total output tokens across all steps in this cycle.
        total_output_tokens: u64,
    },
}
