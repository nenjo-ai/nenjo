//! NATS subject helpers.
//!
//! With per-user NATS accounts, workers see simplified local subjects.
//! The account's cross-account import mapping strips the user_id prefix:
//!
//! | PLATFORM subject              | Worker local subject  |
//! |-------------------------------|-----------------------|
//! | `requests.<user_id>.<cap>`    | `requests.<cap>`      |
//! | `responses.<user_id>`         | `responses`           |

use uuid::Uuid;

use crate::Capability;

// ---------------------------------------------------------------------------
// Worker-local subjects (used by the worker/harness)
// ---------------------------------------------------------------------------

/// Local NATS subject for receiving commands for a specific capability.
/// `requests.<capability>`
pub fn requests_subject(_user_id: Uuid, capability: Capability) -> String {
    format!("requests.{capability}")
}

/// Local NATS wildcard subject for all capabilities.
/// `requests.*`
pub fn requests_subject_all(_user_id: Uuid) -> String {
    "requests.*".to_string()
}

/// Local NATS subject for sending responses back to the backend.
/// `responses`
pub fn responses_subject(_user_id: Uuid) -> String {
    "responses".to_string()
}

/// The JetStream stream name that backs agent events.
pub const STREAM_NAME: &str = "AGENT_EVENTS";

/// Stream subjects — matches requests and responses.
pub const STREAM_SUBJECTS: &[&str] = &["requests.>", "responses.>"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_subject_format() {
        let id = Uuid::nil();
        assert_eq!(requests_subject(id, Capability::Chat), "requests.chat");
    }

    #[test]
    fn requests_subject_all_format() {
        let id = Uuid::nil();
        assert_eq!(requests_subject_all(id), "requests.*");
    }

    #[test]
    fn responses_subject_format() {
        let id = Uuid::nil();
        assert_eq!(responses_subject(id), "responses");
    }
}
