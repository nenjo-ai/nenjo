//! Delete files or directories with path sandboxing and symlink-escape protection.

use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Delete a file or directory in the workspace.
pub struct FileDeleteTool {
    security: Arc<SecurityPolicy>,
}

impl FileDeleteTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for FileDeleteTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn name(&self) -> &str {
        "file_delete"
    }

    fn description(&self) -> &str {
        "Delete a file in the workspace. Directories require recursive=true. \
        This tool refuses to delete the workspace root, .git directories, or symlinks."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file or directory within the workspace"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Required to delete directories. Defaults to false."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;
        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

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

        if path.trim().is_empty() || path == "." {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Refusing to delete workspace root".into()),
            });
        }

        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        let full_path = self.security.workspace_dir.join(path);
        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(path) => path,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve path: {e}")),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Resolved path escapes workspace: {}",
                    resolved_path.display()
                )),
            });
        }

        let workspace_root = self
            .security
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.security.workspace_dir.clone());
        if resolved_path == workspace_root {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Refusing to delete workspace root".into()),
            });
        }

        if resolved_path
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Refusing to delete .git paths".into()),
            });
        }

        let metadata = match tokio::fs::symlink_metadata(&full_path).await {
            Ok(metadata) => metadata,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to inspect path: {e}")),
                });
            }
        };

        if metadata.file_type().is_symlink() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Refusing to delete symlink: {}",
                    full_path.display()
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

        if metadata.is_dir() {
            if !recursive {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Refusing to delete directory without recursive=true".into()),
                });
            }
            match tokio::fs::remove_dir_all(&resolved_path).await {
                Ok(()) => Ok(ToolResult {
                    success: true,
                    output: format!("Deleted directory {path}"),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to delete directory: {e}")),
                }),
            }
        } else {
            match tokio::fs::remove_file(&resolved_path).await {
                Ok(()) => Ok(ToolResult {
                    success: true,
                    output: format!("Deleted file {path}"),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to delete file: {e}")),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::security::{AutonomyLevel, SecurityPolicy};

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

    fn test_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        test_security_with(workspace, AutonomyLevel::Supervised, 1000)
    }

    #[test]
    fn file_delete_name() {
        let tool = FileDeleteTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "file_delete");
    }

    #[test]
    fn file_delete_schema_has_path() {
        let tool = FileDeleteTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["recursive"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
    }

    #[tokio::test]
    async fn file_delete_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete-me.txt");
        tokio::fs::write(&path, "remove").await.unwrap();

        let tool = FileDeleteTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "delete-me.txt"}))
            .await
            .unwrap();

        assert!(result.success, "{result:?}");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn file_delete_requires_recursive_for_directory() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("subdir"))
            .await
            .unwrap();

        let tool = FileDeleteTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"path": "subdir"})).await.unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("recursive=true")
        );
        assert!(dir.path().join("subdir").exists());
    }

    #[tokio::test]
    async fn file_delete_removes_directory_with_recursive() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("subdir/nested"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("subdir/nested/file.txt"), "remove")
            .await
            .unwrap();

        let tool = FileDeleteTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "subdir", "recursive": true}))
            .await
            .unwrap();

        assert!(result.success, "{result:?}");
        assert!(!dir.path().join("subdir").exists());
    }

    #[tokio::test]
    async fn file_delete_blocks_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let tool = FileDeleteTool::new(test_security(dir.path().to_path_buf()));

        let result = tool.execute(json!({"path": "."})).await.unwrap();

        assert!(!result.success);
        assert!(dir.path().exists());
    }

    #[tokio::test]
    async fn file_delete_blocks_dot_git_paths() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join(".git"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join(".git/config"), "config")
            .await
            .unwrap();

        let tool = FileDeleteTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"path": ".git/config"})).await.unwrap();

        assert!(!result.success);
        assert!(dir.path().join(".git/config").exists());
    }

    #[tokio::test]
    async fn file_delete_blocks_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete-me.txt");
        tokio::fs::write(&path, "remove").await.unwrap();

        let tool = FileDeleteTool::new(test_security_with(
            dir.path().to_path_buf(),
            AutonomyLevel::ReadOnly,
            1000,
        ));
        let result = tool
            .execute(json!({"path": "delete-me.txt"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn file_delete_rate_limited() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete-me.txt");
        tokio::fs::write(&path, "remove").await.unwrap();

        let tool = FileDeleteTool::new(test_security_with(
            dir.path().to_path_buf(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool
            .execute(json!({"path": "delete-me.txt"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_delete_refuses_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        tokio::fs::write(&target, "keep").await.unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let tool = FileDeleteTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"path": "link.txt"})).await.unwrap();

        assert!(!result.success);
        assert!(target.exists());
        assert!(link.exists());
    }
}
