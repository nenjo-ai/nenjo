//! Compact task documents returned to agents.
//!
//! Platform task records contain database identities and catalog bookkeeping
//! needed by the dashboard and REST API. Agent tools instead expose stable
//! slugs and human-readable names, matching the summary/document convention
//! used by the manifest resource tools.

use nenjo::Slug;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Task priority exposed by agent task tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Critical,
}

/// Human-readable execution target for a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskTarget {
    Agent { slug: Slug },
    Routine { slug: Slug },
}

/// Scheduling state for a scheduled task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTaskState {
    Queued,
    Paused,
    Running,
}

/// How a task enters the worker inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TaskDispatch {
    Manual,
    Scheduled { state: ScheduledTaskState },
}

/// Compact task metadata returned by `list_tasks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub slug: Slug,
    pub title: String,
    pub status: String,
    pub priority: TaskPriority,
    pub project: Option<Slug>,
    pub target: Option<TaskTarget>,
    pub dispatch: TaskDispatch,
    pub labels: Vec<String>,
}

/// Detailed task document returned by `get_task` and `configure_task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDocument {
    #[serde(flatten)]
    pub summary: TaskSummary,
    pub instructions: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

/// Result returned by `list_tasks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksListResult {
    pub tasks: Vec<TaskSummary>,
}

/// Human-readable task-label catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLabelSummary {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

/// Result returned by `list_task_labels`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLabelsListResult {
    pub labels: Vec<TaskLabelSummary>,
}

/// Result returned by `get_task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGetResult {
    pub task: Option<TaskDocument>,
}

/// Result returned by `configure_task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskConfigureResult {
    pub task: TaskDocument,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlatformTaskRecord {
    pub slug: Slug,
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub project_slug: Option<Slug>,
    pub title: String,
    pub instructions: Option<String>,
    pub encrypted_payload: Option<Value>,
    pub status: PlatformTaskStatus,
    pub priority: TaskPriority,
    pub execution_target: Option<PlatformTaskTarget>,
    pub dispatch: TaskDispatch,
    #[serde(default)]
    pub labels: Vec<PlatformTaskLabel>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlatformTaskStatus {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlatformTaskLabel {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlatformTaskLabelRecord {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PlatformTaskTarget {
    Agent { id: Uuid },
    Routine { id: Uuid },
}
