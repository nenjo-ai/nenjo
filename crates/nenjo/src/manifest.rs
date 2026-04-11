//! Manifest types — the canonical representation of platform resources.
//!
//! A `Manifest` is the full catalog of agents, models, routines, domains,
//! abilities, and context blocks. It can be loaded from multiple
//! sources (API backend, local `.nenjo/` folder) and merged.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Manifest {
    pub user_id: uuid::Uuid,
    /// The API key ID used for bootstrap (if API key auth).
    /// Workers use this as their stable identifier for presence tracking.
    #[serde(default)]
    pub api_key_id: Option<uuid::Uuid>,
    pub routines: Vec<RoutineManifest>,
    pub models: Vec<ModelManifest>,
    pub agents: Vec<AgentManifest>,
    pub councils: Vec<CouncilManifest>,
    pub domains: Vec<DomainManifest>,
    pub projects: Vec<ProjectManifest>,
    pub lambdas: Vec<LambdaManifest>,
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
        // Keep the first non-nil user_id
        if self.user_id.is_nil() && !other.user_id.is_nil() {
            self.user_id = other.user_id;
        }
        // Keep the first api_key_id
        if self.api_key_id.is_none() && other.api_key_id.is_some() {
            self.api_key_id = other.api_key_id;
        }
        self.routines.extend(other.routines);
        self.models.extend(other.models);
        self.agents.extend(other.agents);
        self.councils.extend(other.councils);
        self.domains.extend(other.domains);
        self.projects.extend(other.projects);
        self.lambdas.extend(other.lambdas);
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
}

// ---------------------------------------------------------------------------
// Individual resource types
// ---------------------------------------------------------------------------

/// A deterministic script step executed by the lambda runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaManifest {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub path: String,
    pub body: String,
    pub interpreter: String,
}

/// An external MCP server (stdio or HTTP transport) providing tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerManifest {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub transport: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    #[serde(default)]
    pub env_schema: serde_json::Value,
    pub icon: Option<String>,
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub is_system: bool,
}

/// A project — the top-level organizational unit for agents, routines, and documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub slug: String,
    pub description: Option<String>,
    #[serde(default)]
    pub is_system: bool,
    pub settings: serde_json::Value,
}

/// A routine — a DAG of steps (agent, lambda, gate, council) with edges defining control flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineManifest {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger: String,
    pub is_active: bool,
    pub is_default: bool,
    pub max_retries: i32,
    pub metadata: serde_json::Value,
    pub steps: Vec<RoutineStepManifest>,
    pub edges: Vec<RoutineEdgeManifest>,
}

/// A single step in a routine DAG (agent, lambda, gate, or council).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineStepManifest {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub name: String,
    pub step_type: String,
    pub model_id: Option<Uuid>,
    pub council_id: Option<Uuid>,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    #[serde(default)]
    pub lambda_id: Option<Uuid>,
    pub config: serde_json::Value,
    pub order_index: i32,
}

/// A directed edge between two routine steps with an optional condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineEdgeManifest {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub source_step_id: Uuid,
    pub target_step_id: Uuid,
    pub condition: String,
    pub metadata: serde_json::Value,
}

/// An LLM model configuration (provider, model name, temperature).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelManifest {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub model: String,
    #[serde(default = "default_model_provider")]
    pub model_provider: String,
    pub temperature: Option<f64>,
    pub tags: Vec<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

fn default_model_provider() -> String {
    "openai".to_string()
}

/// An agent definition — prompt config, assigned model, domains, and tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub prompt_config: serde_json::Value,
    pub color: Option<String>,
    pub model_id: Option<Uuid>,
    pub model_name: Option<String>,
    /// Domain IDs assigned to this agent (bootstrap: "domains", detail: "domain_ids").
    #[serde(default, alias = "domain_ids")]
    pub domains: Vec<Uuid>,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_server_ids: Vec<Uuid>,
    /// Ability IDs assigned to this agent (bootstrap: "abilities", detail: "ability_ids").
    #[serde(default, alias = "ability_ids")]
    pub abilities: Vec<Uuid>,
    /// When true, prompt_config updates are blocked.
    #[serde(default)]
    pub prompt_locked: bool,
}

/// An ability — a sub-execution mode with its own prompt and filtered tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityManifest {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub activation_condition: String,
    pub prompt: String,
    pub platform_scopes: Vec<String>,
    pub mcp_server_ids: Vec<Uuid>,
    pub tool_filter: serde_json::Value,
    #[serde(default)]
    pub is_system: bool,
}

/// Lightweight ability metadata — kept in memory for lazy loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityMeta {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub activation_condition: String,
    #[serde(default)]
    pub is_system: bool,
}

impl From<&AbilityManifest> for AbilityMeta {
    fn from(a: &AbilityManifest) -> Self {
        Self {
            id: a.id,
            name: a.name.clone(),
            path: a.path.clone(),
            display_name: a.display_name.clone(),
            description: a.description.clone(),
            activation_condition: a.activation_condition.clone(),
            is_system: a.is_system,
        }
    }
}

/// Lightweight context block metadata — kept in memory for lazy loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlockMeta {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub is_system: bool,
}

impl From<&ContextBlockManifest> for ContextBlockMeta {
    fn from(b: &ContextBlockManifest) -> Self {
        Self {
            id: b.id,
            name: b.name.clone(),
            path: b.path.clone(),
            is_system: b.is_system,
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
    pub is_system: bool,
}

/// A domain — an activatable execution mode with its own prompt addons and tool config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainManifest {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub display_name: String,
    pub description: Option<String>,
    pub command: String,
    pub manifest: serde_json::Value,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub is_system: bool,
    pub source_domain_id: Option<Uuid>,
}

/// A council — a multi-agent deliberation group with a leader and delegation strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilManifest {
    pub id: Uuid,
    pub name: String,
    pub delegation_strategy: String,
    pub leader_agent_id: Uuid,
    pub members: Vec<CouncilMemberManifest>,
}

/// A member of a council with a priority ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilMemberManifest {
    pub agent_id: Uuid,
    pub agent_name: String,
    pub priority: i32,
}
