//! NATS subject helpers.

use uuid::Uuid;

use crate::Capability;

/// NATS subject for commands sent to a specific capability
/// (backend → harness): `agent.requests.<user_id>.<capability>`.
pub fn requests_subject(user_id: Uuid, capability: Capability) -> String {
    format!("agent.requests.{user_id}.{capability}")
}

/// NATS wildcard subject for all capabilities of a user
/// (backend → harness): `agent.requests.<user_id>.*`.
///
/// Used by workers that handle all capabilities (e.g. the full runner).
pub fn requests_subject_all(user_id: Uuid) -> String {
    format!("agent.requests.{user_id}.*")
}

/// NATS subject for responses sent from the harness (harness → backend).
pub fn responses_subject(user_id: Uuid) -> String {
    format!("agent.responses.{user_id}")
}

/// The JetStream stream name that backs agent events.
pub const STREAM_NAME: &str = "AGENT_EVENTS";

/// Subject filter for the JetStream stream — matches all `agent.*` subjects.
pub const STREAM_SUBJECT: &str = "agent.>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_subject_format() {
        let id = Uuid::nil();
        assert_eq!(
            requests_subject(id, Capability::Chat),
            "agent.requests.00000000-0000-0000-0000-000000000000.chat"
        );
    }

    #[test]
    fn requests_subject_all_format() {
        let id = Uuid::nil();
        assert_eq!(
            requests_subject_all(id),
            "agent.requests.00000000-0000-0000-0000-000000000000.*"
        );
    }

    #[test]
    fn responses_subject_format() {
        let id = Uuid::nil();
        assert_eq!(
            responses_subject(id),
            "agent.responses.00000000-0000-0000-0000-000000000000"
        );
    }
}
