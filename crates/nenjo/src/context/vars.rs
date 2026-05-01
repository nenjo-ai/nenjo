//! Template variable building — converts Nenjo types to `HashMap<String, String>`.

use std::collections::HashMap;

use crate::builtin_knowledge::builtin_documents_summary;
use crate::context::{MemoryProfileContext, TaskContext};

use super::types::{
    AbilityContext, AgentContext, AvailableAbilitiesContext, AvailableAgentsContext,
    AvailableDomainsContext, DomainContext, GitContext, ProjectContext, RoutineContext,
};

/// All renderable data for template variable substitution.
///
/// This is the Nenjo-specific typed intermediate. Call [`to_vars()`](RenderContextVars::to_vars)
/// to produce a generic `HashMap<String, String>` for the template engine.
#[derive(Debug, Clone, Default)]
pub struct RenderContextVars {
    // Grouped vars — singular/active context
    pub _self: AgentContext,
    pub task: TaskContext,
    pub project: ProjectContext,
    pub routine: RoutineContext,
    pub memory_profile: MemoryProfileContext,
    pub git: GitContext,

    // Available collections (plural)
    pub available_agents: Vec<AgentContext>,
    pub available_abilities: Vec<AbilityContext>,
    pub available_domains: Vec<DomainContext>,

    // Separate vars
    pub chat_message: String,
    pub gate_criteria: String,
    pub gate_previous_output: String,
    pub heartbeat_previous_output: String,
    pub heartbeat_last_run_at: String,
    pub heartbeat_next_run_at: String,
    pub subtask_parent_task: String,
    pub subtask_description: String,
    pub timestamp: String,

    // Pre-computed memory vars (memories, memories.core, etc.)
    pub memory_vars: HashMap<String, String>,

    // Pre-computed resource vars (resources, resources.project, resources.workspace)
    pub resource_vars: HashMap<String, String>,

    // Pre-computed documents XML for project.documents
    pub documents_xml: String,

    // Context blocks (pre-rendered, keyed by dotted path)
    pub context_blocks: HashMap<String, String>,
}

