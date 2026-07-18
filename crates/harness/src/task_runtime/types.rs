use chrono::{DateTime, Utc};
pub use nenjo_events::TaskExecutionTarget;
use nenjo_events::TaskScheduleDefinition;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Content needed to execute a task after transport decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContent {
    pub title: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// Why a task invocation entered the local inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskTrigger {
    Manual,
    Retry,
    Schedule {
        schedule_id: Uuid,
        scheduled_for: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
        /// Revision of the cached assignment used to create this occurrence.
        assignment_revision: String,
    },
}

/// A transport-independent task invocation accepted by the harness runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSubmission {
    pub requested_by: Uuid,
    pub task_id: Uuid,
    pub execution_run_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub target: TaskExecutionTarget,
    pub content: TaskContent,
    pub trigger: TaskTrigger,
}

/// Durable lifecycle state for one local task invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskExecutionState {
    Queued,
    Running,
    Completed,
    Failed { error: String },
    Cancelled,
    Rejected { reason: String },
}

/// Terminal result returned by a host task executor.
///
/// Keeping failure and cancellation distinct prevents a successfully handled
/// host error from being projected as a completed inbox item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskExecutorOutcome {
    Completed,
    Failed(String),
    Cancelled,
}

impl TaskExecutionState {
    /// Whether no further execution transition is expected.
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Cancelled | Self::Rejected { .. }
        )
    }
}

/// A persisted inbox receipt and its current local lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInboxItem {
    pub submission: TaskSubmission,
    pub state: TaskExecutionState,
    pub queued_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Monotonic local lifecycle revision used to reject stale broker replays.
    #[serde(default)]
    pub revision: u64,
    /// True when startup recovery requeued an execution that was running when
    /// the previous worker process stopped.
    #[serde(default)]
    pub recovered: bool,
}

impl TaskInboxItem {
    /// Create the initial durable receipt for a submission.
    pub fn queued(submission: TaskSubmission, now: DateTime<Utc>) -> Self {
        Self {
            submission,
            state: TaskExecutionState::Queued,
            queued_at: now,
            updated_at: now,
            revision: 0,
            recovered: false,
        }
    }
}

/// One hydrated task schedule installed into a cron-capable harness runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSchedule {
    pub id: Uuid,
    pub task_id: Uuid,
    pub authorized_by: Uuid,
    pub definition: TaskScheduleDefinition,
    pub next_run_at: DateTime<Utc>,
    /// Materialized occurrences used by finite recurrence boundaries.
    #[serde(default)]
    pub occurrence_count: u32,
    pub enabled: bool,
    pub runnable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub target: TaskExecutionTarget,
    pub content: TaskContent,
    /// Source snapshot revision used to distinguish edits from local advancement.
    pub revision: String,
}

/// Lifecycle notification emitted after the corresponding durable transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRuntimeEvent {
    /// The receipt after its durable transition.
    pub item: TaskInboxItem,
}
