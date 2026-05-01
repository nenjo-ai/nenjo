//! Search the agent's long-term memory across project, core, and shared scopes.

use crate::memory::AgentMemory;
use crate::memory::SearchFilters;
use crate::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;

/// Let the agent search its memory across project, core, and shared namespaces.
pub struct MemoryRecallTool {
    memory: Arc<dyn AgentMemory>,
    /// Role-specific, project-scoped namespace.
    role_namespace: String,
    /// Cross-project role knowledge namespace.
    core_namespace: Option<String>,
    /// Project-wide shared namespace.
    shared_namespace: String,
}

impl MemoryRecallTool {
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

    /// Resolve which namespaces to search based on scope.
    fn namespaces_for_scope(&self, scope: &str) -> Vec<String> {
        match scope {
            "core" => self.core_namespace.iter().cloned().collect(),
            "shared" => vec![self.shared_namespace.clone()],
            "project" => vec![self.role_namespace.clone()],
            "all" => {
                let mut ns = vec![self.role_namespace.clone()];
                if let Some(ref core) = self.core_namespace
                    && !ns.contains(core)
                {
                    ns.push(core.clone());
                }
                if !ns.contains(&self.shared_namespace) {
                    ns.push(self.shared_namespace.clone());
                }
                ns
            }
            _ => vec![self.role_namespace.clone()],
        }
    }
}

#[async_trait]
impl Tool for MemoryRecallTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn name(&self) -> &str {
        "recall_memory"
    }

    fn description(&self) -> &str {
        "Search long-term memory for relevant facts, preferences, or context. Returns scored results ranked by relevance. Use 'scope' to target: 'project' (default), 'core', 'shared', or 'all' namespaces."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or phrase to search for in memory"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 5)"
                },
                "category": {
                    "type": "string",
                    "description": "Filter by category (e.g. 'preferences', 'architecture')"
                },
                "min_confidence": {
                    "type": "number",
                    "description": "Minimum confidence threshold (0.0-1.0)"
                },
                "max_age_days": {
                    "type": "integer",
                    "description": "Max age of memories in days"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared", "all"],
                    "description": "Memory scope to search: 'project' (default), 'core', 'shared', or 'all'"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(5, |v| v as usize);

        let scope = args
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("project");

        let namespaces = self.namespaces_for_scope(scope);
        if namespaces.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("No namespace available for the requested scope.".into()),
            });
        }

        let filters = SearchFilters {
            category: args
                .get("category")
                .and_then(|v| v.as_str())
                .map(String::from),
            min_confidence: args
                .get("min_confidence")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32),
            max_age_days: args
                .get("max_age_days")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            status: None,
        };

        let has_filters = filters.category.is_some()
            || filters.min_confidence.is_some()
            || filters.max_age_days.is_some();

        // Search across all target namespaces
        let mut all_items = Vec::new();
        for ns in &namespaces {
            let search_result = if has_filters {
                self.memory
                    .search_items_filtered(ns, query, limit, &filters)
                    .await
            } else {
                self.memory.search_items(ns, query, limit).await
            };
            if let Ok(items) = search_result {
                all_items.extend(items);
            }
        }

        // Sort by score descending and truncate to limit
        all_items.sort_by(|a, b| {
            b.score
                .unwrap_or(0.0)
                .partial_cmp(&a.score.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_items.truncate(limit);

        if all_items.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No memories found matching that query.".into(),
                error: None,
            });
        }

        let mut output = format!("Found {} memories:\n", all_items.len());
        for item in &all_items {
            let score_str = item
                .score
                .map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
            let _ = writeln!(
                output,
                "- [{}] {}{score_str} (confidence: {:.0}%, id: {})",
                item.category,
                item.fact,
                item.confidence * 100.0,
                item.id,
            );
        }
        // Touch accessed items to track usage
        for item in &all_items {
            let _ = self.memory.touch_item(&item.id).await;
        }
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}
