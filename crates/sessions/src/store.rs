use anyhow::Result;
use uuid::Uuid;

use crate::types::SessionRecord;

/// Canonical metadata store for session lifecycle and references.
///
/// This store owns compact session records, not transcript lines, trace events,
/// or checkpoints. Implementations should preserve the record `version` field
/// for optimistic updates and recovery coordination.
pub trait SessionStore: Send + Sync {
    /// List all stored session records in implementation-defined order.
    fn list(&self) -> Result<Vec<SessionRecord>>;

    /// Load one session record by id.
    fn get(&self, session_id: Uuid) -> Result<Option<SessionRecord>>;

    /// Insert or replace a session record.
    fn put(&self, record: &SessionRecord) -> Result<()>;

    /// Delete a session record by id.
    fn delete(&self, session_id: Uuid) -> Result<()>;

    /// Replace a record only when its current version matches `expected_version`.
    ///
    /// Returns `true` when the swap was applied and `false` when the stored
    /// version did not match or the record was missing.
    fn compare_and_swap(
        &self,
        session_id: Uuid,
        expected_version: u64,
        next: &SessionRecord,
    ) -> Result<bool>;
}
