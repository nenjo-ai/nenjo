//! Manifest types — the canonical representation of platform resources.
//!
//! A `Manifest` is the full catalog of agents, models, routines, domains,
//! abilities, and context blocks. It can be loaded from multiple
//! sources (API backend, local `.nenjo/` folder) and merged.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod local;
pub mod store;

/// Loads manifest data from a source.
///
/// Implement this for each data source: Nenjo backend API, local `.nenjo/`
/// folder, or any custom provider.
#[async_trait::async_trait]
pub trait ManifestLoader: Send + Sync {
    async fn load(&self) -> Result<Manifest>;
}

/// The full catalog of platform resources.
///
/// Built by merging one or more [`ManifestLoader`] results. Each loader
/// contributes a partial manifest; the builder merges them in order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub routines: Vec<RoutineManifest>,
    pub models: Vec<ModelManifest>,
    pub agents: Vec<AgentManifest>,
    pub councils: Vec<CouncilManifest>,
    pub domains: Vec<DomainManifest>,
    pub projects: Vec<ProjectManifest>,
    pub mcp_servers: Vec<McpServerManifest>,
    pub abilities: Vec<AbilityManifest>,
    pub context_blocks: Vec<ContextBlockManifest>,
}

impl Manifest {
    /// Merge another manifest into this one (additive).
    ///
    /// Collections are extended. For context blocks, if a name collides
    /// the incoming entry wins (last-write-wins).
    pub fn merge(&mut self, other: Manifest) {
        self.routines.extend(other.routines);
        self.models.extend(other.models);
        self.agents.extend(other.agents);
        self.councils.extend(other.councils);
        self.domains.extend(other.domains);
        self.projects.extend(other.projects);
        self.mcp_servers.extend(other.mcp_servers);
        self.abilities.extend(other.abilities);

        // Context blocks: last-write-wins on name collision.
        for block in other.context_blocks {
            if let Some(existing) = self
                .context_blocks
                .iter_mut()
                .find(|b| b.name == block.name)
            {
                *existing = block;
            } else {
                self.context_blocks.push(block);
            }
        }
    }

    /// Insert or replace a single resource in this manifest.
    pub fn upsert_resource(&mut self, resource: ManifestResource) {
        match resource {
            ManifestResource::Agent(item) => upsert_by_id(&mut self.agents, item),
            ManifestResource::Model(item) => upsert_by_id(&mut self.models, item),
            ManifestResource::Routine(item) => upsert_by_id(&mut self.routines, item),
            ManifestResource::Project(item) => upsert_by_id(&mut self.projects, item),
            ManifestResource::Council(item) => upsert_by_id(&mut self.councils, item),
            ManifestResource::Domain(item) => upsert_by_id(&mut self.domains, item),
            ManifestResource::McpServer(item) => upsert_by_id(&mut self.mcp_servers, item),
            ManifestResource::Ability(item) => upsert_by_id(&mut self.abilities, item),
            ManifestResource::ContextBlock(item) => {
                if let Some(existing) = self.context_blocks.iter_mut().find(|b| b.name == item.name)
                {
                    *existing = item;
                } else {
                    self.context_blocks.push(item);
                }
            }
        }
    }

    /// Remove a single resource from this manifest by type and ID.
    pub fn delete_resource(&mut self, kind: ManifestResourceKind, id: Uuid) {
        match kind {
            ManifestResourceKind::Agent => self.agents.retain(|item| item.id != id),
            ManifestResourceKind::Model => self.models.retain(|item| item.id != id),
            ManifestResourceKind::Routine => self.routines.retain(|item| item.id != id),
            ManifestResourceKind::Project => self.projects.retain(|item| item.id != id),
            ManifestResourceKind::Council => self.councils.retain(|item| item.id != id),
            ManifestResourceKind::Domain => self.domains.retain(|item| item.id != id),
            ManifestResourceKind::McpServer => self.mcp_servers.retain(|item| item.id != id),
            ManifestResourceKind::Ability => self.abilities.retain(|item| item.id != id),
            ManifestResourceKind::ContextBlock => self.context_blocks.retain(|item| item.id != id),
        }
    }
}

fn upsert_by_id<T: HasManifestId>(items: &mut Vec<T>, incoming: T) {
    if let Some(existing) = items
        .iter_mut()
        .find(|item| item.manifest_id() == incoming.manifest_id())
    {
        *existing = incoming;
    } else {
        items.push(incoming);
    }
}

trait HasManifestId {
    fn manifest_id(&self) -> Uuid;
}

// ---------------------------------------------------------------------------
// Individual resource types
// ---------------------------------------------------------------------------

