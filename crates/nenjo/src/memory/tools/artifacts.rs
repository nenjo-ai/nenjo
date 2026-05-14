use std::sync::Arc;

use anyhow::Result;

use crate::tools::{Tool, ToolCategory, ToolResult};

use super::super::Memory;
use super::super::types::MemoryScope;

// ---------------------------------------------------------------------------
// ArtifactSaveTool
// ---------------------------------------------------------------------------

/// Tool for agents to save artifact documents.
pub struct ArtifactSaveTool<M: Memory + ?Sized = dyn Memory> {
    memory: Arc<M>,
    scope: MemoryScope,
    agent_name: String,
}

impl<M: Memory + ?Sized> ArtifactSaveTool<M> {
    pub fn new(memory: Arc<M>, scope: MemoryScope, agent_name: String) -> Self {
        Self {
            memory,
            scope,
            agent_name,
        }
    }
}

#[async_trait::async_trait]
impl<M> Tool for ArtifactSaveTool<M>
where
    M: Memory + ?Sized + 'static,
{
    fn name(&self) -> &str {
        "save_artifact"
    }

    fn description(&self) -> &str {
        "Save a document as a shared artifact. Artifacts are visible to all agents."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Filename for the artifact (e.g. 'auth-prd.md')"
                },
                "description": {
                    "type": "string",
                    "description": "One-line description of the artifact"
                },
                "content": {
                    "type": "string",
                    "description": "Full content of the artifact document"
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

        let ns = self.scope.resolve_artifact(scope);
        self.memory
            .save_artifact(ns, filename, description, &self.agent_name, content)
            .await?;

        Ok(ToolResult {
            success: true,
            output: format!("Saved artifact '{filename}' in {scope} scope"),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// ArtifactReadTool
// ---------------------------------------------------------------------------

/// Tool for agents to read artifact documents.
pub struct ArtifactReadTool<M: Memory + ?Sized = dyn Memory> {
    memory: Arc<M>,
    scope: MemoryScope,
}

impl<M: Memory + ?Sized> ArtifactReadTool<M> {
    pub fn new(memory: Arc<M>, scope: MemoryScope) -> Self {
        Self { memory, scope }
    }
}

#[async_trait::async_trait]
impl<M> Tool for ArtifactReadTool<M>
where
    M: Memory + ?Sized + 'static,
{
    fn name(&self) -> &str {
        "read_artifact"
    }

    fn description(&self) -> &str {
        "Read a shared artifact document by filename."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Filename of the artifact to read"
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

        let ns = self.scope.resolve_artifact(scope);
        match self.memory.read_artifact(ns, filename).await? {
            Some(content) => Ok(ToolResult {
                success: true,
                output: content,
                error: None,
            }),
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Artifact '{filename}' not found in {scope} scope")),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// ArtifactDeleteTool
// ---------------------------------------------------------------------------

/// Tool for agents to delete artifact documents.
pub struct ArtifactDeleteTool<M: Memory + ?Sized = dyn Memory> {
    memory: Arc<M>,
    scope: MemoryScope,
}

impl<M: Memory + ?Sized> ArtifactDeleteTool<M> {
    pub fn new(memory: Arc<M>, scope: MemoryScope) -> Self {
        Self { memory, scope }
    }
}

#[async_trait::async_trait]
impl<M> Tool for ArtifactDeleteTool<M>
where
    M: Memory + ?Sized + 'static,
{
    fn name(&self) -> &str {
        "delete_artifact"
    }

    fn description(&self) -> &str {
        "Delete a shared artifact document."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Filename of the artifact to delete"
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

        let ns = self.scope.resolve_artifact(scope);
        let deleted = self.memory.delete_artifact(ns, filename).await?;

        Ok(ToolResult {
            success: true,
            output: if deleted {
                format!("Deleted artifact '{filename}' from {scope} scope")
            } else {
                format!("Artifact '{filename}' not found in {scope} scope")
            },
            error: None,
        })
    }
}
