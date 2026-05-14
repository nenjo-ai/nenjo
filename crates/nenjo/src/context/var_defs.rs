//! Canonical template variable definitions.
//!
//! This is the single source of truth for all available template variables.
//! The backend serves these via the API, and the frontend renders them.

use serde::Serialize;

/// A single template variable definition.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateVarDef {
    pub name: &'static str,
    pub description: &'static str,
    pub group: &'static str,
}

/// A group of template variables.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateVarGroup {
    pub name: &'static str,
    pub variables: Vec<TemplateVarDef>,
}

/// Returns all available template variable definitions, grouped.
pub fn template_var_groups() -> Vec<TemplateVarGroup> {
    vec![
        TemplateVarGroup {
            name: "Agent (self)",
            variables: vec![
                TemplateVarDef {
                    name: "self",
                    description: "Full XML of the executing agent (id, role, name, model, description)",
                    group: "Agent (self)",
                },
                TemplateVarDef {
                    name: "agent.id",
                    description: "UUID of the executing agent",
                    group: "Agent (self)",
                },
                TemplateVarDef {
                    name: "agent.role",
                    description: "Internal role name (e.g. coder, reviewer)",
                    group: "Agent (self)",
                },
                TemplateVarDef {
                    name: "agent.name",
                    description: "Display name of the executing agent",
                    group: "Agent (self)",
                },
                TemplateVarDef {
                    name: "agent.model",
                    description: "LLM model name (e.g. gpt-4o, claude-sonnet)",
                    group: "Agent (self)",
                },
                TemplateVarDef {
                    name: "agent.description",
                    description: "Description of the agent's purpose",
                    group: "Agent (self)",
                },
            ],
        },
        TemplateVarGroup {
            name: "Task",
            variables: vec![
                TemplateVarDef {
                    name: "task",
                    description: "Full XML of the current task (all fields)",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.id",
                    description: "Unique task UUID",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.title",
                    description: "Short summary of the work item",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.description",
                    description: "Full description with requirements",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.acceptance_criteria",
                    description: "Conditions for completion",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.tags",
                    description: "Comma-separated labels",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.source",
                    description: "Origin — 'user' or agent name when routed from a gate",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.status",
                    description: "Current status (open, ready, in_progress, done)",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.priority",
                    description: "Priority level (low, medium, high, critical)",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.type",
                    description: "Work type (bug, feature, task)",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.slug",
                    description: "URL-safe task identifier",
                    group: "Task",
                },
                TemplateVarDef {
                    name: "task.complexity",
                    description: "Estimated complexity score",
                    group: "Task",
                },
            ],
        },
        TemplateVarGroup {
            name: "Chat",
            variables: vec![TemplateVarDef {
                name: "chat.message",
                description: "Current user chat message text",
                group: "Chat",
            }],
        },
        TemplateVarGroup {
            name: "Project",
            variables: vec![
                TemplateVarDef {
                    name: "project",
                    description: "Full XML of the active project (id, name, description, git)",
                    group: "Project",
                },
                TemplateVarDef {
                    name: "project.id",
                    description: "Project UUID",
                    group: "Project",
                },
                TemplateVarDef {
                    name: "project.name",
                    description: "Display name of the project",
                    group: "Project",
                },
                TemplateVarDef {
                    name: "project.slug",
                    description: "URL-safe project identifier",
                    group: "Project",
                },
                TemplateVarDef {
                    name: "project.description",
                    description: "Project overview and goals",
                    group: "Project",
                },
                TemplateVarDef {
                    name: "project.context",
                    description: "Rendered global project context from project settings",
                    group: "Project",
                },
                TemplateVarDef {
                    name: "project.metadata",
                    description: "Custom key-value metadata from project settings (XML)",
                    group: "Project",
                },
                TemplateVarDef {
                    name: "project.working_dir",
                    description: "Absolute path to the project workspace directory",
                    group: "Project",
                },
            ],
        },
        TemplateVarGroup {
            name: "Knowledge",
            variables: vec![
                TemplateVarDef {
                    name: "builtin.nenjo",
                    description: "Compact XML listing of the built-in Nenjo knowledge pack",
                    group: "Knowledge",
                },
                TemplateVarDef {
                    name: "lib.<pack_slug>",
                    description: "Compact XML listing of a workspace knowledge pack",
                    group: "Knowledge",
                },
            ],
        },
        TemplateVarGroup {
            name: "Routine",
            variables: vec![
                TemplateVarDef {
                    name: "routine",
                    description: "Full XML of the active routine (id, name, execution_id, step)",
                    group: "Routine",
                },
                TemplateVarDef {
                    name: "routine.id",
                    description: "UUID of the active routine",
                    group: "Routine",
                },
                TemplateVarDef {
                    name: "routine.name",
                    description: "Name of the active routine",
                    group: "Routine",
                },
                TemplateVarDef {
                    name: "routine.execution_id",
                    description: "Unique ID for the current routine execution run",
                    group: "Routine",
                },
                TemplateVarDef {
                    name: "routine.step.name",
                    description: "Name of the currently executing routine step",
                    group: "Routine",
                },
                TemplateVarDef {
                    name: "routine.step.type",
                    description: "Type of the current step (agent, gate, council, lambda, terminal, cron)",
                    group: "Routine",
                },
                TemplateVarDef {
                    name: "routine.step.metadata",
                    description: "Arbitrary JSON metadata from the current step's config",
                    group: "Routine",
                },
            ],
        },
        TemplateVarGroup {
            name: "Gate",
            variables: vec![
                TemplateVarDef {
                    name: "gate.criteria",
                    description: "Pass/fail criteria for gate evaluation",
                    group: "Gate",
                },
                TemplateVarDef {
                    name: "gate.previous_output",
                    description: "Output from the previous step being evaluated",
                    group: "Gate",
                },
            ],
        },
        TemplateVarGroup {
            name: "Heartbeat",
            variables: vec![
                TemplateVarDef {
                    name: "heartbeat.previous_output",
                    description: "Final output from the previous heartbeat run, if any",
                    group: "Heartbeat",
                },
                TemplateVarDef {
                    name: "heartbeat.last_run_at",
                    description: "Timestamp of the previous heartbeat run, if any",
                    group: "Heartbeat",
                },
                TemplateVarDef {
                    name: "heartbeat.next_run_at",
                    description: "Scheduled timestamp for the next heartbeat run",
                    group: "Heartbeat",
                },
            ],
        },
        TemplateVarGroup {
            name: "Subtask",
            variables: vec![
                TemplateVarDef {
                    name: "subtask.parent_task",
                    description: "Parent task context for council subtasks",
                    group: "Subtask",
                },
                TemplateVarDef {
                    name: "subtask.description",
                    description: "Subtask description assigned by the council",
                    group: "Subtask",
                },
            ],
        },
        TemplateVarGroup {
            name: "Git",
            variables: vec![
                TemplateVarDef {
                    name: "git",
                    description: "Full XML of the git context (branch, target, work_dir, repo_url)",
                    group: "Git",
                },
                TemplateVarDef {
                    name: "git.current_branch",
                    description: "Current working branch (e.g. agent/<run>/<slug>)",
                    group: "Git",
                },
                TemplateVarDef {
                    name: "git.target_branch",
                    description: "Target branch for merges/PRs (from project settings)",
                    group: "Git",
                },
                TemplateVarDef {
                    name: "git.work_dir",
                    description: "Absolute path to the worktree directory for this execution",
                    group: "Git",
                },
                TemplateVarDef {
                    name: "git.repo_url",
                    description: "Remote clone URL for the repository",
                    group: "Git",
                },
            ],
        },
        TemplateVarGroup {
            name: "Available",
            variables: vec![
                TemplateVarDef {
                    name: "available_agents",
                    description: "XML list of all available agents (id, role, name, model, description)",
                    group: "Available",
                },
                TemplateVarDef {
                    name: "available_abilities",
                    description: "XML list of all available abilities (name, activation condition)",
                    group: "Available",
                },
                TemplateVarDef {
                    name: "available_domains",
                    description: "XML list of all available domains (name, command, description)",
                    group: "Available",
                },
            ],
        },
        TemplateVarGroup {
            name: "Memory",
            variables: vec![
                TemplateVarDef {
                    name: "memories",
                    description: "Full memories XML (all tiers combined)",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "memories.core",
                    description: "Agent's core memories (cross-project)",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "memories.project",
                    description: "Agent's memories for the current project",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "memories.shared",
                    description: "Project memories shared across agents",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "memory_profile",
                    description: "Full XML of the agent's memory focus areas",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "memory_profile.core_focus",
                    description: "Cross-project expertise focus areas",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "memory_profile.project_focus",
                    description: "Project-specific knowledge focus areas",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "memory_profile.shared_focus",
                    description: "Knowledge to store in shared scope for other agents",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "artifacts",
                    description: "Index of available artifacts (project + workspace)",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "artifacts.project",
                    description: "Project-scoped artifact index",
                    group: "Memory",
                },
                TemplateVarDef {
                    name: "artifacts.workspace",
                    description: "Workspace-global artifact index",
                    group: "Memory",
                },
            ],
        },
        TemplateVarGroup {
            name: "Global",
            variables: vec![TemplateVarDef {
                name: "global.timestamp",
                description: "Current UTC timestamp (ISO 8601)",
                group: "Global",
            }],
        },
    ]
}

/// Returns a flat list of all template variable definitions.
pub fn template_var_defs() -> Vec<TemplateVarDef> {
    template_var_groups()
        .into_iter()
        .flat_map(|g| g.variables)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::template_var_groups;

    #[test]
    fn builtin_knowledge_has_own_group() {
        let groups = template_var_groups();
        let project = groups
            .iter()
            .find(|group| group.name == "Project")
            .expect("project group");
        let knowledge = groups
            .iter()
            .find(|group| group.name == "Knowledge")
            .expect("knowledge group");

        assert!(
            !project
                .variables
                .iter()
                .any(|var| var.name == "builtin.nenjo")
        );
        assert!(
            knowledge
                .variables
                .iter()
                .any(|var| var.name == "builtin.nenjo" && var.group == "Knowledge")
        );
    }

    #[test]
    fn chat_message_is_declared() {
        let chat = template_var_groups()
            .into_iter()
            .find(|group| group.name == "Chat")
            .expect("chat group");

        assert!(
            chat.variables
                .iter()
                .any(|var| var.name == "chat.message" && var.group == "Chat")
        );
    }
}
