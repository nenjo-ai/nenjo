//! Responses sent from the harness to the backend (`responses`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Capability, EncryptedPayload};

// ---------------------------------------------------------------------------
// Execution type
// ---------------------------------------------------------------------------

/// Distinguishes how an execution was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionType {
    Cron,
    Task,
    Heartbeat,
}

/// Agent identity attached to step events so the frontend can render identicons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepAgent {
    pub agent_id: Uuid,
    pub agent_name: Option<String>,
    pub agent_color: Option<String>,
}

// ---------------------------------------------------------------------------
// Top-level response wrapper
// ---------------------------------------------------------------------------

/// A response sent from the harness back to the backend.
///
/// Discriminated by the `type` field in JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    /// Wraps a [`StreamEvent`] for real-time streaming to the frontend.
    #[serde(rename = "agent_response")]
    AgentResponse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
        payload: StreamEvent,
    },

    /// A routine step lifecycle event (started, completed, failed, etc.).
    #[serde(rename = "task.step_event")]
    TaskStepEvent {
        execution_run_id: String,
        #[serde(default)]
        task_id: Option<String>,
        /// One of: `step_started`, `step_completed`, `step_failed`,
        /// `step_warning`, `progress`.
        event_type: String,
        step_name: String,
        step_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default)]
        data: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
        /// Agent executing this step (if it's an agent step).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<StepAgent>,
    },

    /// Signals that a task execution finished.
    #[serde(rename = "task.completed")]
    TaskCompleted {
        execution_run_id: String,
        #[serde(default)]
        task_id: Option<String>,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge_error: Option<String>,
        #[serde(default)]
        total_input_tokens: u64,
        #[serde(default)]
        total_output_tokens: u64,
    },

    /// Periodic heartbeat from the cron scheduler reporting active schedules.
    #[serde(rename = "cron.heartbeat")]
    CronHeartbeat {
        active_schedules: Vec<CronScheduleStatus>,
    },

    /// Confirms that a cron schedule was enabled by the worker.
    #[serde(rename = "cron.scheduled")]
    CronScheduled {
        routine_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_run_at: Option<String>,
    },

    /// Confirms that a cron schedule was disabled by the worker.
    #[serde(rename = "cron.stopped")]
    CronStopped { routine_id: Uuid },

    /// Periodic heartbeat from the agent heartbeat scheduler.
    #[serde(rename = "agent_heartbeat.heartbeat")]
    AgentHeartbeatHeartbeat {
        agent_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_run_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_run_at: Option<String>,
    },

    /// Confirms that an agent heartbeat schedule was enabled by the worker.
    #[serde(rename = "agent_heartbeat.scheduled")]
    AgentHeartbeatScheduled {
        agent_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_run_at: Option<String>,
    },

    /// Confirms that an agent heartbeat schedule was disabled by the worker.
    #[serde(rename = "agent_heartbeat.stopped")]
    AgentHeartbeatStopped { agent_id: Uuid },

    /// Signals that a new execution run is starting (e.g. a cron cycle).
    /// The worker pre-generates the UUID; the backend creates the row.
    #[serde(rename = "execution.started")]
    ExecutionStarted {
        id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        routine_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        routine_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<Uuid>,
        config: serde_json::Value,
    },

    /// Signals that an execution run finished.
    #[serde(rename = "execution.completed")]
    ExecutionCompleted {
        id: Uuid,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default)]
        total_input_tokens: u64,
        #[serde(default)]
        total_output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_type: Option<ExecutionType>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        routine_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        routine_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<Uuid>,
    },

    /// Repo sync completed (or failed) for a project.
    #[serde(rename = "repo.sync_complete")]
    RepoSyncComplete {
        project_id: Uuid,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Confirms receipt of a command (sent after processing begins).
    #[serde(rename = "delivery_receipt")]
    DeliveryReceipt { message_id: String },

    /// Worker presence heartbeat — sent on startup and periodically.
    /// The backend uses this to set the Redis presence key.
    #[serde(rename = "worker.heartbeat")]
    WorkerHeartbeat {
        /// Unique instance ID for this worker process (generated at startup).
        worker_id: Uuid,
        /// Capabilities this worker handles.
        capabilities: Vec<Capability>,
        /// Application version (e.g. "0.1.0"). Used by the backend for
        /// backward compatibility decisions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },

    /// Response to a `worker.ping` command — proves the worker is alive.
    #[serde(rename = "worker.pong")]
    WorkerPong,

    /// Sent once on initial connection to register the worker with the backend.
    #[serde(rename = "worker.registered")]
    WorkerRegistered {
        /// Unique instance ID for this worker process.
        worker_id: Uuid,
        /// Capabilities this worker handles.
        capabilities: Vec<Capability>,
        /// Application version (e.g. "0.1.0"). Used by the backend for
        /// backward compatibility decisions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentResponse {
                session_id,
                payload,
            } => match session_id {
                Some(session_id) => write!(f, "agent_response(session={session_id}, {payload})"),
                None => write!(f, "agent_response({payload})"),
            },
            Self::TaskStepEvent {
                execution_run_id,
                event_type,
                step_name,
                ..
            } => {
                write!(
                    f,
                    "task.step_event(run={execution_run_id}, {event_type}, step={step_name})"
                )
            }
            Self::TaskCompleted {
                execution_run_id,
                success,
                ..
            } => {
                write!(
                    f,
                    "task.completed(run={execution_run_id}, success={success})"
                )
            }
            Self::CronHeartbeat { active_schedules } => {
                write!(f, "cron.heartbeat(schedules={})", active_schedules.len())
            }
            Self::CronScheduled {
                routine_id,
                next_run_at,
            } => {
                write!(
                    f,
                    "cron.scheduled(routine={routine_id}, next_run_at={})",
                    next_run_at.as_deref().unwrap_or("none")
                )
            }
            Self::CronStopped { routine_id } => write!(f, "cron.stopped(routine={routine_id})"),
            Self::AgentHeartbeatHeartbeat {
                agent_id,
                next_run_at,
                ..
            } => write!(
                f,
                "agent_heartbeat.heartbeat(agent={agent_id}, next_run_at={})",
                next_run_at.as_deref().unwrap_or("none")
            ),
            Self::AgentHeartbeatScheduled {
                agent_id,
                next_run_at,
            } => write!(
                f,
                "agent_heartbeat.scheduled(agent={agent_id}, next_run_at={})",
                next_run_at.as_deref().unwrap_or("none")
            ),
            Self::AgentHeartbeatStopped { agent_id } => {
                write!(f, "agent_heartbeat.stopped(agent={agent_id})")
            }
            Self::ExecutionStarted {
                id,
                routine_name,
                agent_id,
                ..
            } => match (routine_name, agent_id) {
                (Some(routine_name), _) => {
                    write!(f, "execution.started(id={id}, routine={routine_name})")
                }
                (None, Some(agent_id)) => write!(f, "execution.started(id={id}, agent={agent_id})"),
                (None, None) => write!(f, "execution.started(id={id})"),
            },
            Self::ExecutionCompleted { id, success, .. } => {
                write!(f, "execution.completed(id={id}, success={success})")
            }
            Self::RepoSyncComplete {
                project_id,
                success,
                ..
            } => {
                write!(
                    f,
                    "repo.sync_complete(project={project_id}, success={success})"
                )
            }
            Self::DeliveryReceipt { message_id } => write!(f, "delivery_receipt({message_id})"),
            Self::WorkerPong => write!(f, "worker.pong"),
            Self::WorkerHeartbeat { worker_id, .. } => {
                write!(f, "worker.heartbeat(worker={worker_id})")
            }
            Self::WorkerRegistered { worker_id, .. } => {
                write!(f, "worker.registered(worker={worker_id})")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stream events (real-time agent execution)
// ---------------------------------------------------------------------------

/// Events streamed during agent execution and bridged to clients by the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "data")]
pub enum StreamEvent {
    /// One or more tool invocations.
    ToolCalls {
        tool_calls: Vec<ToolCall>,
        agent_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// A single tool invocation completed.
    ToolCompleted {
        tool_name: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// An ability was activated for an agent.
    AbilityActivated {
        agent: String,
        ability: String,
        ability_tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// An ability finished executing.
    AbilityCompleted {
        agent: String,
        ability: String,
        ability_tool_name: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// A delegation to another agent was started.
    DelegationStarted {
        agent: String,
        target_agent: String,
        target_agent_id: Uuid,
        delegate_tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// A delegation to another agent finished.
    DelegationCompleted {
        agent: String,
        target_agent: String,
        target_agent_id: Uuid,
        delegate_tool_name: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// An error occurred during execution.
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// Execution completed successfully.
    Done {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
        #[serde(default)]
        total_input_tokens: u64,
        #[serde(default)]
        total_output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
    },

    /// A domain session was entered.
    DomainEntered {
        session_id: Uuid,
        domain_name: String,
    },

    /// A domain session was exited.
    DomainExited {
        session_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        document_id: Option<Uuid>,
    },

    /// Chat history was compacted via LLM summarization.
    MessageCompacted {
        messages_before: usize,
        messages_after: usize,
    },

    /// Execution was paused (agent will stop before the next LLM call).
    Paused,

    /// Execution was resumed after a pause.
    Resumed,
}

impl std::fmt::Display for StreamEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolCalls {
                tool_calls,
                agent_name,
                ..
            } => write!(
                f,
                "tool_calls([{}], agent={agent_name})",
                tool_calls
                    .iter()
                    .map(|call| call.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::ToolCompleted {
                tool_name, success, ..
            } => write!(f, "tool_completed({tool_name}, success={success})"),
            Self::AbilityActivated { agent, ability, .. } => {
                write!(f, "ability_activated({ability}, agent={agent})")
            }
            Self::AbilityCompleted {
                agent,
                ability,
                success,
                ..
            } => write!(
                f,
                "ability_completed({ability}, agent={agent}, success={success})"
            ),
            Self::DelegationStarted {
                agent,
                target_agent,
                ..
            } => write!(f, "delegation_started({target_agent}, agent={agent})"),
            Self::DelegationCompleted {
                agent,
                target_agent,
                success,
                ..
            } => write!(
                f,
                "delegation_completed({target_agent}, agent={agent}, success={success})"
            ),
            Self::Error { message, .. } => write!(f, "error({message})"),
            Self::Done {
                payload,
                encrypted_payload,
                ..
            } => write!(
                f,
                "done(payload={}, encrypted={})",
                if payload.is_some() { "yes" } else { "no" },
                if encrypted_payload.is_some() {
                    "yes"
                } else {
                    "no"
                }
            ),
            Self::DomainEntered {
                session_id,
                domain_name,
            } => write!(f, "domain_entered({domain_name}, session={session_id})"),
            Self::DomainExited { session_id, .. } => {
                write!(f, "domain_exited(session={session_id})")
            }
            Self::MessageCompacted {
                messages_before,
                messages_after,
            } => write!(f, "message_compacted({messages_before}->{messages_after})"),
            Self::Paused => write!(f, "paused"),
            Self::Resumed => write!(f, "resumed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub tool_args: String,
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Status of a single cron schedule in a heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronScheduleStatus {
    /// The routine this status refers to.
    pub routine_id: String,
    /// ISO 8601 timestamp of the last successful run, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// ISO 8601 timestamp of the next scheduled fire time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_fire_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Builder helpers
// ---------------------------------------------------------------------------

impl Response {
    /// Build a `task.step_event` response.
    pub fn step_event(
        execution_run_id: Uuid,
        task_id: Option<Uuid>,
        event_type: impl Into<String>,
        step_name: impl Into<String>,
        step_type: impl Into<String>,
        duration_ms: Option<u64>,
        data: serde_json::Value,
    ) -> Self {
        Self::TaskStepEvent {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            event_type: event_type.into(),
            step_name: step_name.into(),
            step_type: step_type.into(),
            duration_ms,
            data,
            payload: None,
            encrypted_payload: None,
            agent: None,
        }
    }

    /// Build a `task.completed` response.
    pub fn task_completed(
        execution_run_id: Uuid,
        task_id: Option<Uuid>,
        success: bool,
        error: Option<String>,
        merge_error: Option<String>,
        total_input_tokens: u64,
        total_output_tokens: u64,
    ) -> Self {
        Self::TaskCompleted {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            success,
            error,
            merge_error,
            total_input_tokens,
            total_output_tokens,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, EncryptedPayload, Envelope};

    #[test]
    fn stream_event_tool_calls_roundtrip() {
        let event = StreamEvent::ToolCalls {
            tool_calls: vec![ToolCall {
                tool_name: "shell".into(),
                tool_args: r#"{"cmd":"ls"}"#.into(),
            }],
            agent_name: "coder".into(),
            parent_tool_name: Some("ability/test.builder".into()),
            payload: None,
            encrypted_payload: Some(EncryptedPayload {
                account_id: Uuid::nil(),
                encryption_scope: None,
                object_id: Uuid::new_v4(),
                object_type: "tool_call_preview".into(),
                algorithm: "aes-256-gcm".into(),
                key_version: 1,
                nonce: "bm9uY2U=".into(),
                ciphertext: "Y2lwaGVydGV4dA==".into(),
            }),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: StreamEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            StreamEvent::ToolCalls {
                tool_calls,
                agent_name,
                parent_tool_name,
                encrypted_payload,
                ..
            } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool_name, "shell");
                assert_eq!(agent_name, "coder");
                assert_eq!(parent_tool_name.as_deref(), Some("ability/test.builder"));
                assert!(encrypted_payload.is_some());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn stream_event_tool_completed_roundtrip() {
        let event = StreamEvent::ToolCompleted {
            tool_name: "shell".into(),
            success: false,
            parent_tool_name: Some("ability/test.builder".into()),
            payload: None,
            encrypted_payload: Some(EncryptedPayload {
                account_id: Uuid::nil(),
                encryption_scope: None,
                object_id: Uuid::new_v4(),
                object_type: "tool_error_preview".into(),
                algorithm: "aes-256-gcm".into(),
                key_version: 1,
                nonce: "bm9uY2U=".into(),
                ciphertext: "Y2lwaGVydGV4dA==".into(),
            }),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: StreamEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            StreamEvent::ToolCompleted {
                tool_name,
                success,
                parent_tool_name,
                encrypted_payload,
                ..
            } => {
                assert_eq!(tool_name, "shell");
                assert!(!success);
                assert_eq!(parent_tool_name.as_deref(), Some("ability/test.builder"));
                assert!(encrypted_payload.is_some());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_chat_message_roundtrip() {
        let cmd = Command::ChatMessage {
            id: Some("msg-123".into()),
            content: "hello".into(),
            encrypted_content: None,
            hidden: true,
            project_id: None,
            routine_id: None,
            agent_id: Some(Uuid::nil()),
            domain_session_id: None,
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"chat.message""#));
        assert!(json.contains(r#""hidden":true"#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::ChatMessage {
                content, hidden, ..
            } => {
                assert_eq!(content, "hello");
                assert!(hidden);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_chat_message_with_encrypted_content_roundtrip() {
        let payload = EncryptedPayload {
            account_id: Uuid::nil(),
            encryption_scope: None,
            object_id: Uuid::new_v4(),
            object_type: "agent_prompt".into(),
            algorithm: "aes-256-gcm".into(),
            key_version: 1,
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVydGV4dA==".into(),
        };
        let cmd = Command::ChatMessage {
            id: None,
            content: String::new(),
            encrypted_content: Some(payload.clone()),
            hidden: false,
            project_id: None,
            routine_id: None,
            agent_id: None,
            domain_session_id: None,
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""encrypted_content""#));

        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::ChatMessage {
                encrypted_content, ..
            } => {
                let parsed_payload = encrypted_content.expect("encrypted content should exist");
                assert_eq!(parsed_payload.account_id, payload.account_id);
                assert_eq!(parsed_payload.object_id, payload.object_id);
                assert_eq!(parsed_payload.object_type, payload.object_type);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_step_event_builder() {
        let resp = Response::step_event(
            Uuid::nil(),
            Some(Uuid::nil()),
            "step_started",
            "Implementation",
            "agent",
            None,
            serde_json::json!({}),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"task.step_event""#));
        assert!(json.contains(r#""event_type":"step_started""#));
    }

    #[test]
    fn response_task_completed_builder() {
        let resp = Response::task_completed(Uuid::nil(), None, true, None, None, 100, 50);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"task.completed""#));
        assert!(json.contains(r#""success":true"#));
    }

    #[test]
    fn response_agent_response_roundtrip() {
        let resp = Response::AgentResponse {
            session_id: Some(Uuid::nil()),
            payload: StreamEvent::Done {
                payload: Some(serde_json::Value::String("result".into())),
                encrypted_payload: None,
                total_input_tokens: 0,
                total_output_tokens: 0,
                project_id: None,
                agent_id: None,
                session_id: None,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"agent_response""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::AgentResponse {
                session_id,
                payload,
            } => match payload {
                StreamEvent::Done {
                    payload,
                    encrypted_payload,
                    ..
                } => {
                    assert_eq!(session_id, Some(Uuid::nil()));
                    assert_eq!(
                        payload.as_ref().and_then(|value| value.as_str()),
                        Some("result")
                    );
                    assert!(encrypted_payload.is_none());
                }
                _ => panic!("wrong stream event"),
            },
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_agent_response_with_encrypted_done_roundtrip() {
        let resp = Response::AgentResponse {
            session_id: Some(Uuid::nil()),
            payload: StreamEvent::Done {
                payload: Some(serde_json::Value::String("compat".into())),
                encrypted_payload: Some(EncryptedPayload {
                    account_id: Uuid::nil(),
                    encryption_scope: None,
                    object_id: Uuid::new_v4(),
                    object_type: "agent_response".into(),
                    algorithm: "aes-256-gcm".into(),
                    key_version: 1,
                    nonce: "bm9uY2U=".into(),
                    ciphertext: "Y2lwaGVydGV4dA==".into(),
                }),
                total_input_tokens: 0,
                total_output_tokens: 0,
                project_id: None,
                agent_id: None,
                session_id: None,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""encrypted_payload""#));

        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::AgentResponse { payload, .. } => match payload {
                StreamEvent::Done {
                    encrypted_payload, ..
                } => {
                    assert!(encrypted_payload.is_some());
                }
                _ => panic!("wrong stream event"),
            },
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_worker_heartbeat_roundtrip() {
        let resp = Response::WorkerHeartbeat {
            worker_id: Uuid::nil(),
            capabilities: vec![crate::Capability::Chat, crate::Capability::Task],
            version: Some("0.1.0".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"worker.heartbeat""#));
        assert!(json.contains(r#""worker_id""#));
        assert!(json.contains(r#""capabilities""#));
        assert!(json.contains(r#""version":"0.1.0""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::WorkerHeartbeat {
                worker_id,
                capabilities,
                version,
            } => {
                assert_eq!(worker_id, Uuid::nil());
                assert_eq!(capabilities.len(), 2);
                assert_eq!(version.as_deref(), Some("0.1.0"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_worker_heartbeat_without_version() {
        // Backward compat: old workers don't send version.
        let json = r#"{"type":"worker.heartbeat","worker_id":"00000000-0000-0000-0000-000000000000","capabilities":["chat"]}"#;
        let parsed: Response = serde_json::from_str(json).unwrap();
        match parsed {
            Response::WorkerHeartbeat { version, .. } => {
                assert!(version.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_worker_registered_roundtrip() {
        let resp = Response::WorkerRegistered {
            worker_id: Uuid::nil(),
            capabilities: vec![crate::Capability::Manifest],
            version: Some("0.2.0".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"worker.registered""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::WorkerRegistered {
                capabilities,
                version,
                ..
            } => {
                assert_eq!(capabilities, vec![crate::Capability::Manifest]);
                assert_eq!(version.as_deref(), Some("0.2.0"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_execution_started_roundtrip() {
        let id = Uuid::new_v4();
        let resp = Response::ExecutionStarted {
            id,
            project_id: None,
            routine_id: Some(Uuid::nil()),
            routine_name: Some("deploy".into()),
            agent_id: None,
            config: serde_json::json!({}),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"execution.started""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::ExecutionStarted {
                id: parsed_id,
                routine_name,
                ..
            } => {
                assert_eq!(parsed_id, id);
                assert_eq!(routine_name.as_deref(), Some("deploy"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_execution_completed_roundtrip() {
        let resp = Response::ExecutionCompleted {
            id: Uuid::nil(),
            success: true,
            error: None,
            total_input_tokens: 1000,
            total_output_tokens: 500,
            execution_type: Some(ExecutionType::Task),
            routine_id: Some(Uuid::nil()),
            routine_name: Some("Test Routine".to_string()),
            agent_id: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"execution.completed""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::ExecutionCompleted {
                success,
                total_input_tokens,
                total_output_tokens,
                ..
            } => {
                assert!(success);
                assert_eq!(total_input_tokens, 1000);
                assert_eq!(total_output_tokens, 500);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_cron_heartbeat_roundtrip() {
        let resp = Response::CronHeartbeat {
            active_schedules: vec![CronScheduleStatus {
                routine_id: "r1".into(),
                last_run_at: Some("2026-01-01T00:00:00Z".into()),
                next_fire_at: None,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"cron.heartbeat""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::CronHeartbeat { active_schedules } => {
                assert_eq!(active_schedules.len(), 1);
                assert_eq!(active_schedules[0].routine_id, "r1");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_delivery_receipt_roundtrip() {
        let resp = Response::DeliveryReceipt {
            message_id: "msg-42".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"delivery_receipt""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::DeliveryReceipt { message_id } => assert_eq!(message_id, "msg-42"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_repo_sync_complete_roundtrip() {
        let resp = Response::RepoSyncComplete {
            project_id: Uuid::nil(),
            success: false,
            error: Some("clone failed".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::RepoSyncComplete { success, error, .. } => {
                assert!(!success);
                assert_eq!(error.unwrap(), "clone failed");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_task_execute_roundtrip() {
        let cmd = Command::TaskExecute {
            task_id: Uuid::nil(),
            project_id: Uuid::nil(),
            execution_run_id: Uuid::nil(),
            routine_id: None,
            assigned_agent_id: None,
            payload: Some(crate::TaskExecuteContent {
                title: "Fix bug".into(),
                description: Some("In auth module".into()),
                slug: None,
                acceptance_criteria: None,
                tags: vec!["urgent".into()],
                status: None,
                priority: None,
                task_type: None,
                complexity: None,
            }),
            encrypted_payload: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"task.execute""#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::TaskExecute {
                payload: Some(payload),
                ..
            } => {
                assert_eq!(payload.title, "Fix bug");
                assert_eq!(payload.tags, vec!["urgent"]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_manifest_changed_roundtrip() {
        let cmd = Command::ManifestChanged {
            resource_type: crate::ResourceType::Agent,
            resource_id: Uuid::nil(),
            action: crate::ResourceAction::Updated,
            project_id: None,
            payload: None,
            encrypted_payload: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"manifest.changed""#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::ManifestChanged {
                resource_type,
                action,
                ..
            } => {
                assert_eq!(resource_type, crate::ResourceType::Agent);
                assert_eq!(action, crate::ResourceAction::Updated);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_cron_enable_roundtrip() {
        let cmd = Command::CronEnable {
            routine_id: Uuid::nil(),
            project_id: None,
            schedule: "0 * * * *".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"cron.enable""#));
        let _: Command = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn envelope_roundtrip() {
        let cmd = Command::ExecutionCancel {
            execution_run_id: Uuid::nil(),
        };
        let payload = serde_json::to_value(&cmd).unwrap();
        let env = Envelope::new(Uuid::nil(), payload);
        let json = serde_json::to_string(&env).unwrap();
        let parsed: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message_id, env.message_id);
        assert_eq!(parsed.attempt, 1);
    }
}
