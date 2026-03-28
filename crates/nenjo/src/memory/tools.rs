//! Memory tools for agent use: store, recall, forget.

use std::sync::Arc;

use anyhow::Result;
use nenjo_tools::{Tool, ToolCategory, ToolResult};

use super::Memory;
use super::types::MemoryScope;

// ---------------------------------------------------------------------------
// MemoryStoreTool
// ---------------------------------------------------------------------------

/// Tool for agents to store facts in memory.
pub struct MemoryStoreTool {
    memory: Arc<dyn Memory>,
    scope: MemoryScope,
}

impl MemoryStoreTool {
    pub fn new(memory: Arc<dyn Memory>, scope: MemoryScope) -> Self {
        Self { memory, scope }
    }
}

#[async_trait::async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn description(&self) -> &str {
        "Store a fact in persistent memory. Facts are organized by category and scope."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "fact": {
                    "type": "string",
                    "description": "The fact or insight to store"
                },
                "category": {
                    "type": "string",
                    "description": "Category for grouping (e.g. 'preferences', 'decisions', 'architecture')"
                },
                "confidence": {
                    "type": "number",
                    "description": "Confidence score from 0.0 to 1.0 (default: 0.9)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared"],
                    "description": "Where to store: 'project' (default), 'core' (cross-project), 'shared' (team-visible)"
                }
            },
            "required": ["fact", "category"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let fact = args["fact"].as_str().unwrap_or("");
        let category = args["category"].as_str().unwrap_or("general");
        let confidence = args["confidence"].as_f64().unwrap_or(0.9);
        let scope = args["scope"].as_str().unwrap_or("project");

        if fact.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("fact is required".into()),
            });
        }

        let ns = self.scope.resolve(scope);
        let id = self.memory.store(ns, fact, category, confidence).await?;

        Ok(ToolResult {
            success: true,
            output: format!("Stored in {scope} memory (id: {id}, category: {category})"),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// MemoryRecallTool
// ---------------------------------------------------------------------------

/// Tool for agents to search and recall facts from memory.
pub struct MemoryRecallTool {
    memory: Arc<dyn Memory>,
    scope: MemoryScope,
}

impl MemoryRecallTool {
    pub fn new(memory: Arc<dyn Memory>, scope: MemoryScope) -> Self {
        Self { memory, scope }
    }
}

#[async_trait::async_trait]
impl Tool for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }

    fn description(&self) -> &str {
        "Search persistent memory for facts matching a query."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords to search for"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 5)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared", "all"],
                    "description": "Where to search (default: 'project', 'all' searches everywhere)"
                }
            },
            "required": ["query"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let query = args["query"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(5) as usize;
        let scope = args["scope"].as_str().unwrap_or("project");

        if query.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("query is required".into()),
            });
        }

        let namespaces: Vec<&str> = if scope == "all" {
            self.scope.all().to_vec()
        } else {
            vec![self.scope.resolve(scope)]
        };

        let mut all_results = Vec::new();
        for ns in namespaces {
            let results = self.memory.search(ns, query, limit).await?;
            all_results.extend(results);
        }

        // Sort by confidence descending, truncate
        all_results.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_results.truncate(limit);

        if all_results.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No memories found matching that query.".into(),
                error: None,
            });
        }

        let mut output = String::new();
        for (i, item) in all_results.iter().enumerate() {
            output.push_str(&format!(
                "{}. [{}] (confidence: {:.1}) {}\n",
                i + 1,
                item.category,
                item.confidence,
                item.fact
            ));
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// MemoryForgetTool
// ---------------------------------------------------------------------------

/// Tool for agents to delete facts from memory.
pub struct MemoryForgetTool {
    memory: Arc<dyn Memory>,
    scope: MemoryScope,
}

impl MemoryForgetTool {
    pub fn new(memory: Arc<dyn Memory>, scope: MemoryScope) -> Self {
        Self { memory, scope }
    }
}

#[async_trait::async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn description(&self) -> &str {
        "Delete facts from memory by query, ID, or age."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search and delete matching facts"
                },
                "id": {
                    "type": "string",
                    "description": "Delete a specific fact by ID"
                },
                "prune_older_than_days": {
                    "type": "integer",
                    "description": "Delete stale facts older than N days with low access"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared"],
                    "description": "Scope to delete from (default: 'project')"
                }
            }
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let scope = args["scope"].as_str().unwrap_or("project");

        // Delete by ID
        if let Some(id) = args["id"].as_str() {
            let deleted = self.memory.delete(id).await?;
            return Ok(ToolResult {
                success: true,
                output: if deleted {
                    format!("Deleted memory {id}")
                } else {
                    format!("Memory {id} not found")
                },
                error: None,
            });
        }

        // Prune by age
        if let Some(days) = args["prune_older_than_days"].as_u64() {
            let ns = self.scope.resolve(scope);
            let count = self.memory.delete_stale(ns, days, 2).await?;
            return Ok(ToolResult {
                success: true,
                output: format!("Pruned {count} stale memories older than {days} days"),
                error: None,
            });
        }

        // Delete by query
        if let Some(query) = args["query"].as_str() {
            let ns = self.scope.resolve(scope);
            let matches = self.memory.search(ns, query, 10).await?;
            let mut deleted = 0;
            for item in &matches {
                if self.memory.delete(&item.id).await? {
                    deleted += 1;
                }
            }
            return Ok(ToolResult {
                success: true,
                output: format!("Deleted {deleted} memories matching '{query}'"),
                error: None,
            });
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Provide 'query', 'id', or 'prune_older_than_days'".into()),
        })
    }
}

/// Create all three memory tools for an agent.
pub fn memory_tools(memory: Arc<dyn Memory>, scope: MemoryScope) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(MemoryStoreTool::new(memory.clone(), scope.clone())),
        Arc::new(MemoryRecallTool::new(memory.clone(), scope.clone())),
        Arc::new(MemoryForgetTool::new(memory, scope)),
    ]
}
