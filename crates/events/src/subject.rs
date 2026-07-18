//! NATS subject helpers.
//!
//! With per-org NATS accounts, workers see org-routed local subjects.
//!
//! | PLATFORM subject                        | Worker local subject       |
//! |-----------------------------------------|----------------------------|
//! | `work_requests.<org_id>.>`              | `work_requests.<org_id>.>` |
//! | `worker_requests.<org_id>.<worker_id>.>` | `worker_requests.<org_id>.<worker_id>.>` |
//! | `broadcast_requests.<org_id>.>`         | `broadcast_requests.<org_id>.>` |
//! | `streams.chat.<org_id>.<session_id>`    | `streams.chat.<org_id>.<session_id>` |
//! | `streams.execution.<org_id>.<execution_run_id>` | `streams.execution.<org_id>.<execution_run_id>` |

use uuid::Uuid;

use crate::{Capability, Response};

// ---------------------------------------------------------------------------
// Worker-local subjects (used by the worker/harness)
// ---------------------------------------------------------------------------

/// Local NATS subject for receiving commands for a specific capability.
/// `work_requests.<org_id>.<capability>`
pub fn requests_subject(org_id: Uuid, capability: Capability) -> String {
    format!("work_requests.{org_id}.{capability}")
}

/// Local NATS wildcard subject for all capabilities.
/// `work_requests.<org_id>.>`
pub fn requests_subject_all(org_id: Uuid) -> String {
    format!("work_requests.{org_id}.>")
}

/// Local NATS subject for receiving commands targeted to one worker.
/// `worker_requests.<org_id>.<worker_id>.<capability>`
pub fn worker_requests_subject(org_id: Uuid, worker_id: Uuid, capability: Capability) -> String {
    format!("worker_requests.{org_id}.{worker_id}.{capability}")
}

/// Local NATS subject for fanout commands for a specific capability.
/// `broadcast_requests.<org_id>.<capability>`
pub fn broadcast_requests_subject(org_id: Uuid, capability: Capability) -> String {
    format!("broadcast_requests.{org_id}.{capability}")
}

/// Local NATS wildcard subject for commands targeted to one worker.
/// `worker_requests.<org_id>.<worker_id>.>`
pub fn worker_requests_subject_all(org_id: Uuid, worker_id: Uuid) -> String {
    format!("worker_requests.{org_id}.{worker_id}.>")
}

