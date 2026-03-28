//! Transport envelope for reliable delivery.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Wire envelope wrapping every event on the message bus.
///
/// Provides deduplication via `message_id`, routing via `user_id`, and retry
/// tracking via `attempt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Unique identifier for deduplication and ack tracking.
    pub message_id: Uuid,
    /// The user whose connection should receive this event.
    pub user_id: Uuid,
    /// The event payload (a serialized [`Command`] or [`Response`]).
    pub payload: serde_json::Value,
    /// When the event was created.
    pub created_at: DateTime<Utc>,
    /// Delivery attempt counter (starts at 1).
    #[serde(default = "default_attempt")]
    pub attempt: u8,
}

impl std::fmt::Display for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "envelope(id={}, user={}, attempt={})",
            self.message_id, self.user_id, self.attempt
        )
    }
}

fn default_attempt() -> u8 {
    1
}

impl Envelope {
    /// Create a new envelope with a random `message_id` and `attempt = 1`.
    pub fn new(user_id: Uuid, payload: serde_json::Value) -> Self {
        Self {
            message_id: Uuid::new_v4(),
            user_id,
            payload,
            created_at: Utc::now(),
            attempt: 1,
        }
    }
}
