use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use nenjo_sessions::SessionLeaseGrant;
use parking_lot::Mutex;
use uuid::Uuid;

#[derive(Clone, Default)]
pub(super) struct SessionLeaseStore {
    leases: Arc<Mutex<HashMap<Uuid, SessionLeaseGrant>>>,
}

impl SessionLeaseStore {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn acquire(
        &self,
        session_id: Uuid,
        worker_id: &str,
        ttl: Duration,
    ) -> Result<SessionLeaseGrant> {
        let now = Utc::now();
        let mut leases = self.leases.lock();
        if let Some(existing) = leases.get(&session_id)
            && existing.lease_expires_at > now
            && existing.worker_id != worker_id
        {
            anyhow::bail!(
                "session {session_id} is already leased by worker {} until {}",
                existing.worker_id,
                existing.lease_expires_at
            );
        }
        let grant = SessionLeaseGrant {
            session_id,
            worker_id: worker_id.to_string(),
            lease_token: Uuid::new_v4(),
            lease_expires_at: now
                + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(30)),
        };
        leases.insert(session_id, grant.clone());
        Ok(grant)
    }

    pub(super) fn renew(
        &self,
        session_id: Uuid,
        lease_token: Uuid,
        ttl: Duration,
    ) -> Result<Option<SessionLeaseGrant>> {
        let mut leases = self.leases.lock();
        let Some(existing) = leases.get_mut(&session_id) else {
            return Ok(None);
        };
        if existing.lease_token != lease_token {
            return Ok(None);
        }
        existing.lease_expires_at = Utc::now()
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(30));
        Ok(Some(existing.clone()))
    }

    pub(super) fn release(&self, session_id: Uuid, lease_token: Uuid) -> Result<()> {
        let mut leases = self.leases.lock();
        if leases
            .get(&session_id)
            .is_some_and(|existing| existing.lease_token == lease_token)
        {
            leases.remove(&session_id);
        }
        Ok(())
    }
}
