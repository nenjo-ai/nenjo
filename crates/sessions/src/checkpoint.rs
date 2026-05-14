use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use crate::types::SessionCheckpoint;

#[derive(Debug, Clone, Default)]
pub struct CheckpointQuery {
    pub before_or_at_seq: Option<u64>,
}

#[async_trait]
pub trait CheckpointStore: Send + Sync {
    async fn save(&self, checkpoint: SessionCheckpoint) -> Result<()>;

    async fn load_latest(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>>;
}