/// Local NATS wildcard subject for fanout worker commands.
/// `broadcast_requests.<org_id>.>`
pub fn broadcast_requests_subject_all(org_id: Uuid) -> String {
    format!("broadcast_requests.{org_id}.>")
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
/// `streams.chat.<org_id>.<session_id>`
pub fn chat_stream_subject(org_id: Uuid, session_id: Uuid) -> String {
    format!("streams.chat.{org_id}.{session_id}")
}

/// Local NATS subject for sending execution progress stream events.
/// `streams.execution.<org_id>.<execution_run_id>`
pub fn execution_stream_subject(org_id: Uuid, execution_run_id: Uuid) -> String {
    format!("streams.execution.{org_id}.{execution_run_id}")
}

/// Resolve the local subject for a response.
pub fn response_subject(org_id: Uuid, user_id: Uuid, response: &Response) -> String {
    match response {
        Response::WorkerHeartbeat { .. }
        | Response::WorkerRegistered { .. }
        | Response::RepoSyncComplete { .. }
        | Response::TaskExecutionState { .. } => response_org_subject(org_id),
        Response::ExecutionEvent {
            execution_run_id, ..
        } => format!("streams.execution.{org_id}.{execution_run_id}"),
        Response::AgentResponse {
            session_id: Some(session_id),
            ..
        } => chat_stream_subject(org_id, *session_id),
        _ => response_user_subject(org_id, user_id),
    }
}

/// The JetStream stream name workers consume commands from.
pub const REQUESTS_STREAM_NAME: &str = "AGENT_WORK_REQUESTS";

/// The JetStream stream name workers consume fanout commands from.
pub const BROADCAST_REQUESTS_STREAM_NAME: &str = "AGENT_BROADCAST_REQUESTS";

/// Stream subjects for the requests stream.
pub const REQUESTS_STREAM_SUBJECTS: &[&str] = &["work_requests.*.*", "worker_requests.*.*.*"];

/// Stream subjects for the broadcast requests stream.
pub const BROADCAST_REQUESTS_STREAM_SUBJECTS: &[&str] = &["broadcast_requests.*.*"];

/// Worker consumer filter for shared work commands.
pub const REQUESTS_CONSUMER_FILTER_SUBJECT: &str = "work_requests.*.*";

/// Worker consumer filters for fanout commands.
pub const BROADCAST_REQUESTS_CONSUMER_FILTER_SUBJECTS: &[&str] = &[
    "broadcast_requests.*.manifest",
    "broadcast_requests.*.repo",
    "broadcast_requests.*.ping",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_subject_format() {
        let org_id = Uuid::from_u128(1);
        assert_eq!(
            requests_subject(org_id, Capability::Chat),
            format!("work_requests.{org_id}.chat")
        );
    }

    #[test]
    fn requests_subject_all_format() {
        let org_id = Uuid::from_u128(1);
        assert_eq!(
            requests_subject_all(org_id),
            format!("work_requests.{org_id}.>")
        );
    }

    #[test]
    fn worker_requests_subject_format() {
        let org_id = Uuid::from_u128(1);
        let worker_id = Uuid::nil();
        assert_eq!(
            worker_requests_subject(org_id, worker_id, Capability::Task),
            format!("worker_requests.{org_id}.{worker_id}.task")
        );
        assert_eq!(
            worker_requests_subject_all(org_id, worker_id),
            format!("worker_requests.{org_id}.{worker_id}.>")
        );
    }

    #[test]
    fn broadcast_requests_subject_all_format() {
        let org_id = Uuid::from_u128(1);
        assert_eq!(
            broadcast_requests_subject_all(org_id),
            format!("broadcast_requests.{org_id}.>")
        );
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
    fn execution_stream_subject_format() {
        let org_id = Uuid::nil();
        let execution_run_id = Uuid::from_u128(1);
        assert_eq!(
            execution_stream_subject(org_id, execution_run_id),
            format!("streams.execution.{org_id}.{execution_run_id}")
        );
    }

    #[test]
    fn worker_presence_responses_are_org_scoped() {
        let org_id = Uuid::from_u128(1);
        let user_id = Uuid::from_u128(2);
        let worker_id = Uuid::from_u128(3);

        assert_eq!(
            response_subject(
                org_id,
                user_id,
                &Response::WorkerHeartbeat {
                    worker_id,
                    capabilities: vec![Capability::Chat],
                    version: None,
                },
            ),
            format!("responses.{org_id}")
        );
        assert_eq!(
            response_subject(
                org_id,
                user_id,
                &Response::WorkerRegistered {
                    worker_id,
                    capabilities: vec![Capability::Chat],
                    version: None,
                },
            ),
            format!("responses.{org_id}")
        );
    }

    #[test]
    fn repo_sync_complete_response_is_org_scoped() {
        let org_id = Uuid::from_u128(1);
        let user_id = Uuid::from_u128(2);

        assert_eq!(
            response_subject(
                org_id,
                user_id,
                &Response::RepoSyncComplete {
                    project: "demo-project".to_string(),
                    success: true,
                    error: None,
                },
            ),
            format!("responses.{org_id}")
        );
    }

    #[test]
    fn execution_responses_are_execution_stream_scoped() {
        let org_id = Uuid::from_u128(1);
        let user_id = Uuid::from_u128(2);
        let execution_run_id = Uuid::from_u128(3);

        assert_eq!(
            response_subject(
                org_id,
                user_id,
                &Response::ExecutionEvent {
                    execution_run_id: execution_run_id.to_string(),
                    task_id: None,
                    event: crate::ExecutionEventPayload::WorkflowStep(
                        crate::ExecutionWorkflowStepEvent {
                            event_type: "step_started".to_string(),
                            step_name: "plan".to_string(),
                            step_type: "agent".to_string(),
                            duration_ms: None,
                            data: serde_json::json!({}),
                            payload: None,
                            encrypted_payload: None,
                            agent: None,
                        },
                    ),
                },
            ),
            format!("streams.execution.{org_id}.{execution_run_id}")
        );
    }

    #[test]
    fn execution_response_subject_does_not_fallback_to_actor_subject() {
        let org_id = Uuid::from_u128(1);
        let user_id = Uuid::from_u128(2);
        let execution_run_id = "invalid-run-id";

        assert_eq!(
            response_subject(
                org_id,
                user_id,
                &Response::ExecutionEvent {
                    execution_run_id: execution_run_id.to_string(),
                    task_id: None,
                    event: crate::ExecutionEventPayload::TaskArtifacts(
                        crate::ExecutionTaskArtifactsEvent {
                            total_input_tokens: 0,
                            total_output_tokens: 0,
                            attachments: Vec::new(),
                        },
                    ),
                },
            ),
            format!("streams.execution.{org_id}.{execution_run_id}")
        );
    }

    #[test]
    fn chat_stream_subject_format() {
        let org_id = Uuid::from_u128(1);
        let session_id = Uuid::nil();
        assert_eq!(
            chat_stream_subject(org_id, session_id),
            format!("streams.chat.{org_id}.{session_id}")
        );
    }
}
