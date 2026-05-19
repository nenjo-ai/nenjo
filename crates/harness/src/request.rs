//! Builder-style request types for harness execution APIs.

use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use nenjo::{ProjectLocation, TaskInput};

/// Agent selector accepted by harness execution APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRef {
    /// Select an agent by manifest ID.
    Id(Uuid),
    /// Select an agent by manifest name.
    Name(String),
}

impl From<Uuid> for AgentRef {
    fn from(value: Uuid) -> Self {
        Self::Id(value)
    }
}

impl From<String> for AgentRef {
    fn from(value: String) -> Self {
        Self::Name(value)
    }
}

impl From<&str> for AgentRef {
    fn from(value: &str) -> Self {
        Self::Name(value.to_string())
    }
}

/// Domain activation requested for a chat turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDomainActivation {
    pub domain_session_id: Uuid,
    pub domain_command: String,
}

impl ChatDomainActivation {
    /// Create a domain activation request.
    pub fn new(domain_session_id: Uuid, domain_command: impl Into<String>) -> Self {
        Self {
            domain_session_id,
            domain_command: domain_command.into(),
        }
    }
}

/// Session-aware chat request for the harness API.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub session_id: Uuid,
    pub agent: AgentRef,
    pub message: String,
    pub project_id: Option<Uuid>,
    pub domain_session_id: Option<Uuid>,
    pub domain_activation: Option<ChatDomainActivation>,
}

impl ChatRequest {
    /// Create a chat request with the required session, agent, and message.
    pub fn new(session_id: Uuid, agent: impl Into<AgentRef>, message: impl Into<String>) -> Self {
        Self {
            session_id,
            agent: agent.into(),
            message: message.into(),
            project_id: None,
            domain_session_id: None,
            domain_activation: None,
        }
    }

    /// Attach project context to the chat turn.
    pub fn with_project(mut self, project_id: Uuid) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Attach an existing domain session to the chat turn.
    pub fn with_domain_session(mut self, domain_session_id: Uuid) -> Self {
        self.domain_session_id = Some(domain_session_id);
        self
    }

    /// Activate a domain as part of this chat turn.
    pub fn with_domain_activation(
        mut self,
        domain_session_id: Uuid,
        domain_command: impl Into<String>,
    ) -> Self {
        self.domain_activation = Some(ChatDomainActivation::new(domain_session_id, domain_command));
        self
    }
}

/// Session-aware task request for the harness API.
#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub description: String,
    pub routine_id: Option<Uuid>,
    pub agent: Option<AgentRef>,
    pub execution_run_id: Option<Uuid>,
    pub slug: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub task_type: Option<String>,
    pub complexity: Option<String>,
    pub project_location: Option<ProjectLocation>,
}

/// Scheduled cron routine request for the harness API.
#[derive(Debug, Clone)]
pub struct CronRequest {
    pub routine_id: Uuid,
    pub project_id: Option<Uuid>,
    pub schedule: String,
    pub timezone: Option<String>,
    pub start_at: Option<DateTime<Utc>>,
    pub timeout: Duration,
    pub execution_run_id: Option<Uuid>,
    pub project_location: Option<ProjectLocation>,
}

impl CronRequest {
    /// Create a cron routine request with the required routine identity and schedule.
    pub fn new(routine_id: Uuid, schedule: impl Into<String>) -> Self {
        Self {
            routine_id,
            project_id: None,
            schedule: schedule.into(),
            timezone: None,
            start_at: None,
            timeout: Duration::ZERO,
            execution_run_id: None,
            project_location: None,
        }
    }

    /// Attach project context to the cron routine run.
    pub fn with_project(mut self, project_id: Uuid) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Set the schedule timezone used when parsing cron expressions.
    pub fn with_timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = Some(timezone.into());
        self
    }

    /// Set the start timestamp passed to the cron routine.
    pub fn with_start_at(mut self, start_at: DateTime<Utc>) -> Self {
        self.start_at = Some(start_at);
        self
    }

    /// Set the cron run timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the execution run ID used for correlation.
    pub fn with_execution_run(mut self, execution_run_id: Uuid) -> Self {
        self.execution_run_id = Some(execution_run_id);
        self
    }

    /// Set the local project location used for this cron execution.
    pub fn with_project_location(mut self, location: ProjectLocation) -> Self {
        self.project_location = Some(location);
        self
    }
}

