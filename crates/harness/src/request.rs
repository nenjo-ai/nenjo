//! Builder-style request types for harness execution APIs.

use nenjo::hooks::ActiveHookScope;
use nenjo::{IntoSlug, Slug};
use std::path::PathBuf;
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
    /// Durable identity of the user message that initiated this turn.
    pub input_message_id: Option<Uuid>,
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
            input_message_id: None,
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

    /// Associate this execution with its persisted user-message identity.
    pub fn with_input_message_id(mut self, input_message_id: Uuid) -> Self {
        self.input_message_id = Some(input_message_id);
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
    pub project: Option<Slug>,
    pub title: String,
    pub instructions: String,
    pub routine: Option<Slug>,
    pub agent: Option<Slug>,
    pub execution_run_id: Option<Uuid>,
    pub slug: Option<String>,
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub project_location: Option<ProjectLocation>,
}

impl TaskRequest {
    /// Create a task request with a new task ID.
    pub fn new(title: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            task_id: Uuid::new_v4(),
            project: None,
            title: title.into(),
            instructions: instructions.into(),
            routine: None,
            agent: None,
            execution_run_id: None,
            slug: None,
            labels: Vec::new(),
            status: None,
            priority: None,
            project_location: None,
        }
    }

    /// Set the task ID used for task continuity and host correlation.
    pub fn with_task_id(mut self, task_id: Uuid) -> Self {
        self.task_id = task_id;
        self
    }

    /// Create a task request from a core SDK task input.
    pub fn from_task_input(task: &TaskInput) -> Self {
        Self {
            task_id: task.task_id,
            project: task.project.clone(),
            title: task.title.clone(),
            instructions: task.instructions.clone(),
            routine: None,
            agent: None,
            execution_run_id: None,
            slug: task.slug.clone(),
            labels: task.labels.clone(),
            status: task.status.clone(),
            priority: task.priority.clone(),
            project_location: None,
        }
    }

    /// Attach project and repository context to this task.
    pub fn with_project(mut self, project: impl IntoSlug) -> Self {
        self.project = Some(project.into_slug());
        self
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

    /// Set first-class task label names.
    pub fn with_labels(mut self, labels: impl IntoIterator<Item = String>) -> Self {
        self.labels = labels.into_iter().collect();
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
        let input_message_id = Uuid::new_v4();
        // Whitespace → kebab-case (`Slug::derive`).
        let explicit = ChatRequest::new("Code Reviewer", "review this")
            .with_session(session_id)
            .with_input_message_id(input_message_id);
        assert_eq!(explicit.session_id, session_id);
        assert_eq!(explicit.input_message_id, Some(input_message_id));
        assert_eq!(explicit.agent.as_str(), "code-reviewer");
    }

    #[test]
    fn task_request_generates_task_id_and_allows_override() {
        let generated = TaskRequest::new("Title", "Description").with_project("demo_project");
        assert!(!generated.task_id.is_nil());
        assert_eq!(
            generated.project.as_ref().map(Slug::as_str),
            Some("demo_project")
        );

        let task_id = Uuid::new_v4();
        // Whitespace → kebab-case (`Slug::derive`).
        let explicit = TaskRequest::new("Title", "Description")
            .with_project("Demo Project")
            .with_task_id(task_id);
        assert_eq!(explicit.task_id, task_id);
        assert_eq!(
            explicit.project.as_ref().map(Slug::as_str),
            Some("demo-project")
        );

        let projectless = TaskRequest::new("Title", "Instructions");
        assert!(projectless.project.is_none());
    }
}
