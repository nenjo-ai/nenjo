//! NATS subject helpers.
//!
//! With per-org NATS accounts, workers see simplified local subjects.
//! The account's cross-account import mapping strips the org prefix:
//!
//! | PLATFORM subject              | Worker local subject  |
//! |-------------------------------|-----------------------|
//! | `requests.org.<org_id>.>`     | `requests.>`          |
//! | `responses.<user_id>`         | `responses.<user_id>` |

use uuid::Uuid;

use crate::{Capability, Response};

// ---------------------------------------------------------------------------
// Worker-local subjects (used by the worker/harness)
// ---------------------------------------------------------------------------

/// Local NATS subject for receiving commands for a specific capability.
/// `requests.<capability>`
pub fn requests_subject(capability: Capability) -> String {
    format!("requests.{capability}")
}

/// Local NATS wildcard subject for all capabilities.
/// `requests.*`
pub fn requests_subject_all() -> String {
    "requests.>".to_string()
}

/// Local NATS subject for sending responses back to the backend.
/// `responses`
pub fn responses_subject(user_id: Uuid) -> String {
    format!("responses.{user_id}")
}

/// Local NATS subject for sending realtime chat events back to the backend.
/// `streams.chat.<session_id>`
pub fn chat_stream_subject(session_id: Uuid) -> String {
    format!("streams.chat.{session_id}")
}

/// Resolve the local subject for a response.
pub fn response_subject(user_id: Uuid, response: &Response) -> String {
    match response {
        Response::AgentResponse {
            session_id: Some(session_id),
            ..
        } => chat_stream_subject(*session_id),
        _ => responses_subject(user_id),
    }
}

/// The JetStream stream name workers consume commands from.
pub const REQUESTS_STREAM_NAME: &str = "AGENT_REQUESTS";

/// Stream subjects for the requests stream.
pub const REQUESTS_STREAM_SUBJECTS: &[&str] = &["requests.>"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_subject_format() {
        assert_eq!(requests_subject(Capability::Chat), "requests.chat");
    }

    #[test]
    fn requests_subject_all_format() {
        assert_eq!(requests_subject_all(), "requests.>");
    }

    #[test]
    fn responses_subject_format() {
        let id = Uuid::nil();
        assert_eq!(responses_subject(id), format!("responses.{id}"));
    }

    #[test]
    fn chat_stream_subject_format() {
        let session_id = Uuid::nil();
        assert_eq!(
            chat_stream_subject(session_id),
            format!("streams.chat.{session_id}")
        );
    }
}
