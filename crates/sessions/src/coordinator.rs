use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLeaseGrant {
    pub session_id: Uuid,
    pub worker_id: String,
    pub lease_token: Uuid,
    pub lease_expires_at: DateTime<Utc>,
}

/// Coordinates exclusive ownership of recoverable sessions across workers.
///
/// Session runtimes use this trait to prevent multiple worker processes from
/// resuming or mutating the same recoverable session at the same time. A
/// coordinator can be in-memory for embedded/local use, or backed by a database
/// or distributed lock service for multi-worker deployments.
pub trait SessionCoordinator: Send + Sync {
    /// Try to acquire an exclusive lease for `session_id`.
    ///
    /// Implementors should return a grant with a unique token and expiration
    /// when the lease is available. If another live lease owns the session, the
    /// method should return an error that explains the conflict.
    fn acquire_lease(
        &self,
        session_id: Uuid,
        worker_id: &str,
        ttl: Duration,
    ) -> Result<SessionLeaseGrant>;

    /// Extend an existing lease when `lease_token` still owns the session.
    ///
    /// Returns `Some` with the renewed grant on success, or `None` when the
    /// token is stale, unknown, or no longer owns the session.
    fn renew_lease(
        &self,
        session_id: Uuid,
        lease_token: Uuid,
        ttl: Duration,
    ) -> Result<Option<SessionLeaseGrant>>;

    /// Release a lease token if it currently owns `session_id`.
    fn release_lease(&self, session_id: Uuid, lease_token: Uuid) -> Result<()>;
}
