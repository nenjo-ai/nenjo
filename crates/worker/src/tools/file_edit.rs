//! Precise find-and-replace file editing with single-match enforcement.

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

/// Edit a file by replacing an exact string match with new content.
///
/// Uses `old_string` → `new_string` precise replacement within the workspace.
/// The `old_string` must appear exactly once in the file (zero matches = not
/// found, multiple matches = ambiguous). `new_string` may be empty to delete
/// the matched text. Security checks mirror [`super::file_write::FileWriteTool`].
pub struct FileEditTool {
    security: Arc<SecurityPolicy>,
    mutations: Arc<FileMutationCoordinator>,
}

impl FileEditTool {
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
struct FileEditArgs {
    path: String,
    old_string: String,
    new_string: String,
}

enum EditOutcome {
    Edited(usize),
    MissingMatch,
    AmbiguousMatch(usize),
    SourceTooLarge(u64),
    ResultTooLarge(usize),
}

#[async_trait]
impl Tool for FileEditTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file in the scoped workspace by replacing one exact, unique text match. The edit fails if the old text is missing or appears more than once."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Relative paths resolve from workspace; outside paths require policy allowlist."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace (must appear exactly once in the file)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text (empty string to delete the matched text)"
                }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // ── 1. Parse parameters ────────────────────────────────────
        let FileEditArgs {
            path,
            old_string,
            new_string,
        } = serde_json::from_value(args)
            .map_err(|error| anyhow::anyhow!("Invalid edit arguments: {error}"))?;

        if old_string.len() > MAX_FILE_MUTATION_BYTES || new_string.len() > MAX_FILE_MUTATION_BYTES
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Edit text is too large (limit: {MAX_FILE_MUTATION_BYTES} bytes per value)"
                )),
            });
        }

        if old_string.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("old_string must not be empty".into()),
            });
        }

        // ── 2. Autonomy check ──────────────────────────────────────
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // ── 3. Rate limit check ────────────────────────────────────
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // ── 4. Path pre-validation ─────────────────────────────────
        if !self.security.is_path_allowed(&path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        let target = WorkspacePath::parse(&path)
            .map_err(|error| anyhow::anyhow!("Invalid edit path: {error}"))?;
        let mutation_key = target.mutation_key(&self.security.workspace_dir);
        let _mutation_guard = self.mutations.lock(&mutation_key).await;
        if self.security.is_managed_runtime_path(&mutation_key) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Path is managed by Nenjo runtime installs and is read-only".into()),
            });
        }

        // ── 8. Record action ───────────────────────────────────────
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // ── 9. Read → match → replace → write ─────────────────────
        let workspace_root = self.security.workspace_dir.clone();
        let edit_result = tokio::task::spawn_blocking(move || -> anyhow::Result<EditOutcome> {
            let parent = open_workspace_parent(&workspace_root, &target, false)?;
            let target_name = std::path::Path::new(&parent.file_name);
            let metadata = parent
                .dir
                .symlink_metadata(target_name)
                .map_err(anyhow::Error::from)?;
            if metadata.is_symlink() {
                anyhow::bail!("refusing to edit through a symlink");
            }
            if metadata.len() > MAX_FILE_MUTATION_BYTES as u64 {
                return Ok(EditOutcome::SourceTooLarge(metadata.len()));
            }

            let content = parent.dir.read_to_string(target_name)?;
            let match_count = content.matches(&old_string).count();
            if match_count == 0 {
                return Ok(EditOutcome::MissingMatch);
            }
            if match_count > 1 {
                return Ok(EditOutcome::AmbiguousMatch(match_count));
            }

            let new_content = content.replacen(&old_string, &new_string, 1);
            if new_content.len() > MAX_FILE_MUTATION_BYTES {
                return Ok(EditOutcome::ResultTooLarge(new_content.len()));
            }
            atomic_replace_file_at(&parent.dir, target_name, new_content.as_bytes())?;
            Ok(EditOutcome::Edited(new_content.len()))
        })
        .await
        .map_err(|error| anyhow::anyhow!("Edit task failed: {error}"))?;

        match edit_result {
            Ok(EditOutcome::Edited(new_len)) => Ok(ToolResult {
                success: true,
                output: format!("Edited {path}: replaced 1 occurrence ({} bytes)", new_len),
                error: None,
            }),
            Ok(EditOutcome::MissingMatch) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("old_string not found in file".into()),
            }),
            Ok(EditOutcome::AmbiguousMatch(count)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "old_string matches {count} times; must match exactly once"
                )),
            }),
            Ok(EditOutcome::SourceTooLarge(bytes)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "File is too large: {bytes} bytes (limit: {MAX_FILE_MUTATION_BYTES} bytes)"
                )),
            }),
            Ok(EditOutcome::ResultTooLarge(bytes)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Edited file would be too large: {bytes} bytes (limit: {MAX_FILE_MUTATION_BYTES} bytes)"
                )),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to read file or apply edit: {e}")),
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
        tempfile::tempdir().expect("create temporary workspace")
    }

    #[test]
    fn file_edit_name() {
        let temp = temp_workspace();
        let tool = FileEditTool::new(test_security(temp.path().to_path_buf()));
        assert_eq!(tool.name(), "edit");
    }

    #[test]
    fn file_edit_schema_has_required_params() {
        let temp = temp_workspace();
        let tool = FileEditTool::new(test_security(temp.path().to_path_buf()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["old_string"].is_object());
        assert!(schema["properties"]["new_string"].is_object());
        assert_eq!(schema["properties"]["old_string"]["type"], "string");
        assert_eq!(schema["properties"]["new_string"]["type"], "string");
        assert_eq!(schema["additionalProperties"], false);
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("old_string")));
        assert!(required.contains(&json!("new_string")));
    }

    #[tokio::test]
    async fn file_edit_replaces_single_match() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "hello",
                "new_string": "goodbye"
            }))
            .await
            .unwrap();

        assert!(result.success, "edit should succeed: {:?}", result.error);
        assert!(result.output.contains("replaced 1 occurrence"));

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "goodbye world");
    }

    #[tokio::test]
    async fn file_edit_waits_for_the_shared_path_lock() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        let target = dir.join("locked.txt");
        tokio::fs::write(&target, "before target after")
            .await
            .unwrap();
        let mutations = Arc::new(FileMutationCoordinator::default());
        let held = mutations.lock(&target).await;
        let tool = FileEditTool::with_coordinator(test_security(dir), mutations);
        let edit = tokio::spawn(async move {
            tool.execute(json!({
                "path": "locked.txt",
                "old_string": "target",
                "new_string": "replacement"
            }))
            .await
        });

        tokio::task::yield_now().await;
        assert!(!edit.is_finished());
        drop(held);
        assert!(edit.await.unwrap().unwrap().success);
        assert_eq!(
            tokio::fs::read_to_string(target).await.unwrap(),
            "before replacement after"
        );
    }

    #[tokio::test]
    async fn file_edit_not_found() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "nonexistent",
                "new_string": "replacement"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("not found"));

        // File should be unchanged
        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn file_edit_rejects_oversized_input_without_touching_the_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        let target = dir.join("test.txt");
        tokio::fs::write(&target, "original").await.unwrap();
        let tool = FileEditTool::new(test_security(dir));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "original",
                "new_string": "x".repeat(MAX_FILE_MUTATION_BYTES + 1)
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("too large"));
        assert_eq!(tokio::fs::read_to_string(target).await.unwrap(), "original");
    }

    #[tokio::test]
    async fn file_edit_rejects_oversized_source_without_loading_it() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        let target = dir.join("test.txt");
        tokio::fs::write(&target, "x".repeat(MAX_FILE_MUTATION_BYTES + 1))
            .await
            .unwrap();
        let tool = FileEditTool::new(test_security(dir));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "x",
                "new_string": "y"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("File is too large"));
        assert_eq!(
            tokio::fs::metadata(target).await.unwrap().len(),
            (MAX_FILE_MUTATION_BYTES + 1) as u64
        );
    }

    #[tokio::test]
    async fn file_edit_multiple_matches() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "aaa bbb aaa")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "aaa",
                "new_string": "ccc"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("matches 2 times")
        );

        // File should be unchanged
        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "aaa bbb aaa");
    }

    #[tokio::test]
    async fn file_edit_delete_via_empty_new_string() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "keep remove keep")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": " remove",
                "new_string": ""
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "delete edit should succeed: {:?}",
            result.error
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "keep keep");
    }

    #[tokio::test]
    async fn file_edit_missing_path_param() {
        let temp = temp_workspace();
        let tool = FileEditTool::new(test_security(temp.path().to_path_buf()));
        let result = tool
            .execute(json!({"old_string": "a", "new_string": "b"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_missing_old_string_param() {
        let temp = temp_workspace();
        let tool = FileEditTool::new(test_security(temp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "f.txt", "new_string": "b"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_missing_new_string_param() {
        let temp = temp_workspace();
        let tool = FileEditTool::new(test_security(temp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "f.txt", "old_string": "a"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_rejects_non_string_replacements_without_touching_the_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        let target = dir.join("module.py");
        tokio::fs::write(&target, "before target after")
            .await
            .unwrap();
        let tool = FileEditTool::new(test_security(dir));

        let malformed_calls = [
            json!({
                "path": "module.py",
                "old_string": ["target"],
                "new_string": "replacement"
            }),
            json!({
                "path": "module.py",
                "old_string": "target",
                "new_string": ["replacement"]
            }),
        ];
        for args in malformed_calls {
            let error = tool
                .execute(args)
                .await
                .expect_err("non-string edit content must be rejected");
            assert!(error.to_string().contains("Invalid edit arguments"));
            assert_eq!(
                tokio::fs::read_to_string(&target).await.unwrap(),
                "before target after"
            );
        }
    }

    #[tokio::test]
    async fn file_edit_rejects_empty_old_string() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "hello")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "",
                "new_string": "x"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("must not be empty")
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn file_edit_blocks_path_traversal() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "../../etc/passwd",
                "old_string": "root",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn file_edit_blocks_absolute_path() {
        let temp = temp_workspace();
        let tool = FileEditTool::new(test_security(temp.path().to_path_buf()));
        let result = tool
            .execute(json!({
                "path": "/etc/passwd",
                "old_string": "root",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_edit_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = temp_workspace();
        let root = temp.path().to_path_buf();
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        symlink(&outside, workspace.join("escape_dir")).unwrap();

        let tool = FileEditTool::new(test_security(workspace.clone()));
        let result = tool
            .execute(json!({
                "path": "escape_dir/target.txt",
                "old_string": "a",
                "new_string": "b"
            }))
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
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_edit_blocks_symlink_target_file() {
        use std::os::unix::fs::symlink;

        let temp = temp_workspace();
        let root = temp.path().to_path_buf();
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        tokio::fs::write(outside.join("target.txt"), "original")
            .await
            .unwrap();
        symlink(outside.join("target.txt"), workspace.join("linked.txt")).unwrap();

        let tool = FileEditTool::new(test_security(workspace.clone()));
        let result = tool
            .execute(json!({
                "path": "linked.txt",
                "old_string": "original",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success, "editing through symlink must be blocked");
        assert!(
            result.error.as_deref().unwrap_or("").contains("symlink"),
            "error should mention symlink"
        );

        let content = tokio::fs::read_to_string(outside.join("target.txt"))
            .await
            .unwrap();
        assert_eq!(content, "original", "original file must not be modified");
    }

    #[tokio::test]
    async fn file_edit_blocks_readonly_mode() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "hello")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security_with(dir.clone(), AutonomyLevel::ReadOnly, 20));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "hello",
                "new_string": "world"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("read-only"));

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn file_edit_blocks_when_rate_limited() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "hello")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security_with(
            dir.clone(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "hello",
                "new_string": "world"
            }))
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

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn file_edit_nonexistent_file() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "missing.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to read file")
        );
    }

    #[tokio::test]
    async fn file_edit_blocks_null_byte_in_path() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test\0evil.txt",
                "old_string": "old",
                "new_string": "new"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }
}
