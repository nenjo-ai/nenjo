//! Responses sent from the harness to the backend (`agent.responses.<user_id>`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Capability;

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
    AgentResponse { payload: StreamEvent },

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
    },

    /// Periodic heartbeat from the cron scheduler reporting active schedules.
    #[serde(rename = "cron.heartbeat")]
    CronHeartbeat {
        active_schedules: Vec<CronScheduleStatus>,
    },

    /// Signals that a new execution run is starting (e.g. a cron cycle).
    /// The worker pre-generates the UUID; the backend creates the row.
    #[serde(rename = "execution.started")]
    ExecutionStarted {
        id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<Uuid>,
        routine_id: Uuid,
        routine_name: String,
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
            Self::AgentResponse { payload } => write!(f, "agent_response({payload})"),
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
            Self::ExecutionStarted {
                id, routine_name, ..
            } => {
                write!(f, "execution.started(id={id}, routine={routine_name})")
            }
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

/// Events streamed during agent execution, delivered to the frontend in
/// real time via WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "data")]
pub enum StreamEvent {
    /// An incremental LLM output token.
    Token { text: String },

    /// A single tool invocation.
    ToolInvoked {
        tool_name: String,
        tool_args: String,
        agent_name: String,
    },

    /// Batch tool invocation (parallel tool calls).
    ToolsInvoked {
        tool_names: Vec<String>,
        tool_args: Vec<String>,
        agent_name: String,
    },

    /// An ability was activated for an agent.
    AbilityActivated {
        agent: String,
        ability: String,
        task_preview: String,
    },

    /// An ability finished executing.
    AbilityCompleted {
        agent: String,
        ability: String,
        success: bool,
        result_preview: String,
    },

    /// An error occurred during execution.
    Error { message: String },

    /// Execution completed successfully.
    Done { final_output: String },

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
            Self::Token { text } => write!(f, "token({}B)", text.len()),
            Self::ToolInvoked {
                tool_name,
                agent_name,
                ..
            } => write!(f, "tool_invoked({tool_name}, agent={agent_name})"),
            Self::ToolsInvoked {
                tool_names,
                agent_name,
                ..
            } => write!(
                f,
                "tools_invoked([{}], agent={agent_name})",
                tool_names.join(", ")
            ),
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
            Self::Error { message } => write!(f, "error({message})"),
            Self::Done { .. } => write!(f, "done"),
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

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Status of a single cron schedule in a heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronScheduleStatus {
    /// The cron assignment this status refers to.
    pub assignment_id: String,
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
        }
    }

    /// Build a `task.completed` response.
    pub fn task_completed(
        execution_run_id: Uuid,
        task_id: Option<Uuid>,
        success: bool,
        error: Option<String>,
        merge_error: Option<String>,
    ) -> Self {
        Self::TaskCompleted {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            success,
            error,
            merge_error,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Envelope};

    #[test]
    fn stream_event_token_roundtrip() {
        let event = StreamEvent::Token {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event_type":"Token""#));
        let parsed: StreamEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            StreamEvent::Token { text } => assert_eq!(text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn stream_event_tool_invoked_roundtrip() {
        let event = StreamEvent::ToolInvoked {
            tool_name: "shell".into(),
            tool_args: r#"{"cmd":"ls"}"#.into(),
            agent_name: "coder".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: StreamEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            StreamEvent::ToolInvoked {
                tool_name,
                agent_name,
                ..
            } => {
                assert_eq!(tool_name, "shell");
                assert_eq!(agent_name, "coder");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_chat_message_roundtrip() {
        let cmd = Command::ChatMessage {
            id: Some("msg-123".into()),
            content: "hello".into(),
            project_id: None,
            routine_id: None,
            agent_id: Some(Uuid::nil()),
            domain_session_id: None,
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"chat.message""#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::ChatMessage { content, .. } => assert_eq!(content, "hello"),
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
        let resp = Response::task_completed(Uuid::nil(), None, true, None, None);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"task.completed""#));
        assert!(json.contains(r#""success":true"#));
    }

    #[test]
    fn response_agent_response_roundtrip() {
        let resp = Response::AgentResponse {
            payload: StreamEvent::Done {
                final_output: "result".into(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"agent_response""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::AgentResponse { payload } => match payload {
                StreamEvent::Done { final_output } => assert_eq!(final_output, "result"),
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
            routine_id: Uuid::nil(),
            routine_name: "deploy".into(),
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
                assert_eq!(routine_name, "deploy");
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
                assignment_id: "a1".into(),
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
                assert_eq!(active_schedules[0].assignment_id, "a1");
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
            title: "Fix bug".into(),
            description: Some("In auth module".into()),
            slug: None,
            acceptance_criteria: None,
            tags: vec!["urgent".into()],
            status: None,
            priority: None,
            task_type: None,
            complexity: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"task.execute""#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::TaskExecute { title, tags, .. } => {
                assert_eq!(title, "Fix bug");
                assert_eq!(tags, vec!["urgent"]);
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
            assignment_id: Uuid::nil(),
            routine_id: Uuid::nil(),
            project_id: Uuid::nil(),
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
