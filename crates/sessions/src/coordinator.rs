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

pub trait SessionCoordinator: Send + Sync {
    fn acquire_lease(
        &self,
        session_id: Uuid,
        worker_id: &str,
        ttl: Duration,
    ) -> Result<SessionLeaseGrant>;

    fn renew_lease(
        &self,
        session_id: Uuid,
        lease_token: Uuid,
        ttl: Duration,
    ) -> Result<Option<SessionLeaseGrant>>;

    fn release_lease(&self, session_id: Uuid, lease_token: Uuid) -> Result<()>;
}
