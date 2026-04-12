use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use nenjo_sessions::{SessionCoordinator, SessionLeaseGrant};
use parking_lot::Mutex;
use uuid::Uuid;

#[derive(Default)]
pub struct LocalSessionCoordinator {
    leases: Arc<Mutex<HashMap<Uuid, SessionLeaseGrant>>>,
}

impl LocalSessionCoordinator {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionCoordinator for LocalSessionCoordinator {
    fn acquire_lease(
        &self,
        session_id: Uuid,
        worker_id: &str,
        ttl: Duration,
    ) -> Result<SessionLeaseGrant> {
        let grant = SessionLeaseGrant {
            session_id,
            worker_id: worker_id.to_string(),
            lease_token: Uuid::new_v4(),
            lease_expires_at: Utc::now()
                + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(30)),
        };
        self.leases.lock().insert(session_id, grant.clone());
        Ok(grant)
    }

    fn renew_lease(
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

    fn release_lease(&self, session_id: Uuid, lease_token: Uuid) -> Result<()> {
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
