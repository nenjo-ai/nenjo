//! Commands sent from the backend to the harness.
//!
//! [`Command::capability`] selects the capability subject segment, and
//! [`Command::delivery`] selects queue, broadcast, or targeted delivery.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Capability, EncryptedPayload, TaskExecuteContent};

/// Transport delivery policy for a backend-to-worker command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandDelivery {
    /// Shared queue for one org/user and capability.
    Queue,
    /// Fanout queue for every subscribed worker in the route scope.
    Broadcast,
    /// Command must be addressed to one worker enrollment.
    Targeted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedAccountContentKey {
    pub key_version: u32,
    pub algorithm: String,
    pub ephemeral_public_key: String,
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A command dispatched to an agent harness.
///
/// Discriminated by the `type` field in JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    // -----------------------------------------------------------------
    // Chat
    // -----------------------------------------------------------------
    /// A user chat message to be processed by an agent.
    #[serde(rename = "chat.message")]
    ChatMessage {
        /// Client-generated message ID for delivery tracking.
        #[serde(default)]
        id: Option<String>,
        /// The user's message text.
        content: String,
        /// Optional encrypted content body. When present, workers should prefer this over `content`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<EncryptedPayload>,
        /// When true, persist for context/history but do not surface in normal chat views.
        #[serde(default)]
        hidden: bool,
        /// Target project for context scoping.
        #[serde(default)]
        project_id: Option<Uuid>,
        /// If set, routes to a specific routine instead of a chat agent.
        #[serde(default)]
        routine_id: Option<Uuid>,
        /// If set, routes to a specific agent; otherwise uses the default.
        #[serde(default)]
        agent_id: Option<Uuid>,
        /// Active domain session context, if any.
        #[serde(default)]
        domain_session_id: Option<Uuid>,
        /// Chat session scope.
        session_id: Uuid,
    },

    /// Enter a domain session (activates a structured interaction mode).
    #[serde(rename = "chat.domain_enter")]
    ChatDomainEnter {
        project_id: Uuid,
        agent_id: Uuid,
        domain_command: String,
        /// Session ID created by the backend API — the harness stores its
        /// domain runner under this key so it matches the frontend's state.
        session_id: Uuid,
    },

    /// Exit an active domain session.
    #[serde(rename = "chat.domain_exit")]
    ChatDomainExit {
        project_id: Uuid,
        agent_id: Uuid,
        domain_session_id: Uuid,
    },

    /// Cancel an in-flight chat response.
    #[serde(rename = "chat.cancel")]
    ChatCancel {
        project_id: Uuid,
        #[serde(default)]
        agent_id: Option<Uuid>,
    },

    /// Delete a chat session's local history.
    #[serde(rename = "chat.session_delete")]
    ChatSessionDelete {
        project_id: Uuid,
        agent_id: Uuid,
        session_id: Uuid,
    },

    // -----------------------------------------------------------------
    // Task execution
    // -----------------------------------------------------------------
    /// Execute a task from the execution queue.
    #[serde(rename = "task.execute")]
    TaskExecute {
        task_id: Uuid,
        project_id: Uuid,
        execution_run_id: Uuid,
        #[serde(default)]
        routine_id: Option<Uuid>,
        #[serde(default)]
        assigned_agent_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<TaskExecuteContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// Cancel a running execution.
    #[serde(rename = "execution.cancel")]
    ExecutionCancel { execution_run_id: Uuid },

    /// Pause a running execution. The agent stops before the next LLM call.
    /// In-flight tool executions finish first.
    #[serde(rename = "execution.pause")]
    ExecutionPause { execution_run_id: Uuid },

    /// Resume a paused execution.
    #[serde(rename = "execution.resume")]
    ExecutionResume { execution_run_id: Uuid },

    // -----------------------------------------------------------------
    // Repository
    // -----------------------------------------------------------------
    /// Clone/pull a project repository.
    #[serde(rename = "repo.sync")]
    RepoSync {
        project_id: Uuid,
        repo_url: String,
        /// Branch to sync. The clone/pull targets this branch.
        target_branch: String,
    },

    /// Remove a synced project repository.
    #[serde(rename = "repo.unsync")]
    RepoUnsync { project_id: Uuid },

    // -----------------------------------------------------------------
    // Cron scheduling
    // -----------------------------------------------------------------
    /// Enable a cron schedule for a routine.
    #[serde(rename = "cron.enable")]
    CronEnable {
        routine_id: Uuid,
        #[serde(default)]
        project_id: Option<Uuid>,
        schedule: String,
        #[serde(default)]
        timezone: Option<String>,
    },

    /// Disable a cron schedule.
    #[serde(rename = "cron.disable")]
    CronDisable { routine_id: Uuid },

    /// Trigger a routine immediately (manual or test run).
    #[serde(rename = "cron.trigger")]
    CronTrigger {
        routine_id: Uuid,
        #[serde(default)]
        project_id: Option<Uuid>,
    },

    /// Enable a recurring heartbeat schedule for an agent.
    #[serde(rename = "agent_heartbeat.enable")]
    AgentHeartbeatEnable {
        agent_id: Uuid,
        interval: String,
        #[serde(default)]
        timezone: Option<String>,
    },

    /// Disable a recurring heartbeat schedule for an agent.
    #[serde(rename = "agent_heartbeat.disable")]
    AgentHeartbeatDisable { agent_id: Uuid },

    /// Trigger a one-time heartbeat run for an agent.
    #[serde(rename = "agent_heartbeat.trigger")]
    AgentHeartbeatTrigger { agent_id: Uuid },

    // -----------------------------------------------------------------
    // Bootstrap
    // -----------------------------------------------------------------
    // -----------------------------------------------------------------
    // Health check
    // -----------------------------------------------------------------
    /// Lightweight ping from the frontend to verify the worker is alive.
    /// The worker should respond with `Response::WorkerPong`.
    #[serde(rename = "worker.ping")]
    WorkerPing,

    /// Push a user-scoped wrapped account content key to a specific worker so
    /// it can decrypt/encrypt that user's private chat traffic.
    #[serde(rename = "worker.account_key_updated")]
    WorkerAccountKeyUpdated {
        wrapped_ack: WrappedAccountContentKey,
    },

    // -----------------------------------------------------------------
    // Bootstrap
    // -----------------------------------------------------------------
    /// Notifies the harness that a backend resource was created, updated,
    /// or deleted. The harness should re-fetch the affected resource.
    #[serde(rename = "manifest.changed")]
    ManifestChanged {
        resource_type: ResourceType,
        resource_id: Uuid,
        action: ResourceAction,
        /// Parent project ID — set for project-scoped resources (documents, etc.)
        /// so the harness can scope operations to the correct project.
        #[serde(default)]
        project_id: Option<Uuid>,
        /// Inline resource payload — avoids a round-trip fetch to the backend API.
        /// `None` means the harness should fetch from the detail endpoint (fallback).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        /// Inline encrypted resource payload — preferred over plaintext `payload`
        /// when the worker has an active ACK.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
        /// Multiple inline encrypted resource payloads for resources with
        /// several independently encrypted sub-documents.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        encrypted_payloads: Vec<EncryptedPayload>,
    },
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChatMessage { session_id, .. } => write!(f, "chat.message(session={session_id})"),
            Self::ChatDomainEnter { session_id, .. } => {
                write!(f, "chat.domain_enter(session={session_id})")
            }
            Self::ChatDomainExit {
                domain_session_id, ..
            } => write!(f, "chat.domain_exit(session={domain_session_id})"),
            Self::ChatCancel { project_id, .. } => write!(f, "chat.cancel(project={project_id})"),
            Self::ChatSessionDelete { session_id, .. } => {
                write!(f, "chat.session_delete(session={session_id})")
            }
            Self::TaskExecute {
                execution_run_id, ..
            } => {
                write!(f, "task.execute(run={execution_run_id})")
            }
            Self::ExecutionCancel { execution_run_id } => {
                write!(f, "execution.cancel(run={execution_run_id})")
            }
            Self::ExecutionPause { execution_run_id } => {
                write!(f, "execution.pause(run={execution_run_id})")
            }
            Self::ExecutionResume { execution_run_id } => {
                write!(f, "execution.resume(run={execution_run_id})")
            }
            Self::RepoSync { project_id, .. } => write!(f, "repo.sync(project={project_id})"),
            Self::RepoUnsync { project_id } => write!(f, "repo.unsync(project={project_id})"),
            Self::CronEnable { routine_id, .. } => {
                write!(f, "cron.enable(routine={routine_id})")
            }
            Self::CronDisable { routine_id } => {
                write!(f, "cron.disable(routine={routine_id})")
            }
            Self::CronTrigger { routine_id, .. } => write!(f, "cron.trigger(routine={routine_id})"),
            Self::AgentHeartbeatEnable { agent_id, .. } => {
                write!(f, "agent_heartbeat.enable(agent={agent_id})")
            }
            Self::AgentHeartbeatDisable { agent_id } => {
                write!(f, "agent_heartbeat.disable(agent={agent_id})")
            }
            Self::AgentHeartbeatTrigger { agent_id } => {
                write!(f, "agent_heartbeat.trigger(agent={agent_id})")
            }
            Self::WorkerPing => write!(f, "worker.ping"),
            Self::WorkerAccountKeyUpdated { .. } => write!(f, "worker.account_key_updated"),
            Self::ManifestChanged {
                resource_type,
                action,
                ..
            } => write!(f, "manifest.changed({resource_type}, {action:?})"),
        }
    }
}

