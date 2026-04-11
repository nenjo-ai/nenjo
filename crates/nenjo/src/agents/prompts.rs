use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::context::types::{
    AbilityContext, AgentContext, DomainContext, RenderContextBlock, RoutineContext,
};

use crate::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, DomainManifest, ProjectManifest,
    RoutineManifest,
};
use crate::types::{ActiveDomain, RenderContextVars};

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

/// Task-specific prompt templates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTemplates {
    /// Template for task execution. Backend stores this as `task_task`.
    #[serde(default, alias = "task_task")]
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

/// Configures what a role wants its memory system to focus on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryProfile {
    /// What this role wants remembered as core (cross-project) knowledge.
    pub core_focus: Vec<String>,
    /// What this role wants remembered as project-specific knowledge.
    pub project_focus: Vec<String>,
    /// What this role should store in shared scope for other agents to reference.
    #[serde(default)]
    pub shared_focus: Vec<String>,
}

impl MemoryProfile {
    pub fn is_empty(&self) -> bool {
        self.core_focus.is_empty() && self.project_focus.is_empty() && self.shared_focus.is_empty()
    }
}

#[derive(Clone)]
pub struct PromptContext {
    /// Agent name (e.g. "manager", "architect").
    pub agent_name: String,
    /// Agent description for template variable rendering.
    pub agent_description: String,
    /// All available agents (behavioral identities).
    pub available_agents: Vec<AgentManifest>,
    /// All available routines.
    pub available_routines: Vec<RoutineManifest>,
    /// All available projects (used to resolve project slugs for paths).
    pub current_project: ProjectManifest,
    /// Abilities available to this agent (assigned + domain-activated).
    pub available_abilities: Vec<AbilityManifest>,
    /// Domains assigned to this agent (for context injection).
    pub available_domains: Vec<DomainManifest>,
    /// MCP server metadata for context injection (name, description).
    pub mcp_server_info: Vec<(String, String)>,
    /// Agent's platform scopes for MCP integration context.
    pub platform_scopes: Vec<String>,
    /// Active domain session (if the user is in a domain like /prd).
    pub active_domain: Option<ActiveDomain>,
    /// Whether the active domain's developer prompt addon should be appended.
    pub append_active_domain_addon: bool,
    /// Workspace directory containing project document subdirs.
    pub docs_base_dir: Option<PathBuf>,
    /// Routine/project-level context fields injected by the executor.
    pub render_ctx_extra: RenderContextVars,
}
// ---------------------------------------------------------------------------
// Manifest → Render type conversions
// ---------------------------------------------------------------------------

pub fn render_agent(a: &AgentManifest) -> AgentContext {
    AgentContext {
        id: a.id,
        role: a.name.clone(),
        display_name: a.name.clone(),
        model_name: a.model_name.clone().unwrap_or_default(),
        description: a.description.clone(),
    }
}

pub fn render_ability(a: &AbilityManifest) -> AbilityContext {
    AbilityContext {
        name: a.name.clone(),
        tool_name: crate::agents::abilities::ability_tool_name(a),
        activate_when: a.activation_condition.clone(),
    }
}

pub fn render_routine(r: &RoutineManifest) -> RoutineContext {
    RoutineContext {
        id: r.id,
        name: r.name.clone(),
        execution_id: String::new(),
        description: r.description.clone(),
        step: Default::default(),
    }
}

pub fn render_domain(d: &DomainManifest) -> DomainContext {
    DomainContext {
        name: d.name.clone(),
        display_name: d.display_name.clone(),
        command: d.command.clone(),
        description: d.description.clone(),
        category: d.category.clone(),
    }
}

pub fn render_context_block(b: &ContextBlockManifest) -> RenderContextBlock {
    RenderContextBlock {
        name: b.name.clone(),
        path: b.path.clone(),
        template: b.template.clone(),
    }
}
