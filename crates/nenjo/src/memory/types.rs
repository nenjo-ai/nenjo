//! Memory types: items, summaries, scopes.

use serde::{Deserialize, Serialize};

/// An atomic fact stored in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub fact: String,
    pub category: String,
    pub confidence: f64,
    pub status: MemoryStatus,
    pub access_count: u64,
    pub created_at: String,
}

/// Category summary — a rolled-up view of items in a category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySummary {
    pub category: String,
    pub text: String,
    pub item_count: u32,
}

/// Lifecycle status of a memory item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MemoryStatus {
    #[default]
    Active,
    Superseded,
    Archived,
}

impl std::fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Superseded => write!(f, "superseded"),
            Self::Archived => write!(f, "archived"),
        }
    }
}

/// 3-tier namespace scoping for memory isolation.
///
/// Each agent gets three namespaces:
/// - **project**: per-agent, per-project facts (the default)
/// - **core**: cross-project agent expertise (persists across projects)
/// - **shared**: visible to all agents in the project
#[derive(Debug, Clone)]
pub struct MemoryScope {
    /// Per-agent-per-project: `"proj:{project_id}:agent:{agent_id}"`
    pub project: String,
    /// Cross-project agent knowledge: `"agent:{agent_id}:core"`
    pub core: String,
    /// All agents in project: `"proj:{project_id}:shared"`
    pub shared: String,
}

impl MemoryScope {
    /// Build a scope from project and agent IDs.
    pub fn new(project_id: &str, agent_id: &str) -> Self {
        Self {
            project: format!("proj:{project_id}:agent:{agent_id}"),
            core: format!("agent:{agent_id}:core"),
            shared: format!("proj:{project_id}:shared"),
        }
    }

    /// Resolve a scope name ("project", "core", "shared") to a namespace string.
    pub fn resolve(&self, scope: &str) -> &str {
        match scope {
            "core" => &self.core,
            "shared" => &self.shared,
            _ => &self.project,
        }
    }

    /// All three namespaces for exhaustive search.
    pub fn all(&self) -> [&str; 3] {
        [&self.project, &self.core, &self.shared]
    }
}
