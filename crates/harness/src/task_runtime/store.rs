use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::types::{TaskExecutionState, TaskInboxItem, TaskSchedule};

/// Result of a durable inbox insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// A new durable receipt was created.
    Inserted(Box<TaskInboxItem>),
    /// A previously persisted cancellation intent consumed this submission.
    Cancelled(Box<TaskInboxItem>),
    /// The execution-run ID already has a retained receipt.
    Duplicate,
}

/// Result of atomically advancing and recording one scheduled occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OccurrenceOutcome {
    /// The schedule advanced and the occurrence entered the execution queue.
    Enqueued(Box<TaskInboxItem>),
    /// The schedule advanced and retained a terminal rejection receipt.
    Rejected(Box<TaskInboxItem>),
    /// The deterministic occurrence ID was already retained.
    Duplicate,
    /// The schedule was replaced or advanced before this occurrence committed.
    Stale,
}

/// Result of durably applying one cancellation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellationOutcome {
    /// Queued work was cancelled before execution.
    Queued(Box<TaskInboxItem>),
    /// Running work was durably cancelled and its execution token must be signalled.
    Running(Box<TaskInboxItem>),
    /// The run was not present yet, so a durable cancellation intent was recorded.
    Recorded,
    /// The receipt is missing or has already reached a terminal state.
    Inactive,
}

/// Persistence boundary used by the harness task coordinator.
#[async_trait]
pub trait TaskRuntimeStore: Send + Sync {
    /// Durably insert a manual or retry receipt with idempotency and overlap checks.
    async fn enqueue(&self, item: TaskInboxItem) -> Result<EnqueueOutcome>;

    /// Load queued receipts that should be dispatched during runtime startup.
    async fn recoverable(&self) -> Result<Vec<TaskInboxItem>>;

    /// Persist one valid lifecycle transition and return the updated receipt.
    async fn transition(
        &self,
        execution_run_id: Uuid,
        state: TaskExecutionState,
    ) -> Result<Option<TaskInboxItem>>;

    /// Atomically cancel queued work, persist an early cancellation intent, or
    /// report that a running execution token must be signalled.
    async fn cancel(&self, execution_run_id: Uuid) -> Result<CancellationOutcome>;

    /// Load the active hydrated schedule set.
    async fn schedules(&self) -> Result<Vec<TaskSchedule>>;

    /// Replace source schedule definitions, preserving progress for unchanged revisions.
    async fn replace_schedules(&self, schedules: Vec<TaskSchedule>) -> Result<()>;

    /// Atomically compare-and-advance a schedule and record its occurrence receipt.
    async fn materialize_occurrence(
        &self,
        schedule_id: Uuid,
        scheduled_for: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
        item: TaskInboxItem,
        rejection: Option<String>,
    ) -> Result<OccurrenceOutcome>;
}