/// An external MCP server (stdio or HTTP transport) providing tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerManifest {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub transport: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    #[serde(default)]
    pub env_schema: serde_json::Value,
    #[serde(default = "default_mcp_source_type")]
    pub source_type: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

fn default_mcp_source_type() -> String {
    "native".to_string()
}

impl HasManifestId for McpServerManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

/// A project — the top-level organizational unit for agents, routines, and documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub settings: serde_json::Value,
}

impl HasManifestId for ProjectManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

/// A routine — a DAG of steps (agent, lambda, gate, council) with edges defining control flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineManifest {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger: RoutineTrigger,
    pub metadata: RoutineMetadata,
    pub steps: Vec<RoutineStepManifest>,
    pub edges: Vec<RoutineEdgeManifest>,
}

impl HasManifestId for RoutineManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RoutineTrigger {
    #[default]
    Task,
    Cron,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutineMetadata {
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub entry_step_ids: Vec<Uuid>,
}

/// A single step in a routine DAG (agent, gate, council, cron, or terminal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineStepManifest {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub name: String,
    pub step_type: RoutineStepType,
    pub council_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub config: serde_json::Value,
    pub order_index: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RoutineStepType {
    #[default]
    Agent,
    Council,
    Cron,
    Gate,
    Terminal,
    TerminalFail,
}

impl std::fmt::Display for RoutineStepType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Agent => "agent",
            Self::Council => "council",
            Self::Cron => "cron",
            Self::Gate => "gate",
            Self::Terminal => "terminal",
            Self::TerminalFail => "terminal_fail",
        };
        f.write_str(value)
    }
}

/// A directed edge between two routine steps with an optional condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineEdgeManifest {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub source_step_id: Uuid,
    pub target_step_id: Uuid,
    pub condition: RoutineEdgeCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RoutineEdgeCondition {
    #[default]
    Always,
    OnPass,
    OnFail,
}

impl RoutineEdgeCondition {
    pub fn from_str_value(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "on_pass" => Self::OnPass,
            "on_fail" => Self::OnFail,
            _ => Self::Always,
        }
    }

    pub fn is_satisfied(&self, passed: bool) -> bool {
        match self {
            Self::Always => true,
            Self::OnPass => passed,
            Self::OnFail => !passed,
        }
    }
}

/// An LLM model configuration (provider, model name, temperature).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelManifest {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub model: String,
    pub model_provider: String,
    pub temperature: Option<f64>,
    pub base_url: Option<String>,
}

impl HasManifestId for ModelManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

/// Prompt configuration parsed from AgentManifestRole.prompt_config JSONB.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptConfig {
    pub system_prompt: String,
    pub developer_prompt: String,
    pub templates: PromptTemplates,
    pub memory_profile: MemoryProfile,
}

/// Task-specific prompt templates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTemplates {
    /// Template for task execution.
    #[serde(default, rename = "task")]
    pub task_execution: String,
    #[serde(default, rename = "chat")]
    pub chat_task: String,
    #[serde(default, rename = "gate")]
    pub gate_eval: String,
    #[serde(default, rename = "cron")]
    pub cron_task: String,
    #[serde(default, rename = "heartbeat")]
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
    pub shared_focus: Vec<String>,
}

impl MemoryProfile {
    pub fn is_empty(&self) -> bool {
        self.core_focus.is_empty() && self.project_focus.is_empty() && self.shared_focus.is_empty()
    }
}

/// An agent definition — prompt config, assigned model, domains, and tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub prompt_config: PromptConfig,
    pub color: Option<String>,
    pub model_id: Option<Uuid>,
    pub domain_ids: Vec<Uuid>,
    pub platform_scopes: Vec<String>,
    pub mcp_server_ids: Vec<Uuid>,
    pub ability_ids: Vec<Uuid>,
    /// When true, prompt_config updates are blocked.
    pub prompt_locked: bool,
    #[serde(default)]
    pub heartbeat: Option<AgentHeartbeatManifest>,
}

impl HasManifestId for AgentManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeatManifest {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub interval: String,
    pub is_active: bool,
    #[serde(default)]
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Prompt configuration for an ability. This mirrors the agent pattern while
/// staying intentionally narrow: abilities contribute only developer guidance.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AbilityPromptConfig {
    pub developer_prompt: String,
}

/// An ability — a sub-execution mode with its own prompt and filtered tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityManifest {
    pub id: Uuid,
    pub name: String,
    pub tool_name: String,
    pub path: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub activation_condition: String,
    pub prompt_config: AbilityPromptConfig,
    pub platform_scopes: Vec<String>,
    pub mcp_server_ids: Vec<Uuid>,
    #[serde(default = "default_ability_source_type")]
    pub source_type: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

