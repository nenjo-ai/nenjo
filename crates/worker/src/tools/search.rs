//! Unified model-facing search tool for file contents and file paths.

mod content;
mod files;

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::security::SecurityPolicy;
use super::{Tool, ToolCategory, ToolResult};
use content::ContentSearchEngine;
use files::FileSearchEngine;

/// Search file contents or discover files inside the scoped workspace.
pub struct SearchTool {
    content: ContentSearchEngine,
    files: FileSearchEngine,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SearchMode {
    Content,
    Files,
}

impl SearchTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            content: ContentSearchEngine::new(security.clone()),
            files: FileSearchEngine::new(security),
        }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Search within the scoped workspace. Use mode=content for regex matches in file contents, or mode=files to find paths with a glob pattern."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "mode": {"type": "string", "const": "content"},
                        "pattern": {"type": "string", "description": "Regular expression to search for"},
                        "path": {"type": "string", "description": "Directory relative to the workspace root", "default": "."},
                        "output_mode": {
                            "type": "string",
                            "enum": ["content", "files_with_matches", "count"],
                            "default": "content"
                        },
                        "include": {"type": "string", "description": "File glob filter such as '*.rs'"},
                        "case_sensitive": {"type": "boolean", "default": true},
                        "context_before": {"type": "integer", "minimum": 0, "default": 0},
                        "context_after": {"type": "integer", "minimum": 0, "default": 0},
                        "multiline": {"type": "boolean", "default": false},
                        "max_results": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 1000}
                    },
                    "required": ["mode", "pattern"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "mode": {"type": "string", "const": "files"},
                        "pattern": {"type": "string", "description": "Glob pattern such as '**/*.rs' or 'src/**/mod.rs'"}
                    },
                    "required": ["mode", "pattern"],
                    "additionalProperties": false
                }
            ]
        })
    }

    async fn execute(&self, mut args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let mode: SearchMode = serde_json::from_value(
            args.get("mode")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing 'mode' parameter"))?,
        )?;
        let Some(arguments) = args.as_object_mut() else {
            anyhow::bail!("Search arguments must be an object");
        };
        arguments.remove("mode");

        match mode {
            SearchMode::Content => self.content.execute(args).await,
            SearchMode::Files => self.files.execute(args).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::security::AutonomyLevel;

    fn test_tool(workspace: &std::path::Path) -> SearchTool {
        SearchTool::new(Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.to_path_buf(),
            ..SecurityPolicy::default()
        }))
    }

    #[test]
    fn search_tool_has_tagged_modes() {
        let tool = test_tool(std::path::Path::new("."));
        let schema = tool.parameters_schema();

        assert_eq!(tool.name(), "search");
        assert_eq!(schema["oneOf"].as_array().unwrap().len(), 2);
        assert_eq!(schema["oneOf"][0]["properties"]["mode"]["const"], "content");
        assert_eq!(schema["oneOf"][1]["properties"]["mode"]["const"], "files");
    }

    #[tokio::test]
    async fn search_dispatches_content_and_file_modes() {
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::write(workspace.path().join("main.rs"), "fn main() {}")
            .await
            .unwrap();
        let tool = test_tool(workspace.path());

        let content = tool
            .execute(json!({"mode": "content", "pattern": "fn main"}))
            .await
            .unwrap();
        let files = tool
            .execute(json!({"mode": "files", "pattern": "**/*.rs"}))
            .await
            .unwrap();

        assert!(content.success, "{:?}", content.error);
        assert!(content.output.contains("main.rs"));
        assert!(files.success, "{:?}", files.error);
        assert!(files.output.contains("main.rs"));
    }
}
