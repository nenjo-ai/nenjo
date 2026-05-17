//! Write file contents with path sandboxing and auto-created parent directories.

use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Write file contents with path sandboxing
pub struct FileWriteTool {
    security: Arc<SecurityPolicy>,
}

impl FileWriteTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write contents to a file in the workspace. \
        IMPORTANT: this completely replaces the entire file with the provided content. \
        To edit an existing file, first read it with file_read, make your changes to the \
        full content, then write the entire modified content back with file_write. \
        Never write only the changed lines — always supply the complete file."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file within the workspace"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // Security check: validate path is within workspace
        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        let full_path = self.security.workspace_dir.join(path);

        let Some(parent) = full_path.parent() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid path: missing parent directory".into()),
            });
        };

        // Ensure parent directory exists
        tokio::fs::create_dir_all(parent).await?;

        // Resolve parent AFTER creation to block symlink escapes.
        let resolved_parent = match tokio::fs::canonicalize(parent).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_parent) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Resolved path escapes workspace: {}",
                    resolved_parent.display()
                )),
            });
        }

        let Some(file_name) = full_path.file_name() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid path: missing file name".into()),
            });
        };

        let resolved_target = resolved_parent.join(file_name);
        if self.security.is_managed_runtime_path(&resolved_target) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Path is managed by Nenjo runtime installs and is read-only".into()),
            });
        }

        // If the target already exists and is a symlink, refuse to follow it
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved_target).await
            && meta.file_type().is_symlink()
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Refusing to write through symlink: {}",
                    resolved_target.display()
                )),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        match tokio::fs::write(&resolved_target, content).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Written {} bytes to {path}", content.len()),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to write file: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::security::{AutonomyLevel, SecurityPolicy};

    fn test_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with(
        workspace: std::path::PathBuf,
        autonomy: AutonomyLevel,
        max_actions_per_hour: u32,
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: workspace,
            max_actions_per_hour,
            ..SecurityPolicy::default()
        })
    }

    fn temp_workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn file_write_name() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "file_write");
    }

    #[test]
    fn file_write_schema_has_path_and_content() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["content"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("content")));
    }

    #[tokio::test]
    async fn file_write_creates_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "out.txt", "content": "written!"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("8 bytes"));

        let content = tokio::fs::read_to_string(dir.join("out.txt"))
            .await
            .unwrap();
        assert_eq!(content, "written!");
    }

    #[tokio::test]
    async fn file_write_creates_parent_dirs() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "a/b/c/deep.txt", "content": "deep"}))
            .await
            .unwrap();
        assert!(result.success);

        let content = tokio::fs::read_to_string(dir.join("a/b/c/deep.txt"))
            .await
            .unwrap();
        assert_eq!(content, "deep");
    }

    #[tokio::test]
    async fn file_write_overwrites_existing() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("exist.txt"), "old")
            .await
            .unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "exist.txt", "content": "new"}))
            .await
            .unwrap();
        assert!(result.success);

        let content = tokio::fs::read_to_string(dir.join("exist.txt"))
            .await
            .unwrap();
        assert_eq!(content, "new");
    }

    #[tokio::test]
    async fn file_write_blocks_path_traversal() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "../../etc/evil", "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn file_write_blocks_absolute_path() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"path": "/etc/evil", "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn file_write_missing_path_param() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"content": "data"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_write_missing_content_param() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"path": "file.txt"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_write_empty_content() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "empty.txt", "content": ""}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("0 bytes"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = temp_workspace();
        let root = temp.path().to_path_buf();
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        symlink(&outside, workspace.join("escape_dir")).unwrap();

        let tool = FileWriteTool::new(test_security(workspace.clone()));
        let result = tool
            .execute(json!({"path": "escape_dir/hijack.txt", "content": "bad"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("escapes workspace")
        );
        assert!(!outside.join("hijack.txt").exists());
    }

    #[tokio::test]
    async fn file_write_blocks_readonly_mode() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileWriteTool::new(test_security_with(dir.clone(), AutonomyLevel::ReadOnly, 20));
        let result = tool
            .execute(json!({"path": "out.txt", "content": "should-block"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("read-only"));
        assert!(!dir.join("out.txt").exists());
    }

    #[tokio::test]
    async fn file_write_blocks_when_rate_limited() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileWriteTool::new(test_security_with(
            dir.clone(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool
            .execute(json!({"path": "out.txt", "content": "should-block"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Rate limit exceeded")
        );
        assert!(!dir.join("out.txt").exists());
    }
}