fn default_ability_source_type() -> String {
    "native".to_string()
}

impl HasManifestId for AbilityManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

/// Lightweight ability metadata — kept in memory for lazy loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityMeta {
    pub id: Uuid,
    pub name: String,
    pub tool_name: String,
    pub path: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub activation_condition: String,
}

impl From<&AbilityManifest> for AbilityMeta {
    fn from(a: &AbilityManifest) -> Self {
        Self {
            id: a.id,
            name: a.name.clone(),
            tool_name: a.tool_name.clone(),
            path: a.path.clone(),
            display_name: a.display_name.clone(),
            description: a.description.clone(),
            activation_condition: a.activation_condition.clone(),
        }
    }
}

/// Lightweight context block metadata — kept in memory for lazy loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlockMeta {
    pub id: Uuid,
    pub name: String,
    pub path: String,
}

impl From<&ContextBlockManifest> for ContextBlockMeta {
    fn from(b: &ContextBlockManifest) -> Self {
        Self {
            id: b.id,
            name: b.name.clone(),
            path: b.path.clone(),
        }
    }
}

/// A context block — a MiniJinja template injected into the agent's prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlockManifest {
    pub id: Uuid,
    pub name: String,
    pub path: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub template: String,
}

impl HasManifestId for ContextBlockManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

/// Prompt overlay configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DomainPromptConfig {
    pub developer_prompt_addon: Option<String>,
}

/// A domain — an activatable execution mode with its own prompt addons and tool config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainManifest {
    pub id: Uuid,
    pub name: String,
    pub path: String,
    pub display_name: String,
    pub description: Option<String>,
    pub command: String,
    pub platform_scopes: Vec<String>,
    pub ability_ids: Vec<Uuid>,
    pub mcp_server_ids: Vec<Uuid>,
    pub prompt_config: DomainPromptConfig,
}

impl HasManifestId for DomainManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

/// A council — a multi-agent deliberation group with a leader and delegation strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilManifest {
    pub id: Uuid,
    pub name: String,
    pub delegation_strategy: CouncilDelegationStrategy,
    pub leader_agent_id: Uuid,
    pub members: Vec<CouncilMemberManifest>,
}

impl HasManifestId for CouncilManifest {
    fn manifest_id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CouncilDelegationStrategy {
    #[default]
    Decompose,
    Dynamic,
    Broadcast,
    RoundRobin,
    Vote,
}

/// A member of a council with a priority ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilMemberManifest {
    pub agent_id: Uuid,
    pub agent_name: String,
    pub priority: i32,
}

/// A typed manifest resource mutation or payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resource_type", content = "resource")]
#[allow(clippy::large_enum_variant)]
pub enum ManifestResource {
    Agent(AgentManifest),
    Model(ModelManifest),
    Routine(RoutineManifest),
    Project(ProjectManifest),
    Council(CouncilManifest),
    Domain(DomainManifest),
    McpServer(McpServerManifest),
    Ability(AbilityManifest),
    ContextBlock(ContextBlockManifest),
}

impl ManifestResource {
    pub fn kind(&self) -> ManifestResourceKind {
        match self {
            Self::Agent(_) => ManifestResourceKind::Agent,
            Self::Model(_) => ManifestResourceKind::Model,
            Self::Routine(_) => ManifestResourceKind::Routine,
            Self::Project(_) => ManifestResourceKind::Project,
            Self::Council(_) => ManifestResourceKind::Council,
            Self::Domain(_) => ManifestResourceKind::Domain,
            Self::McpServer(_) => ManifestResourceKind::McpServer,
            Self::Ability(_) => ManifestResourceKind::Ability,
            Self::ContextBlock(_) => ManifestResourceKind::ContextBlock,
        }
    }

    pub fn id(&self) -> Uuid {
        match self {
            Self::Agent(item) => item.id,
            Self::Model(item) => item.id,
            Self::Routine(item) => item.id,
            Self::Project(item) => item.id,
            Self::Council(item) => item.id,
            Self::Domain(item) => item.id,
            Self::McpServer(item) => item.id,
            Self::Ability(item) => item.id,
            Self::ContextBlock(item) => item.id,
        }
    }
}

/// The kind of a single manifest resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestResourceKind {
    Agent,
    Model,
    Routine,
    Project,
    Council,
    Domain,
    McpServer,
    Ability,
    ContextBlock,
}
