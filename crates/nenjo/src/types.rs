use std::collections::HashSet;
use std::time::Duration;

use crate::manifest::DomainManifest;
pub use crate::manifest::{
    AbilityPromptConfig, DomainManifest as DomainSessionManifest, DomainPromptConfig,
};
use crate::routines::types::StepResult;
use nenjo_models::ChatMessage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Re-export RenderContext from the agents context module.
pub use crate::context::RenderContextVars;

/// Worker-specific extensions for RenderContext.
pub trait RenderContextExt {
    fn from_task(task: &TaskType) -> Self;
}

/// Git context for a task execution — set by the harness when the project
/// has a synced repository. Provides the agent with branch and worktree info.
#[derive(Debug, Clone, Default)]
pub struct GitContext {
    /// Branch name for this task (e.g. `agent/run-id/fix-auth`).
    pub branch: String,
    /// Target branch for PRs/merges (e.g. `main`).
    pub target_branch: String,
    /// Absolute path to the worktree directory.
    pub work_dir: String,
    /// Remote clone URL for the repository.
    pub repo_url: String,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub task_id: Uuid,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Option<String>,
    pub tags: Vec<String>,
    pub source: String,
    pub project_id: Uuid,
    pub status: String,
    pub priority: String,
    pub task_type: String,
    pub slug: String,
    pub complexity: String,
    /// Git context — set when the project has a synced repo and a worktree
    /// was created for this task.
    pub git: Option<GitContext>,
}

impl RenderContextExt for RenderContextVars {
    fn from_task(task: &TaskType) -> Self {
        let mut ctx = Self::default();
        match task {
            TaskType::Task(t) => {
                ctx.git = git_to_context(t.git.as_ref());
                ctx.task = task_to_context(t);
                ctx.project.id = t.project_id.to_string();
            }
            TaskType::Chat {
                user_message,
                project_id,
                ..
            } => {
                ctx.chat_message = user_message.clone();
                ctx.project.id = project_id.to_string();
            }
            TaskType::Gate {
                criteria,
                previous_result,
                project_id,
                task,
            } => {
                ctx.gate_criteria = criteria.clone();
                ctx.gate_previous_output = previous_result.output.clone();
                ctx.project.id = project_id.to_string();
                if let Some(t) = task {
                    ctx.git = git_to_context(t.git.as_ref());
                    ctx.task = task_to_context(t);
                }
            }
            TaskType::CouncilSubtask {
                parent_task,
                subtask_description,
                project_id,
                ..
            } => {
                ctx.subtask_parent_task = parent_task.clone();
                ctx.subtask_description = subtask_description.clone();
                ctx.project.id = project_id.to_string();
            }
            TaskType::Cron {
                task, project_id, ..
            } => {
                ctx.project.id = project_id.to_string();
                if let Some(t) = task {
                    ctx.git = git_to_context(t.git.as_ref());
                    ctx.task = task_to_context(t);
                }
            }
            TaskType::Heartbeat {
                project_id,
                previous_output,
                last_run_at,
                next_run_at,
                ..
            } => {
                ctx.project.id = project_id.map(|id| id.to_string()).unwrap_or_default();
                ctx.heartbeat_previous_output = previous_output.clone().unwrap_or_default();
                ctx.heartbeat_last_run_at =
                    last_run_at.map(|ts| ts.to_rfc3339()).unwrap_or_default();
                ctx.heartbeat_next_run_at =
                    next_run_at.map(|ts| ts.to_rfc3339()).unwrap_or_default();
            }
        }
        ctx
    }
}

fn task_to_context(t: &Task) -> crate::context::TaskContext {
    crate::context::TaskContext {
        id: t.task_id.to_string(),
        title: t.title.clone(),
        description: t.description.clone(),
        acceptance_criteria: t.acceptance_criteria.as_deref().unwrap_or("").to_string(),
        tags: t.tags.join(", "),
        source: t.source.clone(),
        status: t.status.clone(),
        priority: t.priority.clone(),
        task_type: t.task_type.clone(),
        slug: t.slug.clone(),
        complexity: t.complexity.clone(),
    }
}

fn git_to_context(git: Option<&GitContext>) -> crate::context::types::GitContext {
    match git {
        Some(g) => crate::context::types::GitContext {
            repo_url: g.repo_url.clone(),
            branch: g.branch.clone(),
            target_branch: g.target_branch.clone(),
            work_dir: g.work_dir.clone(),
        },
        None => crate::context::types::GitContext::default(),
    }
}

