//! Memory and resource tools for agent use.

use std::sync::Arc;

use anyhow::Result;
use nenjo_tools::{Tool, ToolCategory, ToolResult};

use super::Memory;
use super::types::{MemoryCategory, MemoryScope};

fn normalize_memory_fact(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fact_matches(query: &str, candidate: &str) -> bool {
    let query = normalize_memory_fact(query);
    let candidate = normalize_memory_fact(candidate);
    !query.is_empty()
        && !candidate.is_empty()
        && (query == candidate || query.contains(&candidate) || candidate.contains(&query))
}

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
        "save_memory"
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
        let scope = args["scope"].as_str().unwrap_or("project");

        if fact.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("fact is required".into()),
            });
        }

        let ns = self.scope.resolve(scope);
        self.memory.append(ns, category, fact).await?;

        Ok(ToolResult {
            success: true,
            output: format!("Stored in {scope} memory (category: {category})"),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// MemoryRecallTool
// ---------------------------------------------------------------------------

/// Tool for agents to recall facts from memory.
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
        "recall_memory"
    }

    fn description(&self) -> &str {
        "Recall facts from persistent memory, optionally filtered by category."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Category to read (omit to list all categories)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared", "all"],
                    "description": "Where to search (default: 'all')"
                }
            }
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let category = args["category"].as_str();
        let scope = args["scope"].as_str().unwrap_or("all");

        let namespaces: Vec<(&str, &str)> = if scope == "all" {
            vec![
                ("project", &self.scope.project),
                ("core", &self.scope.core),
                ("shared", &self.scope.shared),
            ]
        } else {
            vec![(scope, self.scope.resolve(scope))]
        };

        let mut output = String::new();

        for (scope_name, ns) in namespaces {
            if let Some(cat_name) = category {
                if let Some(cat) = self.memory.read_category(ns, cat_name).await? {
                    output.push_str(&format!("[{scope_name}/{cat_name}]\n"));
                    for fact in &cat.facts {
                        output.push_str(&format!("  - {}\n", fact.text));
                    }
                }
            } else {
                let cats = self.memory.list_categories(ns).await?;
                if !cats.is_empty() {
                    output.push_str(&format!("[{scope_name}]\n"));
                    for cat in &cats {
                        output.push_str(&format!(
                            "  {}: {} facts\n",
                            cat.category,
                            cat.facts.len()
                        ));
                    }
                }
            }
        }

        if output.is_empty() {
            output = "No memories found.".to_string();
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
        "forget_memory"
    }

    fn description(&self) -> &str {
        "Delete a specific fact from memory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "fact": {
                    "type": "string",
                    "description": "Fact to remove. Exact text is preferred, but close paraphrases are also accepted."
                },
                "category": {
                    "type": "string",
                    "description": "Optional category the fact belongs to. Omit when unknown."
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "core", "shared"],
                    "description": "Scope to delete from (default: 'project')"
                }
            },
            "required": ["fact"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let fact = args["fact"].as_str().unwrap_or("");
        let category = args["category"].as_str().filter(|value| !value.is_empty());
        let scope = args["scope"].as_str().unwrap_or("project");

        if fact.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("fact is required".into()),
            });
        }

        let ns = self.scope.resolve(scope);
        let deleted = if let Some(category) = category {
            if self.memory.delete_fact(ns, category, fact).await? {
                true
            } else {
                delete_matching_fact(&*self.memory, ns, Some(category), fact).await?
            }
        } else {
            delete_matching_fact(&*self.memory, ns, None, fact).await?
        };

        Ok(ToolResult {
            success: true,
            output: if deleted {
                if let Some(category) = category {
                    format!("Deleted fact from {scope}/{category}")
                } else {
                    format!("Deleted fact from {scope} memory")
                }
            } else {
                if let Some(category) = category {
                    format!("Fact not found in {scope}/{category}")
                } else {
                    format!("Fact not found in {scope} memory")
                }
            },
            error: None,
        })
    }
}

async fn delete_matching_fact(
    memory: &dyn Memory,
    ns: &str,
    category: Option<&str>,
    fact: &str,
) -> Result<bool> {
    let categories: Vec<MemoryCategory> = if let Some(category) = category {
        memory
            .read_category(ns, category)
            .await?
            .into_iter()
            .collect()
    } else {
        memory.list_categories(ns).await?
    };

    for memory_category in categories {
        if let Some(matching_fact) = memory_category
            .facts
            .iter()
            .find(|candidate| fact_matches(fact, &candidate.text))
        {
            return memory
                .delete_fact(ns, &memory_category.category, &matching_fact.text)
                .await;
        }
    }

    Ok(false)
}

// ---------------------------------------------------------------------------
// ResourceSaveTool
// ---------------------------------------------------------------------------

