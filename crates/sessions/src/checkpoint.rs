use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use crate::types::SessionCheckpoint;

#[derive(Debug, Clone, Default)]
pub struct CheckpointQuery {
    pub before_or_at_seq: Option<u64>,
}

/// Stores resumable execution state for sessions.
///
/// Checkpoints are snapshots of operational state, such as phase, active tool,
/// worktree, or scheduler runtime data. They are used for recovery and should
/// be append-friendly or versioned so callers can ask for the latest checkpoint
/// at or before a sequence number.
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    /// Persist one checkpoint snapshot.
    async fn save(&self, checkpoint: SessionCheckpoint) -> Result<()>;

    /// Load the newest checkpoint for `session_id` matching `query`.
    async fn load_latest(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>>;
}
