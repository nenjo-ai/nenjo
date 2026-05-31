use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLeaseGrant {
    pub session_id: Uuid,
    pub worker_id: String,
    pub lease_token: Uuid,
    pub lease_expires_at: DateTime<Utc>,
}
