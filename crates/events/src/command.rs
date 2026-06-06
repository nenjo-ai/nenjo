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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainActivation {
    pub domain_session_id: Uuid,
    pub domain_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageGraphUpdate {
    pub schema: String,
    pub nenpm_yml: String,
    pub nenpm_lock_yml: String,
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
        project: Option<String>,
        /// If set, routes to a specific routine instead of a chat agent.
        #[serde(default)]
        routine: Option<String>,
        /// If set, routes to a specific agent; otherwise uses the default.
        #[serde(default)]
        agent: Option<String>,
        /// Typed chat target. New clients should send this with `target`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_type: Option<String>,
        /// Target slug matching `target_type`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        /// Active domain session context, if any.
        #[serde(default)]
        domain_session_id: Option<Uuid>,
        /// Domain activation to apply before processing this turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        domain_activation: Option<DomainActivation>,
        /// Chat session scope.
        session_id: Uuid,
    },

    /// A user-invoked slash command to be expanded by the worker before chat execution.
    #[serde(rename = "chat.command")]
    ChatCommand {
        /// Client-generated message ID for delivery tracking.
        #[serde(default)]
        id: Option<String>,
        /// The installed slash command, including its leading slash.
        command: String,
        /// The user's original message text.
        content: String,
        /// Optional encrypted content body. When present, workers should prefer this over `content`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<EncryptedPayload>,
        /// Target project for context scoping.
        #[serde(default)]
        project: Option<String>,
        /// If set, routes to a specific agent; otherwise uses the default.
        #[serde(default)]
        agent: Option<String>,
        /// Typed chat target. New clients should send this with `target`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_type: Option<String>,
        /// Target slug matching `target_type`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        /// Active domain session context, if any.
        #[serde(default)]
        domain_session_id: Option<Uuid>,
        /// Domain activation to apply before processing this turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        domain_activation: Option<DomainActivation>,
        /// Chat session scope.
        session_id: Uuid,
    },

    /// Exit an active domain session.
    #[serde(rename = "chat.domain_exit")]
    ChatDomainExit {
        project: String,
        agent: String,
        domain_session_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_session_id: Option<Uuid>,
    },

    /// Cancel an in-flight chat response.
    #[serde(rename = "chat.cancel")]
    ChatCancel {
        project: String,
        #[serde(default)]
        agent: Option<String>,
    },

    /// Delete a chat session's local history.
    #[serde(rename = "chat.session_delete")]
    ChatSessionDelete {
        project: String,
        agent: String,
        session_id: Uuid,
    },

    // -----------------------------------------------------------------
    // Task execution
    // -----------------------------------------------------------------
    /// Execute a task from the execution queue.
    #[serde(rename = "task.execute")]
    TaskExecute {
        task_id: Uuid,
        project: String,
        execution_run_id: Uuid,
        #[serde(default)]
        routine: Option<String>,
        #[serde(default)]
        agent: Option<String>,
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
        project: String,
        repo_url: String,
        /// Branch to sync. The clone/pull targets this branch.
        target_branch: String,
    },

    /// Remove a synced project repository.
    #[serde(rename = "repo.unsync")]
    RepoUnsync { project: String },

    // -----------------------------------------------------------------
    // Cron scheduling
    // -----------------------------------------------------------------
    /// Enable a cron schedule for a routine.
    #[serde(rename = "cron.enable")]
    CronEnable {
        routine: String,
        #[serde(default)]
        project: Option<String>,
        schedule: String,
        #[serde(default)]
        timezone: Option<String>,
    },

    /// Disable a cron schedule.
    #[serde(rename = "cron.disable")]
    CronDisable { routine: String },

    /// Trigger a routine immediately (manual or test run).
    #[serde(rename = "cron.trigger")]
    CronTrigger {
        routine: String,
        #[serde(default)]
        project: Option<String>,
    },

    /// Enable a recurring heartbeat schedule for an agent.
    #[serde(rename = "agent_heartbeat.enable")]
    AgentHeartbeatEnable {
        agent: String,
        interval: String,
        #[serde(default)]
        timezone: Option<String>,
    },

    /// Disable a recurring heartbeat schedule for an agent.
    #[serde(rename = "agent_heartbeat.disable")]
    AgentHeartbeatDisable { agent: String },

    /// Trigger a one-time heartbeat run for an agent.
    #[serde(rename = "agent_heartbeat.trigger")]
    AgentHeartbeatTrigger { agent: String },

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
        resource: String,
        action: ResourceAction,
        /// Parent project slug for project-scoped resources.
        #[serde(default)]
        project: Option<String>,
        /// Inline resource payload — avoids a round-trip fetch to the backend API.
        /// `None` means the harness should fetch from the detail endpoint (fallback).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        /// Inline encrypted resource payload — preferred over plaintext `payload`
        /// when the worker has an active ACK.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// Notifies workers that platform-managed packages changed. Workers should
    /// materialize the supplied locked package graph and rebuild runtime package
    /// manifests from the installed package tree.
    #[serde(rename = "package.graph_changed")]
    PackageGraphChanged { packages: PackageGraphUpdate },
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChatMessage { session_id, .. } => write!(f, "chat.message(session={session_id})"),
            Self::ChatCommand {
                session_id,
                command,
                ..
            } => write!(f, "chat.command(command={command}, session={session_id})"),
            Self::ChatDomainExit {
                domain_session_id, ..
            } => write!(f, "chat.domain_exit(session={domain_session_id})"),
            Self::ChatCancel { project, .. } => write!(f, "chat.cancel(project={project})"),
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
            Self::RepoSync { project, .. } => write!(f, "repo.sync(project={project})"),
            Self::RepoUnsync { project } => write!(f, "repo.unsync(project={project})"),
            Self::CronEnable { routine, .. } => {
                write!(f, "cron.enable(routine={routine})")
            }
            Self::CronDisable { routine } => {
                write!(f, "cron.disable(routine={routine})")
            }
            Self::CronTrigger { routine, .. } => write!(f, "cron.trigger(routine={routine})"),
            Self::AgentHeartbeatEnable { agent, .. } => {
                write!(f, "agent_heartbeat.enable(agent={agent})")
            }
            Self::AgentHeartbeatDisable { agent } => {
                write!(f, "agent_heartbeat.disable(agent={agent})")
            }
            Self::AgentHeartbeatTrigger { agent } => {
                write!(f, "agent_heartbeat.trigger(agent={agent})")
            }
            Self::WorkerPing => write!(f, "worker.ping"),
            Self::WorkerAccountKeyUpdated { .. } => write!(f, "worker.account_key_updated"),
            Self::ManifestChanged {
                resource_type,
                action,
                ..
            } => write!(f, "manifest.changed({resource_type}, {action:?})"),
            Self::PackageGraphChanged { .. } => write!(f, "package.graph_changed"),
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
            | Command::ChatCommand { .. }
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
            Command::PackageGraphChanged { .. } => Capability::Manifest,

            Command::RepoSync { .. } | Command::RepoUnsync { .. } => Capability::Repo,
        }
    }

    /// How this command should be delivered over worker transport.
    pub fn delivery(&self) -> CommandDelivery {
        match self {
            Command::ManifestChanged { .. }
            | Command::PackageGraphChanged { .. }
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
    KnowledgePack,
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
            Self::KnowledgePack => write!(f, "knowledge_pack"),
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
                project: "demo_project".into(),
                agent: None,
            }
            .delivery(),
            CommandDelivery::Queue
        );
        assert_eq!(
            Command::TaskExecute {
                task_id: id,
                project: "demo_project".into(),
                execution_run_id: id,
                routine: None,
                agent: None,
                payload: None,
                encrypted_payload: None,
            }
            .delivery(),
            CommandDelivery::Queue
        );
        assert_eq!(
            Command::ManifestChanged {
                resource_type: ResourceType::Agent,
                resource: "demo_agent".into(),
                action: ResourceAction::Updated,
                project: None,
                payload: None,
                encrypted_payload: None,
            }
            .delivery(),
            CommandDelivery::Broadcast
        );
        assert_eq!(
            Command::RepoSync {
                project: "demo_project".into(),
                repo_url: "https://example.test/repo.git".into(),
                target_branch: "main".into(),
            }
            .delivery(),
            CommandDelivery::Broadcast
        );
        assert_eq!(
            Command::PackageGraphChanged {
                packages: PackageGraphUpdate {
                    schema: "nenjo.platform_packages.v1".into(),
                    nenpm_yml: "schema: nenjo.dependencies.v1\n".into(),
                    nenpm_lock_yml: "schema: nenjo.lock.v1\n".into(),
                },
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
