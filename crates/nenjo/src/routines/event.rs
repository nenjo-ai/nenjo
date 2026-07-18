//! Routine execution events.

use uuid::Uuid;

use crate::Slug;
use crate::agents::runner::types::TurnEvent;
use crate::routines::{RoutineHandoff, StepResult};

/// Events emitted during routine execution.
#[derive(Debug, Clone)]
pub enum RoutineEvent {
    /// A step is about to execute.
    StepStarted {
        /// Step manifest slug.
        step_slug: Slug,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Human-readable step name.
        step_name: String,
        /// Step type label.
        step_type: String,
    },
    /// A turn-loop event from an agent or gate step.
    AgentEvent {
        /// Step manifest slug.
        step_slug: Slug,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Agent turn event emitted during the step.
        event: TurnEvent,
    },
    /// A step completed successfully.
    StepCompleted {
        /// Step manifest slug.
        step_slug: Slug,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Step output.
        result: StepResult,
        /// Step duration in milliseconds.
        duration_ms: u64,
    },
    /// A step failed.
    StepFailed {
        /// Step manifest slug.
        step_slug: Slug,
        /// Unique ID for this step execution attempt.
        step_run_id: Uuid,
        /// Human-readable step name.
        step_name: String,
        /// Step type label.
        step_type: String,
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
        /// Every activated incoming edge that reached a successful terminal step.
        handoffs: Vec<RoutineHandoff>,
    },
}
