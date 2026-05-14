use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use crate::types::SessionTranscriptEvent;

/// Query parameters for transcript reads.
#[derive(Debug, Clone, Default)]
pub struct TranscriptQuery {
    pub after_seq: Option<u64>,
    pub limit: Option<usize>,
}

/// Transcript stores persist ordered evidence for a session.
#[async_trait]
pub trait TranscriptStore: Send + Sync {
    async fn append(&self, event: SessionTranscriptEvent) -> Result<u64>;

    async fn read(
        &self,
        session_id: Uuid,
        query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>>;
}
