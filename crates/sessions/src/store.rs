use anyhow::Result;
use uuid::Uuid;

use crate::types::SessionRecord;

/// Canonical metadata store for session lifecycle and references.
pub trait SessionStore: Send + Sync {
    fn list(&self) -> Result<Vec<SessionRecord>>;

    fn get(&self, session_id: Uuid) -> Result<Option<SessionRecord>>;

    fn put(&self, record: &SessionRecord) -> Result<()>;

    fn delete(&self, session_id: Uuid) -> Result<()>;

    fn compare_and_swap(
        &self,
        session_id: Uuid,
        expected_version: u64,
        next: &SessionRecord,
    ) -> Result<bool>;
}
