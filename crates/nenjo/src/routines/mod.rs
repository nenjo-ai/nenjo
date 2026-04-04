//! Routine execution — DAG-based execution pipelines.
//!
//! Routines are directed acyclic graphs of steps connected by conditional edges.
//! Each step can be an agent task, gate evaluation, lambda script, council
//! delegation, or terminal node.
//!
//! ```ignore
//! use nenjo::types::{TaskType, Task};
//!
//! // One-shot task execution
//! let result = provider.routine_by_id(routine_id)?.run(task).await?;
//!
//! // Streaming execution with events
//! let mut handle = provider.routine_by_id(routine_id)?
//!     .run_stream(task)
//!     .await?;
//!
//! // Cancel a running cron
//! handle.cancel();
//! ```

pub mod council;
pub mod cron;
pub mod executor;
pub mod gate;
pub mod traits;
pub mod types;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::runner::types::TurnEvent;

// Re-export key types at module level.
pub use traits::{LambdaOutput, LambdaRunner};
pub use types::{
    CronMode, CronStepConfig, EdgeCondition, LambdaStepConfig, RoutineInput, RoutineMetrics,
    StepMetrics, StepResult, StepType,
};

/// Events emitted during routine execution.
#[derive(Debug, Clone)]
pub enum RoutineEvent {
    /// A step is about to execute.
    StepStarted {
        step_id: Uuid,
        step_name: String,
        step_type: String,
        agent_id: Option<Uuid>,
    },
    /// A turn-loop event from an agent or gate step (tool calls, etc.).
    AgentEvent { step_id: Uuid, event: TurnEvent },
    /// A step completed successfully.
    StepCompleted {
        step_id: Uuid,
        result: StepResult,
        duration_ms: u64,
    },
    /// A step failed.
    StepFailed {
        step_id: Uuid,
        error: String,
        duration_ms: u64,
    },
    /// The entire routine finished.
    Done { result: StepResult },
    /// A cron cycle is starting.
    CronCycleStarted { cycle: u32 },
    /// A cron cycle completed with a result.
    CronCycleCompleted {
        cycle: u32,
        result: StepResult,
        /// Total input tokens across all steps in this cycle.
        total_input_tokens: u64,
        /// Total output tokens across all steps in this cycle.
        total_output_tokens: u64,
    },
}

/// Handle to a running routine execution.
///
/// Provides a stream of [`RoutineEvent`]s as the routine progresses, plus
/// access to the final [`StepResult`] when done. Cron routines can be
/// cancelled via [`cancel()`](Self::cancel).
pub struct RoutineExecutionHandle {
    events_rx: mpsc::UnboundedReceiver<RoutineEvent>,
    join: tokio::task::JoinHandle<Result<StepResult>>,
    cancel: CancellationToken,
}

impl RoutineExecutionHandle {
    pub(crate) fn new(
        events_rx: mpsc::UnboundedReceiver<RoutineEvent>,
        join: tokio::task::JoinHandle<Result<StepResult>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            events_rx,
            join,
            cancel,
        }
    }

    /// Cancel the running routine. For cron routines, this stops the poll loop
    /// between cycles. For one-shot routines, this stops between DAG steps.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Receive the next event. Returns `None` when the routine finishes.
    pub async fn recv(&mut self) -> Option<RoutineEvent> {
        self.events_rx.recv().await
    }

    /// Get a mutable reference to the underlying event receiver.
    pub fn events(&mut self) -> &mut mpsc::UnboundedReceiver<RoutineEvent> {
        &mut self.events_rx
    }

    /// Wait for the final result.
    pub async fn output(self) -> Result<StepResult> {
        self.join
            .await
            .map_err(|e| anyhow::anyhow!("routine execution task panicked: {e}"))?
    }
}