/// Determines which prompt template to use for agent execution.
#[derive(Debug, Clone)]
pub enum TaskType {
    Task(Task),
    Chat {
        user_message: String,
        history: Vec<ChatMessage>,
        project_id: Uuid,
    },
    Gate {
        previous_result: StepResult,
        criteria: String,
        project_id: Uuid,
        /// Optional task context so gate templates can access {{ task.* }} variables.
        task: Option<Task>,
    },
    CouncilSubtask {
        parent_task: String,
        subtask_description: String,
        subtask_index: usize,
        project_id: Uuid,
    },
    /// A cron-triggered routine execution. Runs the routine on a repeating
    /// schedule until a completion signal is received or the timeout expires.
    Cron {
        /// Optional task context — present when a cron step runs inside a
        /// task-triggered routine, absent for standalone cron routines.
        task: Option<Task>,
        project_id: Uuid,
        schedule: crate::routines::types::CronSchedule,
        /// Optional persisted next fire time used when restoring active
        /// schedules after a worker restart.
        start_at: Option<chrono::DateTime<chrono::Utc>>,
        timeout: Duration,
    },
    Heartbeat {
        agent_id: Uuid,
        project_id: Option<Uuid>,
        interval: Duration,
        start_at: Option<chrono::DateTime<chrono::Utc>>,
        previous_output: Option<String>,
        last_run_at: Option<chrono::DateTime<chrono::Utc>>,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    },
}

/// Tracks delegation depth and prevents cycles in agent-to-agent delegation.
#[derive(Debug, Clone)]
pub struct DelegationContext {
    pub current_depth: u32,
    pub max_depth: u32,
    pub ancestor_agent_ids: HashSet<Uuid>,
}

impl DelegationContext {
    /// Create a new root delegation context with the given max depth.
    pub fn new(max_depth: u32) -> Self {
        Self {
            current_depth: 0,
            max_depth,
            ancestor_agent_ids: HashSet::new(),
        }
    }

    /// Create a child context for a delegated agent. Returns `None` if max depth reached.
    pub fn child(&self, parent_id: Uuid) -> Option<Self> {
        let next_depth = self.current_depth + 1;
        if next_depth >= self.max_depth {
            return None;
        }
        let mut ancestors = self.ancestor_agent_ids.clone();
        ancestors.insert(parent_id);
        Some(Self {
            current_depth: next_depth,
            max_depth: self.max_depth,
            ancestor_agent_ids: ancestors,
        })
    }

    /// Check if delegating to the target would create a cycle.
    pub fn would_cycle(&self, target_id: Uuid) -> bool {
        self.ancestor_agent_ids.contains(&target_id)
    }
}

/// Active domain state carried across turns within a domain session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveDomain {
    pub session_id: Uuid,
    pub domain_id: Uuid,
    pub domain_name: String,
    pub manifest: DomainManifest,
}

/// Outcome of a single turn in the agent loop.
#[derive(Debug)]
pub enum TurnOutcome {
    /// The LLM returned tool calls that need execution.
    ToolCalls,
    /// The LLM returned a final text response (no tool calls).
    Final(String),
    /// The loop hit the max iteration limit.
    MaxIterations(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_result_serde_roundtrip() {
        let result = StepResult {
            passed: true,
            output: "done".into(),
            data: serde_json::json!({"key": "value"}),
            step_name: "step-1".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: StepResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.passed);
        assert_eq!(parsed.output, "done");
    }

    #[test]
    fn delegation_context_new() {
        let ctx = DelegationContext::new(3);
        assert_eq!(ctx.current_depth, 0);
        assert_eq!(ctx.max_depth, 3);
        assert!(ctx.ancestor_agent_ids.is_empty());
    }

    #[test]
    fn delegation_context_child_increments_depth() {
        let ctx = DelegationContext::new(3);
        let parent_id = Uuid::new_v4();
        let child = ctx.child(parent_id).unwrap();
        assert_eq!(child.current_depth, 1);
        assert!(child.ancestor_agent_ids.contains(&parent_id));
    }

    #[test]
    fn delegation_context_max_depth_blocks() {
        let ctx = DelegationContext::new(2);
        let id1 = Uuid::new_v4();
        let child = ctx.child(id1).unwrap();
        assert_eq!(child.current_depth, 1);
        // depth 1 + 1 = 2 >= max_depth 2, so child returns None
        let id2 = Uuid::new_v4();
        assert!(child.child(id2).is_none());
    }

    #[test]
    fn delegation_context_cycle_detection() {
        let ctx = DelegationContext::new(5);
        let id1 = Uuid::new_v4();
        let child = ctx.child(id1).unwrap();
        assert!(child.would_cycle(id1));
        assert!(!child.would_cycle(Uuid::new_v4()));
    }
}