impl Command {
    /// The capability category this command belongs to.
    ///
    /// Used by the backend to route commands to the correct NATS subject
    /// and by workers to validate they should handle a given command.
    pub fn capability(&self) -> Capability {
        match self {
            Command::ChatMessage { .. }
            | Command::ChatDomainEnter { .. }
            | Command::ChatDomainExit { .. }
            | Command::ChatCancel { .. }
            | Command::ChatSessionDelete { .. } => Capability::Chat,

            Command::TaskExecute { .. }
            | Command::ExecutionCancel { .. }
            | Command::ExecutionPause { .. }
            | Command::ExecutionResume { .. } => Capability::Task,

            Command::CronEnable { .. }
            | Command::CronDisable { .. }
            | Command::CronTrigger { .. }
            | Command::AgentHeartbeatEnable { .. }
            | Command::AgentHeartbeatDisable { .. }
            | Command::AgentHeartbeatTrigger { .. } => Capability::Cron,

            Command::WorkerPing => Capability::Ping,
            Command::WorkerAccountKeyUpdated { .. } => Capability::Manifest,

            Command::ManifestChanged { .. } => Capability::Manifest,

            Command::RepoSync { .. } | Command::RepoUnsync { .. } => Capability::Repo,
        }
    }

    /// How this command should be delivered over worker transport.
    pub fn delivery(&self) -> CommandDelivery {
        match self {
            Command::ManifestChanged { .. }
            | Command::RepoSync { .. }
            | Command::RepoUnsync { .. } => CommandDelivery::Broadcast,
            Command::WorkerAccountKeyUpdated { .. } => CommandDelivery::Targeted,
            _ => CommandDelivery::Queue,
        }
    }
}

