//! Write file contents with path sandboxing and auto-created parent directories.

use crate::tools::file_mutation::{
    FileMutationCoordinator, MAX_FILE_MUTATION_BYTES, WorkspacePath, atomic_replace_file_at,
    open_workspace_parent,
};
use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// Write file contents with path sandboxing
pub struct FileWriteTool {
    security: Arc<SecurityPolicy>,
    mutations: Arc<FileMutationCoordinator>,
}

impl FileWriteTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self::with_coordinator(security, Arc::new(FileMutationCoordinator::default()))
    }

    pub(crate) fn with_coordinator(
        security: Arc<SecurityPolicy>,
        mutations: Arc<FileMutationCoordinator>,
    ) -> Self {
        Self {
            security,
            mutations,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for FileWriteTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Create or completely replace a file in the scoped workspace, creating parent \
        directories when needed. The supplied content becomes the entire file; use edit \
        for a precise change to an existing file."
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
                    "description": "Exact text to write to the file. Arrays, objects, numbers, booleans, and null are rejected."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let FileWriteArgs { path, content } = serde_json::from_value(args)
            .map_err(|error| anyhow::anyhow!("Invalid write arguments: {error}"))?;

        if content.len() > MAX_FILE_MUTATION_BYTES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Content is too large: {} bytes (limit: {MAX_FILE_MUTATION_BYTES} bytes)",
                    content.len()
                )),
            });
        }

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
        if !self.security.is_path_allowed(&path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        let target = WorkspacePath::parse(&path)
            .map_err(|error| anyhow::anyhow!("Invalid write path: {error}"))?;
        let mutation_key = target.mutation_key(&self.security.workspace_dir);
        let _mutation_guard = self.mutations.lock(&mutation_key).await;
        if self.security.is_managed_runtime_path(&mutation_key) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Path is managed by Nenjo runtime installs and is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let workspace_root = self.security.workspace_dir.clone();
        let content_len = content.len();
        let write_result = tokio::task::spawn_blocking(move || {
            let parent = open_workspace_parent(&workspace_root, &target, true)?;
            atomic_replace_file_at(
                &parent.dir,
                std::path::Path::new(&parent.file_name),
                content.as_bytes(),
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Write task failed: {error}"))?;

        match write_result {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Written {content_len} bytes to {path}"),
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
        assert_eq!(tool.name(), "write");
    }

    #[test]
    fn file_write_schema_has_path_and_content() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["content"].is_object());
        assert_eq!(schema["properties"]["content"]["type"], "string");
        assert_eq!(schema["additionalProperties"], false);
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
    async fn file_write_rejects_non_string_content_without_touching_the_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        let target = dir.join("module.py");
        tokio::fs::write(&target, "original Python source")
            .await
            .unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        for malformed in [
            json!([["fragment", 3000]]),
            json!({"source": "fragment"}),
            json!(42),
            json!(true),
            serde_json::Value::Null,
        ] {
            let error = tool
                .execute(json!({"path": "module.py", "content": malformed}))
                .await
                .expect_err("non-string content must be rejected");
            assert!(error.to_string().contains("Invalid write arguments"));
            assert_eq!(
                tokio::fs::read_to_string(&target).await.unwrap(),
                "original Python source"
            );
        }
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
    async fn file_write_waits_for_the_shared_path_lock() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        let target = dir.join("locked.txt");
        let mutations = Arc::new(FileMutationCoordinator::default());
        let held = mutations.lock(&target).await;
        let tool = FileWriteTool::with_coordinator(test_security(dir), mutations);
        let write = tokio::spawn(async move {
            tool.execute(json!({"path": "locked.txt", "content": "complete"}))
                .await
        });

        tokio::task::yield_now().await;
        assert!(!write.is_finished());
        drop(held);
        assert!(write.await.unwrap().unwrap().success);
        assert_eq!(tokio::fs::read_to_string(target).await.unwrap(), "complete");
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

    #[tokio::test]
    async fn file_write_rejects_oversized_content_without_creating_a_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "too-large.txt",
                "content": "x".repeat(MAX_FILE_MUTATION_BYTES + 1)
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("too large"));
        assert!(!dir.join("too-large.txt").exists());
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
            .execute(json!({"path": "escape_dir/new/hijack.txt", "content": "bad"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(!outside.join("new").exists());
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
