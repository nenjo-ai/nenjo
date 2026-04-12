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

use crate::{Capability, Response};

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

/// Local NATS subject for sending realtime chat events back to the backend.
/// `streams.chat.<session_id>`
pub fn chat_stream_subject(_user_id: Uuid, session_id: Uuid) -> String {
    format!("streams.chat.{session_id}")
}

/// Resolve the local subject for a response.
pub fn response_subject(user_id: Uuid, response: &Response) -> String {
    match response {
        Response::AgentResponse {
            session_id: Some(session_id),
            ..
        } => chat_stream_subject(user_id, *session_id),
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

    #[test]
    fn chat_stream_subject_format() {
        let user_id = Uuid::nil();
        let session_id = Uuid::nil();
        assert_eq!(
            chat_stream_subject(user_id, session_id),
            format!("streams.chat.{session_id}")
        );
    }
}
