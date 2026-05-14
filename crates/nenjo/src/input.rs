//! Public execution input types for agents and routines.
//!
//! These types describe what the caller wants to run. Runtime-specific local
//! locations, such as checked-out worktrees, can be supplied by the caller via
//! [`ProjectLocation`].

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use nenjo_models::ChatMessage;
use uuid::Uuid;

use crate::routines::types::{CronSchedule, SessionBinding, StepResult};
use crate::types::GitContext;

pub(crate) fn render_context_from_agent_run(run: &AgentRun) -> crate::context::RenderContextVars {
    let mut ctx = crate::context::RenderContextVars::default();
    match &run.kind {
        AgentRunKind::Task(task) => {
            ctx.task = task_to_context(task);
            ctx.project.id = task.project_id.to_string();
            ctx.git = git_to_context(run.execution.project_location.as_ref());
        }
        AgentRunKind::Chat(chat) => {
            ctx.chat_message = chat.message.clone();
            ctx.project.id = chat
                .project_id
                .map(|project_id| project_id.to_string())
                .unwrap_or_default();
        }
        AgentRunKind::Gate(gate) => {
            ctx.gate_criteria = gate.criteria.clone();
            ctx.gate_previous_output = gate.previous_result.output.clone();
            ctx.project.id = gate.project_id.to_string();
            if let Some(task) = &gate.task {
                ctx.task = task_to_context(task);
            }
            ctx.git = git_to_context(run.execution.project_location.as_ref());
        }
        AgentRunKind::CouncilSubtask(subtask) => {
            ctx.subtask_parent_task = subtask.parent_task.clone();
            ctx.subtask_description = subtask.subtask_description.clone();
            ctx.project.id = subtask.project_id.to_string();
        }
        AgentRunKind::Cron(cron) => {
            ctx.project.id = cron
                .project_id
                .map(|project_id| project_id.to_string())
                .unwrap_or_default();
            if let Some(task) = &cron.task {
                ctx.task = task_to_context(task);
            }
            ctx.git = git_to_context(run.execution.project_location.as_ref());
        }
        AgentRunKind::Heartbeat(heartbeat) => {
            ctx.heartbeat_previous_output = heartbeat.previous_output.clone().unwrap_or_default();
            ctx.heartbeat_last_run_at = heartbeat
                .last_run_at
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_default();
            ctx.heartbeat_next_run_at = heartbeat
                .next_run_at
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_default();
        }
    }
    ctx
}

fn task_to_context(task: &TaskInput) -> crate::context::TaskContext {
    crate::context::TaskContext {
        id: task.task_id.to_string(),
        title: task.title.clone(),
        description: task.description.clone(),
        acceptance_criteria: task.acceptance_criteria.clone().unwrap_or_default(),
        tags: task.tags.join(", "),
        source: task.source.clone().unwrap_or_else(|| "task".to_string()),
        status: task.status.clone().unwrap_or_default(),
        priority: task.priority.clone().unwrap_or_default(),
        task_type: task.task_type.clone().unwrap_or_default(),
        slug: task.slug.clone().unwrap_or_default(),
        complexity: task.complexity.clone().unwrap_or_default(),
    }
}

fn git_to_context(location: Option<&ProjectLocation>) -> crate::context::types::GitContext {
    match location.and_then(|location| location.git.as_ref()) {
        Some(git) => crate::context::types::GitContext {
            repo_url: git.repo_url.clone(),
            branch: git.branch.clone(),
            target_branch: git.target_branch.clone(),
            work_dir: git.work_dir.clone(),
        },
        None => crate::context::types::GitContext::default(),
    }
}

/// Task execution input supplied by SDK callers.
#[derive(Debug, Clone)]
pub struct TaskInput {
    pub project_id: Uuid,
    pub task_id: Uuid,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Option<String>,
    pub tags: Vec<String>,
    pub source: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub task_type: Option<String>,
    pub slug: Option<String>,
    pub complexity: Option<String>,
}

impl TaskInput {
    pub fn new(
        project_id: Uuid,
        task_id: Uuid,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            project_id,
            task_id,
            title: title.into(),
            description: description.into(),
            acceptance_criteria: None,
            tags: Vec::new(),
            source: None,
            status: None,
            priority: None,
            task_type: None,
            slug: None,
            complexity: None,
        }
    }

    pub fn acceptance_criteria(mut self, value: impl Into<String>) -> Self {
        self.acceptance_criteria = Some(value.into());
        self
    }

    pub fn tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    pub fn source(mut self, value: impl Into<String>) -> Self {
        self.source = Some(value.into());
        self
    }

    pub fn status(mut self, value: impl Into<String>) -> Self {
        self.status = Some(value.into());
        self
    }

    pub fn priority(mut self, value: impl Into<String>) -> Self {
        self.priority = Some(value.into());
        self
    }

    pub fn task_type(mut self, value: impl Into<String>) -> Self {
        self.task_type = Some(value.into());
        self
    }

    pub fn slug(mut self, value: impl Into<String>) -> Self {
        self.slug = Some(value.into());
        self
    }

    pub fn complexity(mut self, value: impl Into<String>) -> Self {
        self.complexity = Some(value.into());
        self
    }
}

