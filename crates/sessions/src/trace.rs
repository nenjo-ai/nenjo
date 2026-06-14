use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Token usage persisted in session traces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// Coarse execution phase for diagnostic and optimization traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TracePhase {
    Preparing,
    PromptRendered,
    CallingModel,
    ModelCompleted,
    ToolStarted,
    ToolCompleted,
    AbilityStarted,
    AbilityCompleted,
    SubAgentEvent,
    SubAgentTranscript,
    AsyncOperationEvent,
    AsyncOperationTranscript,
    MessageCompacted,
    Paused,
    Resumed,
    Completed,
    Failed,
}

/// Durable execution trace event for observability and later optimization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub session_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    pub recorded_at: DateTime<Utc>,
    pub phase: TracePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default)]
    pub usage: TokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_preview: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Query parameters for trace reads.
#[derive(Debug, Clone, Default)]
pub struct TraceQuery {
    pub session_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub phase: Option<TracePhase>,
    pub tool_name: Option<String>,
    pub parent_tool_name: Option<String>,
    pub ability_name: Option<String>,
    pub target_agent_id: Option<Uuid>,
    pub target_agent_name: Option<String>,
    pub success: Option<bool>,
    pub limit: Option<usize>,
}

/// Trace stores persist diagnostic execution evidence for sessions.
///
/// Traces are structured observability events, separate from transcript replay.
/// Implementations should keep enough fields queryable to support debugging,
/// optimization, and worker/platform inspection.
#[async_trait]
pub trait TraceStore: Send + Sync {
    /// Append one trace event.
    async fn append(&self, event: TraceEvent) -> Result<()>;

    /// Query trace events by session, agent, tool, phase, success, or limit.
    async fn query(&self, query: TraceQuery) -> Result<Vec<TraceEvent>>;
}