impl RenderContextVars {
    /// Convert to a flat `HashMap<String, String>` for the template engine.
    ///
    /// Only non-empty values are included. Context blocks are merged in
    /// with their dotted keys preserved.
    pub fn to_vars(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();

        // Grouped XML renders (singular entity = full XML)
        // Only serialize if the entity has meaningful data.
        let self_xml = if self._self.id.is_nil() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(&self._self, 2)
        };
        let project_xml = if self.project.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(&self.project, 2)
        };
        let routine_xml = if self.routine.name.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(&self.routine, 2)
        };
        let task_xml = if self.task.id.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(&self.task, 2)
        };
        let memory_profile_xml = if self.memory_profile.core_focus.is_none()
            && self.memory_profile.project_focus.is_none()
            && self.memory_profile.shared_focus.is_none()
        {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(&self.memory_profile, 2)
        };
        let memory_profile_core_xml = match &self.memory_profile.core_focus {
            Some(focus) => nenjo_xml::to_xml_pretty(focus, 2),
            None => String::new(),
        };
        let memory_profile_project_xml = match &self.memory_profile.project_focus {
            Some(focus) => nenjo_xml::to_xml_pretty(focus, 2),
            None => String::new(),
        };
        let memory_profile_shared_xml = match &self.memory_profile.shared_focus {
            Some(focus) => nenjo_xml::to_xml_pretty(focus, 2),
            None => String::new(),
        };
        let git_xml = if self.git.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(&self.git, 2)
        };

        // Available collections (plural = XML list)
        let available_agents_xml = if self.available_agents.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(
                &AvailableAgentsContext {
                    agents: self.available_agents.clone(),
                },
                2,
            )
        };
        let available_abilities_xml = if self.available_abilities.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(
                &AvailableAbilitiesContext {
                    abilities: self.available_abilities.clone(),
                },
                2,
            )
        };
        let available_domains_xml = if self.available_domains.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(
                &AvailableDomainsContext {
                    domains: self.available_domains.clone(),
                },
                2,
            )
        };
        // Convert UUIDs, skipping nil values
        let agent_id = if self._self.id.is_nil() {
            String::new()
        } else {
            self._self.id.to_string()
        };
        let routine_id = if self.routine.id.is_nil() {
            String::new()
        } else {
            self.routine.id.to_string()
        };

        let fields: &[(&str, &str)] = &[
            // Task — singular XML + fields
            ("task", task_xml.as_str()),
            ("task.id", &self.task.id),
            ("task.title", &self.task.title),
            ("task.description", &self.task.description),
            ("task.acceptance_criteria", &self.task.acceptance_criteria),
            ("task.tags", &self.task.tags),
            ("task.source", &self.task.source),
            ("task.status", &self.task.status),
            ("task.priority", &self.task.priority),
            ("task.type", &self.task.task_type),
            ("task.slug", &self.task.slug),
            ("task.complexity", &self.task.complexity),
            // Chat
            ("chat.message", &self.chat_message),
            // Gate
            ("gate.criteria", &self.gate_criteria),
            ("gate.previous_output", &self.gate_previous_output),
            // Heartbeat
            ("heartbeat.previous_output", &self.heartbeat_previous_output),
            ("heartbeat.last_run_at", &self.heartbeat_last_run_at),
            ("heartbeat.next_run_at", &self.heartbeat_next_run_at),
            // Subtask
            ("subtask.parent_task", &self.subtask_parent_task),
            ("subtask.description", &self.subtask_description),
            // Agent (self) — singular XML + fields
            ("self", self_xml.as_str()),
            ("agent.id", agent_id.as_str()),
            ("agent.role", &self._self.role),
            ("agent.name", &self._self.display_name),
            ("agent.model", &self._self.model_name),
            (
                "agent.description",
                self._self.description.as_deref().unwrap_or(""),
            ),
            // Project — singular XML + fields
            ("project", project_xml.as_str()),
            ("project.id", &self.project.id),
            ("project.name", &self.project.name),
            ("project.slug", &self.project.slug),
            ("project.description", &self.project.description),
            ("project.metadata", &self.project.metadata),
            ("project.working_dir", &self.project.working_dir),
            // Routine — singular XML + fields
            ("routine", routine_xml.as_str()),
            ("routine.id", routine_id.as_str()),
            ("routine.name", &self.routine.name),
            ("routine.execution_id", &self.routine.execution_id),
            // Routine step context
            ("routine.step.name", &self.routine.step.name),
            ("routine.step.type", &self.routine.step.step_type),
            ("routine.step.metadata", &self.routine.step.metadata),
            // Git — singular XML + fields
            ("git", git_xml.as_str()),
            ("git.current_branch", &self.git.branch),
            ("git.target_branch", &self.git.target_branch),
            ("git.work_dir", &self.git.work_dir),
            ("git.repo_url", &self.git.repo_url),
            // Global
            ("global.timestamp", &self.timestamp),
            // Memory profile — singular XML + sub-keys
            ("memory_profile", memory_profile_xml.as_str()),
            (
                "memory_profile.core_focus",
                memory_profile_core_xml.as_str(),
            ),
            (
                "memory_profile.project_focus",
                memory_profile_project_xml.as_str(),
            ),
            (
                "memory_profile.shared_focus",
                memory_profile_shared_xml.as_str(),
            ),
            // Available collections — plural XML
            ("available_agents", available_agents_xml.as_str()),
            ("available_abilities", available_abilities_xml.as_str()),
            ("available_domains", available_domains_xml.as_str()),
        ];

        for (key, value) in fields {
            if !value.is_empty() {
                vars.insert(key.to_string(), value.to_string());
            }
        }

        // Memory vars (memories, memories.core, etc.)
        vars.extend(self.memory_vars.clone());

        // Resource vars (resources, resources.project, resources.workspace)
        vars.extend(self.resource_vars.clone());

        // Documents
        if !self.documents_xml.is_empty() {
            vars.insert("project.documents".to_string(), self.documents_xml.clone());
        }
        vars.insert("builtin.documents".to_string(), builtin_documents_summary());

        // Merge context blocks
        vars.extend(self.context_blocks.clone());

        vars
    }
}
