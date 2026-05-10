//! NATS subject helpers.
//!
//! With per-org NATS accounts, workers see simplified local subjects.
//! The account's cross-account import mapping strips the org prefix:
//!
//! | PLATFORM subject              | Worker local subject  |
//! |-------------------------------|-----------------------|
//! | `work_requests.org.<org_id>.>` | `work_requests.>`     |
//! | `worker_requests.org.<org_id>.>` | `worker_requests.>` |
//! | `responses.<user_id>`         | `responses.<user_id>` |

use uuid::Uuid;

use crate::{Capability, Response};

// ---------------------------------------------------------------------------
// Worker-local subjects (used by the worker/harness)
// ---------------------------------------------------------------------------

/// Local NATS subject for receiving commands for a specific capability.
/// `work_requests.<capability>`
pub fn requests_subject(capability: Capability) -> String {
    format!("work_requests.{capability}")
}

/// Local NATS wildcard subject for all capabilities.
/// `work_requests.*`
pub fn requests_subject_all() -> String {
    "work_requests.>".to_string()
}

/// Local NATS subject for receiving commands targeted to one worker.
/// `worker_requests.<worker_id>.<capability>`
pub fn worker_requests_subject(worker_id: Uuid, capability: Capability) -> String {
    format!("worker_requests.{worker_id}.{capability}")
}

/// Local NATS wildcard subject for commands targeted to one worker.
/// `worker_requests.<worker_id>.>`
pub fn worker_requests_subject_all(worker_id: Uuid) -> String {
    format!("worker_requests.{worker_id}.>")
}

/// Local NATS wildcard subject for fanout worker commands.
/// `broadcast_requests.*`
pub fn broadcast_requests_subject_all() -> String {
    "broadcast_requests.>".to_string()
}

/// Local NATS subject for sending worker/system responses back to the backend.
/// `responses.<org_id>`
pub fn responses_subject(org_id: Uuid) -> String {
    response_org_subject(org_id)
}

/// Local NATS subject for sending actor-scoped responses back to the backend.
/// `responses.<org_id>.<user_id>`
pub fn response_user_subject(org_id: Uuid, user_id: Uuid) -> String {
    format!("responses.{org_id}.{user_id}")
}

/// Local NATS subject for sending org-scoped system responses back to the backend.
/// `responses.<org_id>`
pub fn response_org_subject(org_id: Uuid) -> String {
    format!("responses.{org_id}")
}

/// Local NATS subject for sending realtime chat events back to the backend.
/// `streams.chat.<session_id>`
pub fn chat_stream_subject(session_id: Uuid) -> String {
    format!("streams.chat.{session_id}")
}

/// Resolve the local subject for a response.
pub fn response_subject(org_id: Uuid, user_id: Uuid, response: &Response) -> String {
    match response {
        Response::AgentResponse {
            session_id: Some(session_id),
            ..
        } => chat_stream_subject(*session_id),
        _ => response_user_subject(org_id, user_id),
    }
}

/// The JetStream stream name workers consume commands from.
pub const REQUESTS_STREAM_NAME: &str = "AGENT_WORK_REQUESTS";

/// The JetStream stream name workers consume fanout commands from.
pub const BROADCAST_REQUESTS_STREAM_NAME: &str = "AGENT_BROADCAST_REQUESTS";

/// Stream subjects for the requests stream.
pub const REQUESTS_STREAM_SUBJECTS: &[&str] = &["work_requests.*", "worker_requests.*.*"];

/// Stream subjects for the broadcast requests stream.
pub const BROADCAST_REQUESTS_STREAM_SUBJECTS: &[&str] = &["broadcast_requests.*"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_subject_format() {
        assert_eq!(requests_subject(Capability::Chat), "work_requests.chat");
    }

    #[test]
    fn requests_subject_all_format() {
        assert_eq!(requests_subject_all(), "work_requests.>");
    }

    #[test]
    fn worker_requests_subject_format() {
        let worker_id = Uuid::nil();
        assert_eq!(
            worker_requests_subject(worker_id, Capability::Task),
            format!("worker_requests.{worker_id}.task")
        );
        assert_eq!(
            worker_requests_subject_all(worker_id),
            format!("worker_requests.{worker_id}.>")
        );
    }

    #[test]
    fn broadcast_requests_subject_all_format() {
        assert_eq!(broadcast_requests_subject_all(), "broadcast_requests.>");
    }

    #[test]
    fn responses_subject_format() {
        let org_id = Uuid::nil();
        assert_eq!(responses_subject(org_id), format!("responses.{org_id}"));
    }

    #[test]
    fn response_user_subject_format() {
        let org_id = Uuid::nil();
        let user_id = Uuid::from_u128(1);
        assert_eq!(
            response_user_subject(org_id, user_id),
            format!("responses.{org_id}.{user_id}")
        );
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