/// Tool for agents to save resource documents.
pub struct ResourceSaveTool {
    memory: Arc<dyn Memory>,
    scope: MemoryScope,
    agent_name: String,
}

impl ResourceSaveTool {
    pub fn new(memory: Arc<dyn Memory>, scope: MemoryScope, agent_name: String) -> Self {
        Self {
            memory,
            scope,
            agent_name,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ResourceSaveTool {
    fn name(&self) -> &str {
        "save_resource"
    }

    fn description(&self) -> &str {
        "Save a document as a shared resource. Resources are visible to all agents."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Filename for the resource (e.g. 'auth-prd.md')"
                },
                "description": {
                    "type": "string",
                    "description": "One-line description of the resource"
                },
                "content": {
                    "type": "string",
                    "description": "Full content of the resource document"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "workspace"],
                    "description": "Where to save: 'project' (default) or 'workspace' (global)"
                }
            },
            "required": ["filename", "description", "content"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let filename = args["filename"].as_str().unwrap_or("");
        let description = args["description"].as_str().unwrap_or("");
        let content = args["content"].as_str().unwrap_or("");
        let scope = args["scope"].as_str().unwrap_or("project");

        if filename.is_empty() || content.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("filename and content are required".into()),
            });
        }

        let ns = self.scope.resolve_resource(scope);
        self.memory
            .save_resource(ns, filename, description, &self.agent_name, content)
            .await?;

        Ok(ToolResult {
            success: true,
            output: format!("Saved resource '{filename}' in {scope} scope"),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// ResourceReadTool
// ---------------------------------------------------------------------------

/// Tool for agents to read resource documents.
pub struct ResourceReadTool {
    memory: Arc<dyn Memory>,
    scope: MemoryScope,
}

impl ResourceReadTool {
    pub fn new(memory: Arc<dyn Memory>, scope: MemoryScope) -> Self {
        Self { memory, scope }
    }
}

#[async_trait::async_trait]
impl Tool for ResourceReadTool {
    fn name(&self) -> &str {
        "read_resource"
    }

    fn description(&self) -> &str {
        "Read a shared resource document by filename."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Filename of the resource to read"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "workspace"],
                    "description": "Where to look: 'project' (default) or 'workspace'"
                }
            },
            "required": ["filename"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let filename = args["filename"].as_str().unwrap_or("");
        let scope = args["scope"].as_str().unwrap_or("project");

        if filename.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("filename is required".into()),
            });
        }

        let ns = self.scope.resolve_resource(scope);
        match self.memory.read_resource(ns, filename).await? {
            Some(content) => Ok(ToolResult {
                success: true,
                output: content,
                error: None,
            }),
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Resource '{filename}' not found in {scope} scope")),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceDeleteTool
// ---------------------------------------------------------------------------

/// Tool for agents to delete resource documents.
pub struct ResourceDeleteTool {
    memory: Arc<dyn Memory>,
    scope: MemoryScope,
}

impl ResourceDeleteTool {
    pub fn new(memory: Arc<dyn Memory>, scope: MemoryScope) -> Self {
        Self { memory, scope }
    }
}

#[async_trait::async_trait]
impl Tool for ResourceDeleteTool {
    fn name(&self) -> &str {
        "delete_resource"
    }

    fn description(&self) -> &str {
        "Delete a shared resource document."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Filename of the resource to delete"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "workspace"],
                    "description": "Where to delete from: 'project' (default) or 'workspace'"
                }
            },
            "required": ["filename"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let filename = args["filename"].as_str().unwrap_or("");
        let scope = args["scope"].as_str().unwrap_or("project");

        if filename.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("filename is required".into()),
            });
        }

        let ns = self.scope.resolve_resource(scope);
        let deleted = self.memory.delete_resource(ns, filename).await?;

        Ok(ToolResult {
            success: true,
            output: if deleted {
                format!("Deleted resource '{filename}' from {scope} scope")
            } else {
                format!("Resource '{filename}' not found in {scope} scope")
            },
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Tool factory
// ---------------------------------------------------------------------------

/// Create all memory and resource tools for an agent.
pub fn memory_tools(
    memory: Arc<dyn Memory>,
    scope: MemoryScope,
    agent_name: &str,
) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(MemoryStoreTool::new(memory.clone(), scope.clone())),
        Arc::new(MemoryRecallTool::new(memory.clone(), scope.clone())),
        Arc::new(MemoryForgetTool::new(memory.clone(), scope.clone())),
        Arc::new(ResourceSaveTool::new(
            memory.clone(),
            scope.clone(),
            agent_name.to_string(),
        )),
        Arc::new(ResourceReadTool::new(memory.clone(), scope.clone())),
        Arc::new(ResourceDeleteTool::new(memory, scope)),
    ]
}
