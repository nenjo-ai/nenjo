//! Template variable building — converts Nenjo types to `HashMap<String, String>`.

use std::collections::HashMap;

use crate::context::{MemoryProfileContext, TaskContext};

use super::types::{AgentContext, GitContext, ProjectContext, RoutineContext};

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

    // Separate vars
    pub chat_message: String,
    pub timestamp: String,

    // Pre-computed memory vars (memories, memories.core, etc.)
    pub memory_vars: HashMap<String, String>,

    // Pre-computed artifact vars (artifacts, artifacts.project, artifacts.workspace)
    pub artifact_vars: HashMap<String, String>,

    // Pre-computed knowledge vars keyed by template path.
    pub knowledge_vars: HashMap<String, String>,

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
        let self_xml = if self._self.slug.is_empty() {
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
        let routine_handoffs_xml = if self.routine.handoffs.is_empty() {
            String::new()
        } else {
            nenjo_xml::to_xml_pretty(&self.routine.handoffs, 2)
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

        let fields: &[(&str, &str)] = &[
            // Task — singular XML + fields
            ("task", task_xml.as_str()),
            ("task.id", &self.task.id),
            ("task.title", &self.task.title),
            ("task.instructions", &self.task.instructions),
            // Compatibility alias for installed templates. New templates must
            // use `task.instructions`.
            ("task.description", &self.task.instructions),
            ("task.labels", &self.task.labels),
            ("task.status", &self.task.status),
            ("task.priority", &self.task.priority),
            ("task.slug", &self.task.slug),
            // Chat
            ("chat.message", &self.chat_message),
            // Agent (self) — singular XML + fields
            ("self", self_xml.as_str()),
            ("agent.slug", &self._self.slug),
            ("agent.name", &self._self.name),
            ("agent.model", &self._self.model_name),
            (
                "agent.description",
                self._self.description.as_deref().unwrap_or(""),
            ),
            // Project — singular XML + fields
            ("project", project_xml.as_str()),
            ("project.name", &self.project.name),
            ("project.slug", &self.project.slug),
            ("project.description", &self.project.description),
            ("project.context", &self.project.context),
            ("project.metadata", &self.project.metadata),
            ("project.working_dir", &self.project.working_dir),
            // Routine — singular XML + fields
            ("routine", routine_xml.as_str()),
            ("routine.slug", &self.routine.slug),
            ("routine.name", &self.routine.name),
            ("routine.execution_id", &self.routine.execution_id),
            ("routine.handoffs", routine_handoffs_xml.as_str()),
            // Routine step context
            ("routine.step.name", &self.routine.step.name),
            ("routine.step.type", &self.routine.step.step_type),
            ("routine.step.instructions", &self.routine.step.instructions),
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
        ];

        for (key, value) in fields {
            if !value.is_empty() {
                vars.insert(key.to_string(), value.to_string());
            }
        }

        // Memory vars (memories, memories.core, etc.)
        vars.extend(self.memory_vars.clone());

        // Artifact vars (artifacts, artifacts.project, artifacts.workspace)
        vars.extend(self.artifact_vars.clone());

        vars.extend(self.knowledge_vars.clone());

        // Merge context blocks
        vars.extend(self.context_blocks.clone());

        vars
    }
}

#[cfg(test)]
mod tests {
    use super::RenderContextVars;

    #[test]
    fn empty_project_slug_is_not_rendered() {
        let mut ctx = RenderContextVars::default();
        ctx.project.slug.clear();

        let vars = ctx.to_vars();

        assert_eq!(vars.get("project.slug"), None);
    }

    #[test]
    fn project_slug_is_rendered() {
        let mut ctx = RenderContextVars::default();
        ctx.project.name = "Project".to_string();
        ctx.project.slug = "project".to_string();

        let vars = ctx.to_vars();

        assert_eq!(vars.get("project.slug"), Some(&"project".to_string()));
    }
}
