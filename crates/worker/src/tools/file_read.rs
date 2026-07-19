//! Read file contents with path sandboxing and symlink-escape protection.

use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_LINE_COUNT: usize = 500;
const MAX_LINE_COUNT: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

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
        "read"
    }

    fn description(&self) -> &str {
        "Read a bounded range of lines from a file in the scoped workspace. Reads start at line 1 and return at most 500 lines by default; use start_line and line_count to continue through large files."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file within the workspace"
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "First line to return, using one-based line numbers",
                    "default": 1
                },
                "line_count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 2000,
                    "description": "Maximum number of lines to return",
                    "default": 500
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
        let start_line = parse_positive_usize(&args, "start_line", 1)?;
        let line_count =
            parse_positive_usize(&args, "line_count", DEFAULT_LINE_COUNT)?.min(MAX_LINE_COUNT);

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
            Ok(contents) => match render_line_range(&contents, start_line, line_count) {
                Ok(output) => Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                }),
                Err(error) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error),
                }),
            },
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to read file: {e}")),
            }),
        }
    }
}

fn parse_positive_usize(
    args: &serde_json::Value,
    field: &str,
    default: usize,
) -> anyhow::Result<usize> {
    let Some(value) = args.get(field) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("'{field}' must be a positive integer"))?;
    let value = usize::try_from(value)
        .map_err(|_| anyhow::anyhow!("'{field}' is too large for this platform"))?;
    if value == 0 {
        anyhow::bail!("'{field}' must be a positive integer");
    }
    Ok(value)
}

fn render_line_range(
    contents: &str,
    start_line: usize,
    line_count: usize,
) -> Result<String, String> {
    let total_lines = contents.lines().count();
    if !contents.is_empty() && start_line > total_lines {
        return Err(format!(
            "start_line {start_line} is past the end of the file ({total_lines} lines)"
        ));
    }

    let mut output: String = contents
        .split_inclusive('\n')
        .skip(start_line - 1)
        .take(line_count)
        .collect();
    let last_selected_line = start_line
        .saturating_add(line_count)
        .saturating_sub(1)
        .min(total_lines);
    let has_more_lines = last_selected_line < total_lines;
    let mut byte_truncated = false;
    if output.len() > MAX_OUTPUT_BYTES {
        output.truncate(output.floor_char_boundary(MAX_OUTPUT_BYTES));
        byte_truncated = true;
    }
    if has_more_lines || byte_truncated {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if byte_truncated {
            output.push_str(&format!(
                "... [output bounded at {MAX_OUTPUT_BYTES} bytes; request a narrower line range or use search]"
            ));
        } else {
            output.push_str(&format!(
                "... [output bounded; file has {total_lines} lines. Continue with start_line={}]",
                last_selected_line + 1
            ));
        }
    }
    Ok(output)
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
    fn file_read_name() {
        let tool = FileReadTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "read");
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
        assert_eq!(schema["properties"]["start_line"]["minimum"], 1);
        assert_eq!(schema["properties"]["line_count"]["maximum"], 2000);
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
    async fn file_read_returns_requested_line_range() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "one\ntwo\nthree\nfour\n")
            .await
            .unwrap();

        let tool = FileReadTool::new(test_security(dir));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "start_line": 2,
                "line_count": 2
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(
            result.output,
            "two\nthree\n... [output bounded; file has 4 lines. Continue with start_line=4]"
        );
    }

    #[tokio::test]
    async fn file_read_rejects_start_line_past_end() {
        let temp = temp_workspace();
        let dir = temp.path().to_path_buf();
        tokio::fs::write(dir.join("test.txt"), "one\ntwo\n")
            .await
            .unwrap();

        let tool = FileReadTool::new(test_security(dir));
        let result = tool
            .execute(json!({"path": "test.txt", "start_line": 3}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("past the end"));
    }

    #[test]
    fn byte_bounded_read_does_not_offer_a_repeating_line_cursor() {
        let contents = "x".repeat(MAX_OUTPUT_BYTES + 1);
        let output = render_line_range(&contents, 1, 1).unwrap();

        assert!(output.contains("output bounded at 262144 bytes"));
        assert!(output.contains("use search"));
        assert!(!output.contains("Continue with start_line=1"));
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
