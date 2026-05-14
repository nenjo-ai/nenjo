//! Store facts, preferences, and insights in the agent's long-term memory.

use crate::tools::memory::AgentMemory;
use crate::tools::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Let the agent store facts in memory with configurable scope.
///
/// Scopes:
/// - `"project"` (default) — role-specific, project-scoped namespace
/// - `"core"` — cross-project role knowledge (requires a role)
/// - `"shared"` — project-wide, visible to all roles
pub struct MemoryStoreTool {
    memory: Arc<dyn AgentMemory>,
    /// Role-specific, project-scoped namespace (default target).
    role_namespace: String,
    /// Cross-project role knowledge namespace (`None` for shared/anonymous contexts).
    core_namespace: Option<String>,
    /// Project-wide shared namespace.
    shared_namespace: String,
}

impl MemoryStoreTool {
    pub fn new(
        memory: Arc<dyn AgentMemory>,
        role_namespace: String,
        core_namespace: Option<String>,
        shared_namespace: String,
    ) -> Self {
        Self {
            memory,
            role_namespace,
            core_namespace,
            shared_namespace,
        }
    }
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn name(&self) -> &str {
        "save_memory"
    }

    fn description(&self) -> &str {
        "Store a fact, preference, or insight in long-term memory. Use category to organize: 'decisions' for choices made, 'requirements' for project needs, 'preferences' for user preferences, 'architecture' for system design. Use scope to control visibility: 'project' (default) for this project, 'core' for cross-project expertise, 'shared' for team-visible facts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "fact": {
                    "type": "string",
                    "description": "The fact or insight to remember"
                },
                "category": {
                    "type": "string",
                    "description": "Category for organizing this fact (e.g. 'decisions', 'requirements', 'preferences', 'architecture')"
                },
                "confidence": {
                    "type": "number",
                    "description": "Confidence level 0.0-1.0 (default: 0.9)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared"],
                    "description": "Memory scope: 'project' (default) for project-specific, 'core' for cross-project role expertise, 'shared' for team-visible facts"
                }
            },
            "required": ["fact", "category"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let fact = args
            .get("fact")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'fact' parameter"))?;

        let category = args
            .get("category")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'category' parameter"))?;

        #[allow(clippy::cast_possible_truncation)]
        let confidence = args
            .get("confidence")
            .and_then(|v| v.as_f64())
            .map_or(0.9, |v| v as f32);

        let scope = args
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("project");

        let namespace = match scope {
            "core" => {
                if let Some(ref ns) = self.core_namespace {
                    ns.clone()
                } else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("No core namespace available (no role assigned). Use 'project' or 'shared' scope instead.".into()),
                    });
                }
            }
            "shared" => self.shared_namespace.clone(),
            _ => self.role_namespace.clone(),
        };

        let scope_label = match scope {
            "core" => "core",
            "shared" => "shared",
            _ => "project",
        };

        match self
            .memory
            .store_item(&namespace, fact, category, confidence, None)
            .await
        {
            Ok(id) => Ok(ToolResult {
                success: true,
                output: format!("Stored {scope_label} fact in '{category}' (id: {id})"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to store memory: {e}")),
            }),
        }
    }
}
