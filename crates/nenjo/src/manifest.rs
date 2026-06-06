//! Manifest types — the canonical representation of platform resources.
//!
//! A `Manifest` is the full catalog of agents, models, routines, domains,
//! abilities, and context blocks. It can be loaded from multiple
//! sources (API backend, local `.nenjo/` folder) and merged.

use anyhow::Result;
use derive_builder::Builder;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Slug;

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
    /// Merge another manifest into this one.
    ///
    /// Collections are last-write-wins by manifest resource identity so package,
    /// platform, global, and local loaders can model normal dependency
    /// precedence. Executable SDK resources are keyed by slug; platform-owned
    /// support resources keep UUID identity until their schemas are slug-native.
    pub fn merge(&mut self, other: Manifest) {
        merge_by_slug(&mut self.routines, other.routines);
        merge_by_slug(&mut self.models, other.models);
        merge_by_slug(&mut self.agents, other.agents);
        merge_by_slug(&mut self.councils, other.councils);
        merge_by_slug(&mut self.domains, other.domains);
        merge_by_slug(&mut self.projects, other.projects);
        merge_by_slug(&mut self.mcp_servers, other.mcp_servers);
        merge_by_slug(&mut self.abilities, other.abilities);
        merge_by_slug(&mut self.context_blocks, other.context_blocks);
    }

    /// Insert or replace a single resource in this manifest.
    pub fn upsert_resource(&mut self, resource: ManifestResource) {
        match resource {
            ManifestResource::Agent(item) => upsert_by_slug(&mut self.agents, item),
            ManifestResource::Model(item) => upsert_by_slug(&mut self.models, item),
            ManifestResource::Routine(item) => upsert_by_slug(&mut self.routines, item),
            ManifestResource::Project(item) => upsert_by_slug(&mut self.projects, item),
            ManifestResource::Council(item) => upsert_by_slug(&mut self.councils, item),
            ManifestResource::Domain(item) => upsert_by_slug(&mut self.domains, item),
            ManifestResource::McpServer(item) => upsert_by_slug(&mut self.mcp_servers, item),
            ManifestResource::Ability(item) => upsert_by_slug(&mut self.abilities, item),
            ManifestResource::ContextBlock(item) => upsert_by_slug(&mut self.context_blocks, item),
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

fn upsert_by_slug<T: HasManifestSlug>(items: &mut Vec<T>, incoming: T) {
    let incoming_slug = incoming.manifest_slug();
    if let Some(existing) = items
        .iter_mut()
        .find(|item| item.manifest_slug() == incoming_slug)
    {
        *existing = incoming;
    } else {
        items.push(incoming);
    }
}

fn merge_by_slug<T: HasManifestSlug>(items: &mut Vec<T>, incoming: Vec<T>) {
    for item in incoming {
        upsert_by_slug(items, item);
    }
}

pub(crate) trait HasManifestSlug {
    fn manifest_slug(&self) -> Slug;
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

impl HasManifestSlug for McpServerManifest {
    fn manifest_slug(&self) -> Slug {
        Slug::derive(&self.name)
    }
}

/// A project — the top-level organizational unit for agents, routines, and documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub id: Uuid,
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
    pub settings: serde_json::Value,
}

impl HasManifestSlug for ProjectManifest {
    fn manifest_slug(&self) -> Slug {
        self.slug.clone()
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

impl HasManifestSlug for RoutineManifest {
    fn manifest_slug(&self) -> Slug {
        Slug::derive(&self.name)
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
    pub entry_steps: Vec<Slug>,
}

/// A single step in a routine DAG (agent, gate, council, cron, or terminal).
#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned", setter(prefix = "with", into))]
pub struct RoutineStepManifest {
    #[builder(default = "Uuid::new_v4()")]
    pub id: Uuid,
    pub slug: Slug,
    pub routine: Slug,
    pub name: String,
    #[builder(default)]
    pub step_type: RoutineStepType,
    #[builder(default, setter(strip_option))]
    pub council: Option<Slug>,
    #[builder(default, setter(strip_option))]
    pub agent: Option<Slug>,
    #[builder(default = "serde_json::json!({})")]
    pub config: serde_json::Value,
    #[builder(default)]
    pub order_index: i32,
}

impl RoutineStepManifest {
    pub fn builder() -> RoutineStepManifestBuilder {
        RoutineStepManifestBuilder::default()
    }
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
    pub routine: Slug,
    pub source_step: Slug,
    pub target_step: Slug,
    pub condition: RoutineEdgeCondition,
    #[serde(default)]
    pub metadata: serde_json::Value,
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
    /// Stable manifest ID for this model configuration.
    pub id: Uuid,
    /// Human-readable model configuration name.
    pub name: String,
    pub description: Option<String>,
    /// Provider-specific model identifier, for example `openai/gpt-4.1`.
    pub model: String,
    /// Provider registry key, for example `openrouter`, `openai`, or `anthropic`.
    pub model_provider: String,
    /// Optional sampling temperature for calls using this model.
    pub temperature: Option<f64>,
    /// Optional provider base URL override.
    pub base_url: Option<String>,
}

impl HasManifestSlug for ModelManifest {
    fn manifest_slug(&self) -> Slug {
        model_manifest_slug(&self.model_provider, &self.model)
    }
}

pub fn model_manifest_slug(model_provider: &str, model: &str) -> Slug {
    Slug::derive(format!(
        "{}_{}",
        slug_segment(model_provider, "provider"),
        slug_segment(model, "model")
    ))
}

fn slug_segment(value: &str, fallback: &str) -> String {
    let mut segment = String::new();
    let mut previous_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            segment.push(ch.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !segment.is_empty() {
            segment.push('_');
            previous_separator = true;
        }
    }
    while segment.ends_with('_') {
        segment.pop();
    }
    if segment.is_empty() {
        fallback.to_string()
    } else {
        segment
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
    #[serde(default)]
    pub core_focus: Vec<String>,
    /// What this role wants remembered as project-specific knowledge.
    #[serde(default)]
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

/// An agent definition — prompt config, assigned model, domains, and tools.
///
/// Runtime-created agents, including ephemeral sub-agents, can use the builder
/// with only a name and prompt:
///
/// ```
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use nenjo::manifest::AgentManifest;
///
/// let agent = AgentManifest::builder()
///     .with_name("reviewer")
///     .with_system_prompt("Act as a focused review worker.")
///     .with_developer_prompt("Be concise and evidence-driven.")
///     .with_task_template("Task: {{ task.title }}\n\n{{ task.description }}")
///     .build()?;
/// # let _ = agent;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned", setter(prefix = "with", into))]
pub struct AgentManifest {
    #[builder(default = "Uuid::new_v4()")]
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub slug: Option<Slug>,
    #[builder(default, setter(strip_option))]
    pub description: Option<String>,
    pub prompt_config: PromptConfig,
    #[builder(default, setter(strip_option))]
    pub color: Option<String>,
    #[builder(default, setter(strip_option))]
    pub model: Option<Slug>,
    #[builder(default)]
    /// Domain slugs assigned to this agent.
    pub domains: Vec<Slug>,
    #[builder(default)]
    pub platform_scopes: Vec<String>,
    #[builder(default)]
    /// MCP server slugs assigned to this agent.
    pub mcp_servers: Vec<Slug>,
    /// Ability slugs assigned to this agent.
    #[serde(default)]
    #[builder(default)]
    pub abilities: Vec<String>,
    /// When true, prompt_config updates are blocked.
    #[builder(default)]
    pub prompt_locked: bool,
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub heartbeat: Option<AgentHeartbeatManifest>,
}

impl AgentManifest {
    /// Create a builder for an agent manifest.
    pub fn builder() -> AgentManifestBuilder {
        AgentManifestBuilder::default()
    }

    /// Return the canonical selector slug for this agent.
    pub fn slug(&self) -> Slug {
        self.manifest_slug()
    }
}

impl AgentManifestBuilder {
    /// Set the system prompt without manually constructing [`PromptConfig`].
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        let mut prompt_config = self.prompt_config.take().unwrap_or_default();
        prompt_config.system_prompt = prompt.into();
        self.prompt_config = Some(prompt_config);
        self
    }

    /// Set the developer prompt without manually constructing [`PromptConfig`].
    pub fn with_developer_prompt(mut self, prompt: impl Into<String>) -> Self {
        let mut prompt_config = self.prompt_config.take().unwrap_or_default();
        prompt_config.developer_prompt = prompt.into();
        self.prompt_config = Some(prompt_config);
        self
    }

    /// Set the task execution template without manually constructing [`PromptConfig`].
    pub fn with_task_template(mut self, template: impl Into<String>) -> Self {
        let mut prompt_config = self.prompt_config.take().unwrap_or_default();
        prompt_config.templates.task_execution = template.into();
        self.prompt_config = Some(prompt_config);
        self
    }
}

impl HasManifestSlug for AgentManifest {
    fn manifest_slug(&self) -> Slug {
        self.slug
            .clone()
            .unwrap_or_else(|| Slug::derive(&self.name))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeatManifest {
    pub id: Uuid,
    pub agent: Slug,
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
    /// Developer prompt appended while an agent is executing inside this ability.
    pub developer_prompt: String,
}

/// An ability — a sub-execution mode with its own prompt and filtered tools.
///
/// Runtime-created abilities can use the builder with only a name and prompt.
/// The optional `path` field defaults to `None`, which places the ability at
/// the root of local manifest trees.
///
/// ```
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use nenjo::manifest::AbilityManifest;
///
/// let ability = AbilityManifest::builder()
///     .with_name("review")
///     .with_description("Reviews code changes")
///     .with_activation_condition("When code review is requested")
///     .with_prompt("Focus on correctness, regressions, and tests.")
///     .build()?;
/// # let _ = ability;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned", setter(prefix = "with", into))]
pub struct AbilityManifest {
    /// Stable UUID for this ability resource.
    #[builder(default = "Uuid::new_v4()")]
    pub id: Uuid,
    /// Stable slug used by agents to assign and invoke this ability.
    pub name: String,
    /// Optional folder path used only for local manifest tree organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub path: Option<String>,
    /// Human-readable summary of what this ability does.
    #[builder(default, setter(strip_option))]
    pub description: Option<String>,
    /// Condition shown by `list_assigned_abilities` to help the model decide when to invoke this ability.
    #[builder(default)]
    pub activation_condition: String,
    /// Developer prompt applied while the ability sub-execution runs.
    pub prompt_config: AbilityPromptConfig,
    /// Platform permissions available while this ability runs.
    #[builder(default)]
    pub platform_scopes: Vec<String>,
    /// MCP server slugs made available while this ability runs.
    #[builder(default)]
    pub mcp_servers: Vec<Slug>,
    /// Source classification for lifecycle behavior such as native, skill, or package.
    #[serde(default = "default_ability_source_type")]
    #[builder(default = "default_ability_source_type()")]
    pub source_type: String,
    /// Whether the ability is source-managed and should not be edited directly.
    #[serde(default)]
    #[builder(default)]
    pub read_only: bool,
    /// Source/install/runtime metadata carried with this ability.
    #[serde(default)]
    #[builder(default)]
    pub metadata: serde_json::Value,
}

fn default_ability_source_type() -> String {
    "native".to_string()
}

impl HasManifestSlug for AbilityManifest {
    fn manifest_slug(&self) -> Slug {
        Slug::derive(&self.name)
    }
}

impl AbilityManifest {
    /// Create a builder for an ability manifest.
    pub fn builder() -> AbilityManifestBuilder {
        AbilityManifestBuilder::default()
    }
}

impl AbilityManifestBuilder {
    /// Set the developer prompt without manually constructing [`AbilityPromptConfig`].
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        let mut prompt_config = self.prompt_config.take().unwrap_or_default();
        prompt_config.developer_prompt = prompt.into();
        self.prompt_config = Some(prompt_config);
        self
    }
}

/// Lightweight ability metadata — kept in memory for lazy loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityMeta {
    pub id: Uuid,
    pub name: String,
    pub path: Option<String>,
    pub description: Option<String>,
    pub activation_condition: String,
}

impl From<&AbilityManifest> for AbilityMeta {
    fn from(a: &AbilityManifest) -> Self {
        Self {
            id: a.id,
            name: a.name.clone(),
            path: a.path.clone(),
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

impl ContextBlockManifest {
    pub fn slug(&self) -> Slug {
        context_block_slug(&self.path, &self.name)
    }
}

impl HasManifestSlug for ContextBlockManifest {
    fn manifest_slug(&self) -> Slug {
        self.slug()
    }
}

pub fn context_block_slug(path: &str, name: &str) -> Slug {
    if path.trim().is_empty() {
        Slug::derive(name)
    } else {
        Slug::derive(format!("{}/{}", path, name))
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
    /// Ability slugs activated by this domain.
    #[serde(default)]
    pub abilities: Vec<String>,
    pub mcp_servers: Vec<Slug>,
    pub prompt_config: DomainPromptConfig,
}

impl HasManifestSlug for DomainManifest {
    fn manifest_slug(&self) -> Slug {
        self.slug()
    }
}

impl DomainManifest {
    pub fn slug(&self) -> Slug {
        domain_slug(&self.path, &self.name)
    }
}

pub fn domain_slug(path: &str, name: &str) -> Slug {
    if path.trim().is_empty() {
        Slug::derive(name)
    } else {
        Slug::derive(format!("{}/{}", path, name))
    }
}

/// A council — a multi-agent deliberation group with a leader and delegation strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilManifest {
    pub id: Uuid,
    pub name: String,
    pub delegation_strategy: CouncilDelegationStrategy,
    pub leader_agent: Slug,
    pub members: Vec<CouncilMemberManifest>,
}

impl HasManifestSlug for CouncilManifest {
    fn manifest_slug(&self) -> Slug {
        Slug::derive(&self.name)
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
    pub agent: Slug,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_manifest_slug_uses_provider_and_model_identity() {
        assert_eq!(
            model_manifest_slug("openrouter", "anthropic/claude-3.5-sonnet").as_str(),
            "openrouter_anthropic_claude_3_5_sonnet"
        );
    }

    #[test]
    fn routine_step_builder_defaults_runtime_fields() {
        let step = RoutineStepManifest::builder()
            .with_slug(Slug::derive("council_chat"))
            .with_routine(Slug::derive("council_chat"))
            .with_name("Council Chat")
            .with_step_type(RoutineStepType::Council)
            .with_council(Slug::derive("strategy_council"))
            .build()
            .unwrap();

        assert_eq!(step.step_type, RoutineStepType::Council);
        assert_eq!(
            step.council.as_ref().map(Slug::as_str),
            Some("strategy_council")
        );
        assert!(step.agent.is_none());
        assert_eq!(step.config, serde_json::json!({}));
        assert_eq!(step.order_index, 0);
    }
}
