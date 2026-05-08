//! Read file contents with path sandboxing and symlink-escape protection.

use crate::security::SecurityPolicy;
use crate::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024;

/// Read file contents with path sandboxing
pub struct FileReadTool {
    security: Arc<SecurityPolicy>,
}

impl FileReadTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file in the workspace"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file within the workspace"
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

        // Record action BEFORE canonicalization so that every non-trivially-rejected
        // request consumes rate limit budget. This prevents attackers from probing
        // path existence (via canonicalize errors) without rate limit cost.
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let full_path = self.security.workspace_dir.join(path);

        // Resolve path before reading to block symlink escapes.
        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
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

        // Check file size AFTER canonicalization to prevent TOCTOU symlink bypass
        match tokio::fs::metadata(&resolved_path).await {
            Ok(meta) => {
                if meta.len() > MAX_FILE_SIZE_BYTES {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "File too large: {} bytes (limit: {MAX_FILE_SIZE_BYTES} bytes)",
                            meta.len()
                        )),
                    });
                }
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file metadata: {e}")),
                });
            }
        }

        match tokio::fs::read_to_string(&resolved_path).await {
            Ok(contents) => Ok(ToolResult {
                success: true,
                output: contents,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to read file: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

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
    fn file_read_name() {
        let tool = FileReadTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "file_read");
    }

    #[test]
    fn file_read_schema_has_path() {
        let tool = FileReadTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("path"))
        );
    }

    #[tokio::test]
    async fn file_read_existing_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let tool = FileReadTool::new(test_security(dir.clone()));
        let result = tool.execute(json!({"path": "test.txt"})).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "hello world");
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn file_read_nonexistent_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileReadTool::new(test_security(dir.clone()));
        let result = tool.execute(json!({"path": "nope.txt"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Failed to resolve"));
    }

    #[tokio::test]
    async fn file_read_blocks_path_traversal() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileReadTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "../../../etc/passwd"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn file_read_blocks_absolute_path() {
        let tool = FileReadTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"path": "/etc/passwd"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn file_read_blocks_when_rate_limited() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let tool = FileReadTool::new(test_security_with(
            dir.clone(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool.execute(json!({"path": "test.txt"})).await.unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Rate limit exceeded")
        );
    }

    #[tokio::test]
    async fn file_read_allows_readonly_mode() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "readonly ok")
            .await
            .unwrap();

        let tool = FileReadTool::new(test_security_with(dir.clone(), AutonomyLevel::ReadOnly, 20));
        let result = tool.execute(json!({"path": "test.txt"})).await.unwrap();

        assert!(result.success);
        assert_eq!(result.output, "readonly ok");
    }

    #[tokio::test]
    async fn file_read_missing_path_param() {
        let tool = FileReadTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_read_empty_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("empty.txt"), "").await.unwrap();

        let tool = FileReadTool::new(test_security(dir.clone()));
        let result = tool.execute(json!({"path": "empty.txt"})).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "");
    }

    #[tokio::test]
    async fn file_read_nested_path() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::create_dir_all(dir.join("sub/dir"))
            .await
            .unwrap();
        tokio::fs::write(dir.join("sub/dir/deep.txt"), "deep content")
            .await
            .unwrap();

        let tool = FileReadTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "sub/dir/deep.txt"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "deep content");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_read_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = temp_workspace();
        let root = temp.path().to_path_buf();
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        tokio::fs::write(outside.join("secret.txt"), "outside workspace")
            .await
            .unwrap();

        symlink(outside.join("secret.txt"), workspace.join("escape.txt")).unwrap();

        let tool = FileReadTool::new(test_security(workspace.clone()));
        let result = tool.execute(json!({"path": "escape.txt"})).await.unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("escapes workspace")
        );
    }

    #[tokio::test]
    async fn file_read_nonexistent_consumes_rate_limit_budget() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        // Allow only 2 actions total
        let tool = FileReadTool::new(test_security_with(
            dir.clone(),
            AutonomyLevel::Supervised,
            2,
        ));

        // Both reads fail (file doesn't exist) but should consume budget
        let r1 = tool.execute(json!({"path": "nope1.txt"})).await.unwrap();
        assert!(!r1.success);
        assert!(r1.error.as_ref().unwrap().contains("Failed to resolve"));

        let r2 = tool.execute(json!({"path": "nope2.txt"})).await.unwrap();
        assert!(!r2.success);
        assert!(r2.error.as_ref().unwrap().contains("Failed to resolve"));

        // Third attempt should be rate limited even though file doesn't exist
        let r3 = tool.execute(json!({"path": "nope3.txt"})).await.unwrap();
        assert!(!r3.success);
        assert!(
            r3.error.as_ref().unwrap().contains("Rate limit"),
            "Expected rate limit error, got: {:?}",
            r3.error
        );
    }

    #[tokio::test]
    async fn file_read_rejects_oversized_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        // Create a file just over 10 MB
        let big = vec![b'x'; 10 * 1024 * 1024 + 1];
        tokio::fs::write(dir.join("huge.bin"), &big).await.unwrap();

        let tool = FileReadTool::new(test_security(dir.clone()));
        let result = tool.execute(json!({"path": "huge.bin"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("File too large"));
    }
}
