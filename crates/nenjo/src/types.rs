use std::collections::HashSet;
use std::time::Duration;

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

/// Prompt configuration parsed from AgentManifestRole.prompt_config JSONB.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptConfig {
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub developer_prompt: String,
    #[serde(default)]
    pub templates: PromptTemplates,
    #[serde(default)]
    pub memory_profile: MemoryProfile,
}

/// Configures what a role wants its memory system to focus on.
///
/// Core focus = cross-project expertise that persists everywhere.
/// Project focus = project-specific knowledge.
/// Priority categories = categories this role cares about most (prioritized in retrieval).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryProfile {
    /// What this role wants remembered as core (cross-project) knowledge.
    #[serde(default)]
    pub core_focus: Vec<String>,
    /// What this role wants remembered as project-specific knowledge.
    #[serde(default)]
    pub project_focus: Vec<String>,
}

impl MemoryProfile {
    /// Returns true if the profile has any focus configured.
    pub fn is_empty(&self) -> bool {
        self.core_focus.is_empty() && self.project_focus.is_empty()
    }
}

/// Task-specific prompt templates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTemplates {
    #[serde(default)]
    pub task_execution: String,
    #[serde(default)]
    pub chat_task: String,
    #[serde(default)]
    pub gate_eval: String,
    #[serde(default)]
    pub cron_task: String,
    #[serde(default)]
    pub heartbeat_task: String,
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

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Active domain state carried across turns within a domain session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveDomain {
    pub session_id: Uuid,
    pub domain_id: Uuid,
    pub domain_name: String,
    pub manifest: DomainSessionManifest,
    pub turn_number: u32,
    pub artifact_draft: serde_json::Value,
}

/// Top-level domain manifest (deserialized from JSONB).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DomainSessionManifest {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub domain_type: String,
    #[serde(default)]
    pub prompt: DomainPromptConfig,
    #[serde(default)]
    pub tools: DomainToolConfig,
    #[serde(default)]
    pub artifact: Option<DomainArtifactConfig>,
    #[serde(default)]
    pub session: DomainSessionConfig,
}

/// Prompt overlay configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DomainPromptConfig {
    #[serde(default)]
    pub system_addon: Option<String>,
    #[serde(default)]
    pub guidelines: Vec<String>,
    #[serde(default)]
    pub entry_message: Option<String>,
    #[serde(default)]
    pub exit_message: Option<String>,
}

/// Tool allow/deny filter with optional injection for profile escalation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DomainToolConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub additional: Vec<String>,
    /// Built-in tool names to inject (grants write tools that the understanding default excludes).
    #[serde(default)]
    pub inject_tools: Vec<String>,
    /// Tool categories to inject. Values: "read", "write", "readwrite".
    #[serde(default)]
    pub inject_categories: Vec<String>,
    /// Additional platform scopes to grant when this domain is active.
    /// Escalates the role's base platform_scopes with extra MCP access.
    /// Example: ["projects:write", "routines:write"]
    #[serde(default)]
    pub additional_scopes: Vec<String>,
    /// Activate external MCP servers by name for this domain.
    /// Example: ["github", "linear"]
    #[serde(default)]
    pub activate_mcp: Vec<String>,
    /// Activate abilities by name for this domain session.
    /// Example: ["code-review", "migration-writer"]
    #[serde(default)]
    pub activate_abilities: Vec<String>,
}

/// Artifact output configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainArtifactConfig {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub filename_template: Option<String>,
    #[serde(default)]
    pub schema: Option<serde_json::Value>,
    #[serde(default)]
    pub output_template: Option<String>,
}

/// Session behavior configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainSessionConfig {
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_auto_save_interval")]
    pub auto_save_interval: u32,
    #[serde(default)]
    pub exit_commands: Vec<String>,
}

impl Default for DomainSessionConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            auto_save_interval: default_auto_save_interval(),
            exit_commands: vec![
                "/exit".to_string(),
                "/done".to_string(),
                "/finish".to_string(),
            ],
        }
    }
}

fn default_max_turns() -> u32 {
    50
}

fn default_auto_save_interval() -> u32 {
    5
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
    fn prompt_config_default_is_empty() {
        let config = PromptConfig::default();
        assert!(config.system_prompt.is_empty());
        assert!(config.templates.task_execution.is_empty());
    }

    #[test]
    fn domain_tool_config_inject_fields() {
        let json = serde_json::json!({
            "inject_tools": ["file_write", "file_edit"],
            "inject_categories": ["write"]
        });
        let config: DomainToolConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.inject_tools, vec!["file_write", "file_edit"]);
        assert_eq!(config.inject_categories, vec!["write"]);
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

    #[test]
    fn prompt_config_from_json() {
        let json = serde_json::json!({
            "system_prompt": "You are a helpful assistant.",
            "templates": {
                "task_execution": "Implement: {{ title }}",
                "chat_task": "Respond to: {{ chat.message }}",
                "gate_eval": "Evaluate: {{ criteria }}"
            },
            "output_templates": {
                "agent_done": "Task complete.",
                "gate_pass": "PASS",
                "gate_fail": "FAIL"
            }
        });
        let config: PromptConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.system_prompt, "You are a helpful assistant.");
        assert_eq!(config.templates.task_execution, "Implement: {{ title }}");
    }
}