/// Type of platform resource that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    Agent,
    Model,
    Routine,
    Project,
    Council,
    Ability,
    ContextBlock,
    McpServer,
    Domain,
    Document,
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent => write!(f, "agent"),
            Self::Model => write!(f, "model"),
            Self::Routine => write!(f, "routine"),
            Self::Project => write!(f, "project"),
            Self::Council => write!(f, "council"),
            Self::Ability => write!(f, "ability"),
            Self::ContextBlock => write!(f, "context_block"),
            Self::McpServer => write!(f, "mcp_server"),
            Self::Domain => write!(f, "domain"),
            Self::Document => write!(f, "document"),
        }
    }
}

/// Action performed on a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAction {
    Created,
    Updated,
    Deleted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_delivery_uses_queue_broadcast_and_targeted_lanes() {
        let id = Uuid::nil();

        assert_eq!(
            Command::ChatCancel {
                project_id: id,
                agent_id: None,
            }
            .delivery(),
            CommandDelivery::Queue
        );
        assert_eq!(
            Command::TaskExecute {
                task_id: id,
                project_id: id,
                execution_run_id: id,
                routine_id: None,
                assigned_agent_id: None,
                payload: None,
                encrypted_payload: None,
            }
            .delivery(),
            CommandDelivery::Queue
        );
        assert_eq!(
            Command::ManifestChanged {
                resource_type: ResourceType::Agent,
                resource_id: id,
                action: ResourceAction::Updated,
                project_id: None,
                payload: None,
                encrypted_payload: None,
                encrypted_payloads: Vec::new(),
            }
            .delivery(),
            CommandDelivery::Broadcast
        );
        assert_eq!(
            Command::RepoSync {
                project_id: id,
                repo_url: "https://example.test/repo.git".into(),
                target_branch: "main".into(),
            }
            .delivery(),
            CommandDelivery::Broadcast
        );
        assert_eq!(
            Command::WorkerAccountKeyUpdated {
                wrapped_ack: WrappedAccountContentKey {
                    key_version: 1,
                    algorithm: "x25519-aes-gcm".into(),
                    ephemeral_public_key: "epk".into(),
                    nonce: "nonce".into(),
                    ciphertext: "ciphertext".into(),
                    created_at: chrono::Utc::now(),
                },
            }
            .delivery(),
            CommandDelivery::Targeted
        );
    }
}
