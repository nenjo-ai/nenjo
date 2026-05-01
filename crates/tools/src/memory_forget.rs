//! Delete or prune memory items by ID, search query, or age threshold.

use crate::memory::AgentMemory;
use crate::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Let the agent delete a specific memory item by ID, search-and-delete by
/// query, or prune stale items. Supports `scope` to target project, core,
/// shared, or all namespaces.
pub struct MemoryForgetTool {
    memory: Arc<dyn AgentMemory>,
    /// Role-specific, project-scoped namespace.
    role_namespace: String,
    /// Cross-project role knowledge namespace.
    core_namespace: Option<String>,
    /// Project-wide shared namespace.
    shared_namespace: String,
}

impl MemoryForgetTool {
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
            _ => vec![self.role_namespace.clone()], // "project" or default
        }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn name(&self) -> &str {
        "forget_memory"
    }

    fn description(&self) -> &str {
        "Remove memories. Use 'query' to search and delete matching facts, 'id' to delete a specific item, or 'prune_older_than_days' to prune old memories. Use 'scope' to target: 'project' (default), 'core', 'shared', or 'all' namespaces."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query — all matching memory items will be deleted. Use this when the user asks to forget something (e.g. 'forget my name')."
                },
                "id": {
                    "type": "string",
                    "description": "The ID of a specific memory item to delete"
                },
                "prune_older_than_days": {
                    "type": "integer",
                    "description": "Delete items older than this many days with low access counts"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared", "all"],
                    "description": "Memory scope to search: 'project' (default), 'core', 'shared', or 'all' to search everywhere"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("all");

        let namespaces = self.namespaces_for_scope(scope);
        if namespaces.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("No namespace available for the requested scope.".into()),
            });
        }

        // Delete by query — search and remove all matches across target namespaces
        if let Some(query) = args.get("query").and_then(|v| v.as_str()) {
            let mut all_items = Vec::new();
            for ns in &namespaces {
                if let Ok(items) = self.memory.search_items(ns, query, 20).await {
                    all_items.extend(items)
                }
            }

            if all_items.is_empty() {
                return Ok(ToolResult {
                    success: true,
                    output: format!("No memories found matching '{query}'."),
                    error: None,
                });
            }

            let mut deleted = 0u32;
            let mut facts = Vec::new();
            let mut affected_categories: std::collections::HashMap<
                String,
                std::collections::HashSet<String>,
            > = std::collections::HashMap::new();

            for item in &all_items {
                affected_categories
                    .entry(item.namespace.clone())
                    .or_default()
                    .insert(item.category.clone());
                if let Ok(true) = self.memory.delete_item(&item.id).await {
                    deleted += 1;
                    facts.push(item.fact.clone());
                }
            }

            // Rebuild summaries for affected categories in each namespace
            for (ns, categories) in &affected_categories {
                for category in categories {
                    let remaining = self
                        .memory
                        .list_items_by_category(ns, category)
                        .await
                        .unwrap_or_default();
                    if remaining.is_empty() {
                        let _ = self.memory.delete_summary(ns, category).await;
                    } else {
                        // Rebuild summary from remaining facts
                        let summary: String = remaining
                            .iter()
                            .map(|item| item.fact.as_str())
                            .collect::<Vec<_>>()
                            .join(". ");
                        #[allow(clippy::cast_possible_truncation)]
                        let _ = self
                            .memory
                            .upsert_summary(ns, category, &summary, remaining.len() as u32)
                            .await;
                    }
                }
            }

            let facts_list = facts.join("\n  - ");
            return Ok(ToolResult {
                success: true,
                output: format!(
                    "Deleted {deleted} memory item(s) matching '{query}':\n  - {facts_list}"
                ),
                error: None,
            });
        }

        // Delete by ID (ID is globally unique, no namespace needed)
        if let Some(id) = args.get("id").and_then(|v| v.as_str()) {
            return match self.memory.delete_item(id).await {
                Ok(true) => Ok(ToolResult {
                    success: true,
                    output: format!("Deleted memory item: {id}"),
                    error: None,
                }),
                Ok(false) => Ok(ToolResult {
                    success: true,
                    output: format!("No memory item found with id: {id}"),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to delete memory: {e}")),
                }),
            };
        }

        // Prune by age — prune across all target namespaces
        if let Some(days) = args.get("prune_older_than_days").and_then(|v| v.as_u64()) {
            #[allow(clippy::cast_possible_truncation)]
            let days = days as u32;
            let mut total = 0u64;
            for ns in &namespaces {
                if let Ok(count) = self.memory.delete_stale_items(ns, days, 2).await {
                    total += count
                }
            }
            return Ok(ToolResult {
                success: true,
                output: format!("Pruned {total} stale memory items older than {days} days"),
                error: None,
            });
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Provide 'query' to search and delete, 'id' to delete a specific item, or 'prune_older_than_days' to prune old items.".into()),
        })
    }
}