/// Chat execution input.
#[derive(Debug, Clone)]
pub struct ChatInput {
    pub project_id: Option<Uuid>,
    pub message: String,
    pub history: Vec<ChatMessage>,
}

impl ChatInput {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            project_id: None,
            message: message.into(),
            history: Vec::new(),
        }
    }

    pub fn project_id(mut self, project_id: Uuid) -> Self {
        self.project_id = Some(project_id);
        self
    }

    pub fn history(mut self, history: Vec<ChatMessage>) -> Self {
        self.history = history;
        self
    }
}

/// Gate evaluation input used by routine internals.
#[derive(Debug, Clone)]
pub struct GateInput {
    pub previous_result: StepResult,
    pub criteria: String,
    pub project_id: Uuid,
    pub task: Option<TaskInput>,
}

/// Council subtask input used by routine internals.
#[derive(Debug, Clone)]
pub struct CouncilSubtaskInput {
    pub parent_task: String,
    pub subtask_description: String,
    pub subtask_index: usize,
    pub project_id: Uuid,
}

/// Cron execution input.
#[derive(Debug, Clone)]
pub struct CronInput {
    pub project_id: Option<Uuid>,
    pub task: Option<TaskInput>,
    pub schedule: CronSchedule,
    pub start_at: Option<DateTime<Utc>>,
    pub timeout: Duration,
}

/// Heartbeat execution input.
#[derive(Debug, Clone)]
pub struct HeartbeatInput {
    pub agent_id: Uuid,
    pub interval: Duration,
    pub start_at: Option<DateTime<Utc>>,
    pub previous_output: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
}

/// Runtime options common to agent and routine runs.
#[derive(Debug, Clone, Default)]
pub struct ExecutionOptions {
    pub execution_run_id: Option<Uuid>,
    pub session_binding: Option<SessionBinding>,
    /// Optional runtime location prepared by the host, such as a task worktree.
    pub project_location: Option<ProjectLocation>,
}

impl ExecutionOptions {
    pub fn execution_run_id(mut self, id: Uuid) -> Self {
        self.execution_run_id = Some(id);
        self
    }

    pub fn session_binding(mut self, binding: SessionBinding) -> Self {
        self.session_binding = Some(binding);
        self
    }

    pub fn project_location(mut self, location: ProjectLocation) -> Self {
        self.project_location = Some(location);
        self
    }
}

/// Agent execution input.
#[derive(Debug, Clone)]
pub struct AgentRun {
    pub kind: AgentRunKind,
    pub execution: ExecutionOptions,
}

#[derive(Debug, Clone)]
pub enum AgentRunKind {
    Task(TaskInput),
    Chat(ChatInput),
    Gate(GateInput),
    CouncilSubtask(CouncilSubtaskInput),
    Cron(CronInput),
    Heartbeat(HeartbeatInput),
}

impl AgentRun {
    pub fn task(task: TaskInput) -> Self {
        Self {
            kind: AgentRunKind::Task(task),
            execution: ExecutionOptions::default(),
        }
    }

    pub fn chat(chat: ChatInput) -> Self {
        Self {
            kind: AgentRunKind::Chat(chat),
            execution: ExecutionOptions::default(),
        }
    }

    pub fn execution_run(mut self, id: Uuid) -> Self {
        self.execution.execution_run_id = Some(id);
        self
    }

    pub fn session_binding(mut self, binding: SessionBinding) -> Self {
        self.execution.session_binding = Some(binding);
        self
    }

    pub fn project_location(mut self, location: ProjectLocation) -> Self {
        self.execution.project_location = Some(location);
        self
    }
}

/// Routine execution input.
#[derive(Debug, Clone)]
pub struct RoutineRun {
    pub kind: RoutineRunKind,
    pub execution: ExecutionOptions,
}

#[derive(Debug, Clone)]
pub enum RoutineRunKind {
    Task(TaskInput),
    Cron(CronInput),
}

impl RoutineRun {
    pub fn task(task: TaskInput) -> Self {
        Self {
            kind: RoutineRunKind::Task(task),
            execution: ExecutionOptions::default(),
        }
    }

    pub fn cron(cron: CronInput) -> Self {
        Self {
            kind: RoutineRunKind::Cron(cron),
            execution: ExecutionOptions::default(),
        }
    }

    pub fn execution_run(mut self, id: Uuid) -> Self {
        self.execution.execution_run_id = Some(id);
        self
    }

    pub fn session_binding(mut self, binding: SessionBinding) -> Self {
        self.execution.session_binding = Some(binding);
        self
    }

    pub fn project_location(mut self, location: ProjectLocation) -> Self {
        self.execution.project_location = Some(location);
        self
    }
}

impl From<TaskInput> for RoutineRun {
    fn from(task: TaskInput) -> Self {
        Self::task(task)
    }
}

/// Local runtime location for a project execution.
#[derive(Debug, Clone, Default)]
pub struct ProjectLocation {
    pub working_dir: Option<PathBuf>,
    pub git: Option<GitContext>,
}

impl ProjectLocation {
    pub fn from_git(git: GitContext) -> Self {
        let working_dir = if git.work_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(&git.work_dir))
        };
        Self {
            working_dir,
            git: Some(git),
        }
    }
}