/// Scheduled agent heartbeat request for the harness API.
#[derive(Debug, Clone)]
pub struct HeartbeatRequest {
    pub agent_id: Uuid,
    pub interval: Duration,
    pub start_at: Option<DateTime<Utc>>,
    pub previous_output: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub execution_run_id: Option<Uuid>,
}

impl HeartbeatRequest {
    /// Create a heartbeat request with the required agent and interval.
    pub fn new(agent_id: Uuid, interval: Duration) -> Self {
        Self {
            agent_id,
            interval,
            start_at: None,
            previous_output: None,
            last_run_at: None,
            next_run_at: None,
            execution_run_id: None,
        }
    }

    /// Set the heartbeat start timestamp.
    pub fn with_start_at(mut self, start_at: DateTime<Utc>) -> Self {
        self.start_at = Some(start_at);
        self
    }

    /// Attach the previous heartbeat output.
    pub fn with_previous_output(mut self, previous_output: impl Into<String>) -> Self {
        self.previous_output = Some(previous_output.into());
        self
    }

    /// Attach the previous heartbeat completion timestamp.
    pub fn with_last_run_at(mut self, last_run_at: DateTime<Utc>) -> Self {
        self.last_run_at = Some(last_run_at);
        self
    }

    /// Attach the next scheduled heartbeat timestamp.
    pub fn with_next_run_at(mut self, next_run_at: DateTime<Utc>) -> Self {
        self.next_run_at = Some(next_run_at);
        self
    }

    /// Set the execution run ID used for correlation.
    pub fn with_execution_run(mut self, execution_run_id: Uuid) -> Self {
        self.execution_run_id = Some(execution_run_id);
        self
    }
}

impl TaskRequest {
    /// Create a task request with the required task identity and content.
    pub fn new(
        task_id: Uuid,
        project_id: Uuid,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            task_id,
            project_id,
            title: title.into(),
            description: description.into(),
            routine_id: None,
            agent: None,
            execution_run_id: None,
            slug: None,
            acceptance_criteria: None,
            tags: Vec::new(),
            status: None,
            priority: None,
            task_type: None,
            complexity: None,
            project_location: None,
        }
    }

    /// Create a task request from a core SDK task input.
    pub fn from_task_input(task: &TaskInput) -> Self {
        Self {
            task_id: task.task_id,
            project_id: task.project_id,
            title: task.title.clone(),
            description: task.description.clone(),
            routine_id: None,
            agent: None,
            execution_run_id: None,
            slug: task.slug.clone(),
            acceptance_criteria: task.acceptance_criteria.clone(),
            tags: task.tags.clone(),
            status: task.status.clone(),
            priority: task.priority.clone(),
            task_type: task.task_type.clone(),
            complexity: task.complexity.clone(),
            project_location: None,
        }
    }

    /// Execute the task through a routine.
    pub fn with_routine(mut self, routine_id: Uuid) -> Self {
        self.routine_id = Some(routine_id);
        self
    }

    /// Execute the task directly with an agent.
    pub fn with_agent(mut self, agent: impl Into<AgentRef>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Set the execution run ID used for cancellation, tracing, and host correlation.
    pub fn with_execution_run(mut self, execution_run_id: Uuid) -> Self {
        self.execution_run_id = Some(execution_run_id);
        self
    }

    /// Set the task slug.
    pub fn with_slug(mut self, slug: impl Into<String>) -> Self {
        self.slug = Some(slug.into());
        self
    }

    /// Set acceptance criteria.
    pub fn with_acceptance_criteria(mut self, acceptance_criteria: impl Into<String>) -> Self {
        self.acceptance_criteria = Some(acceptance_criteria.into());
        self
    }

    /// Set task tags.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.tags = tags.into_iter().collect();
        self
    }

    /// Set task status metadata.
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    /// Set task priority metadata.
    pub fn with_priority(mut self, priority: impl Into<String>) -> Self {
        self.priority = Some(priority.into());
        self
    }

    /// Set task type metadata.
    pub fn with_task_type(mut self, task_type: impl Into<String>) -> Self {
        self.task_type = Some(task_type.into());
        self
    }

    /// Set task complexity metadata.
    pub fn with_complexity(mut self, complexity: impl Into<String>) -> Self {
        self.complexity = Some(complexity.into());
        self
    }

    /// Set the local project location used for this task execution.
    pub fn with_project_location(mut self, location: ProjectLocation) -> Self {
        self.project_location = Some(location);
        self
    }
}
