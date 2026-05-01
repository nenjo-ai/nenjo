use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Chat,
    Task,
    Domain,
    CronSchedule,
    HeartbeatSchedule,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Pending,
    Active,
    Paused,
    Waiting,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionRefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionLease {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_token: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionSummary {
    #[serde(default)]
    pub last_checkpoint_seq: u64,
    #[serde(default)]
    pub last_transcript_seq: u64,
    #[serde(default)]
    pub transcript_state: TranscriptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: Uuid,
    pub kind: SessionKind,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_run_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<Uuid>,
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub refs: SessionRefs,
    #[serde(default)]
    pub lease: SessionLease,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<ScheduleState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<DomainState>,
    #[serde(default)]
    pub summary: SessionSummary,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptState {
    #[default]
    Clean,
    MidTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTranscriptChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionTranscriptEventPayload {
    ChatMessage {
        message: SessionTranscriptChatMessage,
    },
    ToolCalls {
        parent_tool_name: Option<String>,
        tool_names: Vec<String>,
        text_preview: Option<String>,
    },
    ToolResult {
        parent_tool_name: Option<String>,
        tool_name: String,
        success: bool,
        output_preview: Option<String>,
        error_preview: Option<String>,
    },
    AbilityStarted {
        ability_tool_name: String,
        ability_name: String,
        task_input: String,
    },
    AbilityCompleted {
        ability_tool_name: String,
        ability_name: String,
        success: bool,
        final_output: String,
    },
    TurnCompleted {
        final_output: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTranscriptEvent {
    pub session_id: Uuid,
    pub seq: u64,
    pub recorded_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    #[serde(flatten)]
    pub payload: SessionTranscriptEventPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainState {
    pub domain_command: String,
    pub turn_number: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleState {
    Cron(CronScheduleState),
    Heartbeat(HeartbeatScheduleState),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronScheduleState {
    pub schedule_expr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completion: Option<RunCompletion>,
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatScheduleState {
    pub interval_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_output_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completion: Option<RunCompletion>,
    #[serde(default)]
    pub run_in_progress: bool,
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCompletion {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_summary: Option<String>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub session_id: Uuid,
    pub seq: u64,
    pub saved_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_phase: Option<ExecutionPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_runtime: Option<SchedulerRuntimeSnapshot>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Preparing,
    CallingModel,
    ExecutingTools,
    Waiting,
    Finalizing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeSnapshot {
    pub repo_dir: String,
    pub work_dir: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerRuntimeSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_execution_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<u32>,
}
