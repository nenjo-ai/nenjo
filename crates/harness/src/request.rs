//! Builder-style request types for harness execution APIs.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use nenjo::hooks::ActiveHookScope;
use nenjo::{IntoSlug, Slug};
use uuid::Uuid;

use nenjo::{ProjectLocation, TaskInput};

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
    pub agent: Slug,
    pub message: String,
    pub project: Option<Slug>,
    pub domain_session_id: Option<Uuid>,
    pub domain_activation: Option<ChatDomainActivation>,
    pub template_override: Option<String>,
    pub hook_scopes: Vec<ActiveHookScope>,
    pub hook_transcript_dir: Option<PathBuf>,
}

impl ChatRequest {
    /// Create a chat request with a new session ID.
    pub fn new(agent: impl IntoSlug, message: impl Into<String>) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            agent: agent.into_slug(),
            message: message.into(),
            project: None,
            domain_session_id: None,
            domain_activation: None,
            template_override: None,
            hook_scopes: Vec::new(),
            hook_transcript_dir: None,
        }
    }

    /// Set the session ID used for conversation continuity and host correlation.
    pub fn with_session(mut self, session_id: Uuid) -> Self {
        self.session_id = session_id;
        self
    }

    /// Attach project context to the chat turn.
    pub fn with_project(mut self, project: impl IntoSlug) -> Self {
        self.project = Some(project.into_slug());
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

    /// Attach hooks that are active only for this chat turn.
    pub fn with_hook_scope(mut self, scope: ActiveHookScope) -> Self {
        self.hook_scopes.push(scope);
        self
    }

    /// Replace the agent chat template for this turn.
    pub fn with_template_override(mut self, template: impl Into<String>) -> Self {
        self.template_override = Some(template.into());
        self
    }

    /// Store Claude-compatible hook transcript files outside the working tree.
    pub fn with_hook_transcript_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.hook_transcript_dir = Some(dir.into());
        self
    }
}

/// Session-aware task request for the harness API.
#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub task_id: Uuid,
    pub project: Slug,
    pub title: String,
    pub description: String,
    pub routine: Option<Slug>,
    pub agent: Option<Slug>,
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
    pub routine: Slug,
    pub project: Option<Slug>,
    pub schedule: String,
    pub timezone: Option<String>,
    pub start_at: Option<DateTime<Utc>>,
    pub timeout: Duration,
    pub execution_run_id: Option<Uuid>,
    pub project_location: Option<ProjectLocation>,
}

impl CronRequest {
    /// Create a cron routine request with the required routine identity and schedule.
    pub fn new(routine: impl IntoSlug, schedule: impl Into<String>) -> Self {
        Self {
            routine: routine.into_slug(),
            project: None,
            schedule: schedule.into(),
            timezone: None,
            start_at: None,
            timeout: Duration::ZERO,
            execution_run_id: None,
            project_location: None,
        }
    }

    /// Attach project context to the cron routine run.
    pub fn with_project(mut self, project: impl IntoSlug) -> Self {
        self.project = Some(project.into_slug());
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
    pub agent: Slug,
    pub interval: Duration,
    pub start_at: Option<DateTime<Utc>>,
    pub instructions: Option<String>,
    pub previous_output: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub execution_run_id: Option<Uuid>,
}

impl HeartbeatRequest {
    /// Create a heartbeat request with the required agent and interval.
    pub fn new(agent: impl IntoSlug, interval: Duration) -> Self {
        Self {
            agent: agent.into_slug(),
            interval,
            start_at: None,
            instructions: None,
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

    /// Attach user-configured heartbeat instructions.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
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
    /// Create a task request with a new task ID.
    pub fn new(
        project: impl IntoSlug,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            task_id: Uuid::new_v4(),
            project: project.into_slug(),
            title: title.into(),
            description: description.into(),
            routine: None,
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

    /// Set the task ID used for task continuity and host correlation.
    pub fn with_task_id(mut self, task_id: Uuid) -> Self {
        self.task_id = task_id;
        self
    }

    /// Create a task request from a core SDK task input.
    pub fn from_task_input(task: &TaskInput, project: Slug) -> Self {
        Self {
            task_id: task.task_id,
            project,
            title: task.title.clone(),
            description: task.description.clone(),
            routine: None,
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
    pub fn with_routine(mut self, routine: impl IntoSlug) -> Self {
        self.routine = Some(routine.into_slug());
        self
    }

    /// Execute the task directly with an agent.
    pub fn with_agent(mut self, agent: impl IntoSlug) -> Self {
        self.agent = Some(agent.into_slug());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_generates_session_and_allows_override() {
        let generated = ChatRequest::new("code_reviewer", "review this");
        assert!(!generated.session_id.is_nil());
        assert_eq!(generated.agent.as_str(), "code_reviewer");

        let session_id = Uuid::new_v4();
        let explicit = ChatRequest::new("Code Reviewer", "review this").with_session(session_id);
        assert_eq!(explicit.session_id, session_id);
        assert_eq!(explicit.agent.as_str(), "code_reviewer");
    }

    #[test]
    fn task_request_generates_task_id_and_allows_override() {
        let generated = TaskRequest::new("demo_project", "Title", "Description");
        assert!(!generated.task_id.is_nil());
        assert_eq!(generated.project.as_str(), "demo_project");

        let task_id = Uuid::new_v4();
        let explicit =
            TaskRequest::new("Demo Project", "Title", "Description").with_task_id(task_id);
        assert_eq!(explicit.task_id, task_id);
        assert_eq!(explicit.project.as_str(), "demo_project");
    }
}
