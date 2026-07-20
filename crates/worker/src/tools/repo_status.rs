//! Structured, read-only repository status for model-facing inspection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::json;

use super::security::SecurityPolicy;
use super::{Tool, ToolCategory, ToolResult};

pub struct RepoStatusTool {
    security: Arc<SecurityPolicy>,
}

impl RepoStatusTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }

    fn is_repository(&self) -> bool {
        self.security.workspace_dir.join(".git").exists()
    }

    async fn git_raw(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut command = tokio::process::Command::new("git");
        command
            .args(args)
            .current_dir(&self.security.workspace_dir)
            .env_clear();
        for name in ["PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE"] {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        for (name, value) in &self.security.forwarded_env {
            if name.starts_with("GIT_CONFIG_") {
                command.env(name, value);
            }
        }
        let output = command
            .output()
            .await
            .with_context(|| format!("failed to run git {}", args.join(" ")))?;
        if !output.status.success() {
            anyhow::bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        Ok(self.git_raw(args).await?.trim().to_owned())
    }

    async fn optional_git(&self, args: &[&str]) -> Option<String> {
        self.git(args).await.ok().filter(|value| !value.is_empty())
    }

    async fn snapshot(&self) -> anyhow::Result<RepoStatus> {
        let root = PathBuf::from(self.git(&["rev-parse", "--show-toplevel"]).await?);
        ensure_repo_root_is_scoped(&root, &self.security.workspace_dir)?;
        let branch = self.git(&["branch", "--show-current"]).await?;
        let head = self
            .optional_git(&["rev-parse", "--short=12", "HEAD"])
            .await;
        let upstream = self
            .optional_git(&[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ])
            .await;
        let (ahead, behind) = match upstream.as_deref() {
            Some(_) => self
                .optional_git(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
                .await
                .and_then(|counts| parse_ahead_behind(&counts))
                .unwrap_or_default(),
            None => (0, 0),
        };
        let changes = parse_porcelain_status(
            &self
                .git_raw(&["status", "--porcelain=v1", "--untracked-files=all"])
                .await?,
        );
        let unstaged = self.git(&["diff", "--stat", "--", "."]).await?;
        let staged = self.git(&["diff", "--cached", "--stat", "--", "."]).await?;

        Ok(RepoStatus {
            root: root.to_string_lossy().into_owned(),
            branch: if branch.is_empty() {
                "(detached)".into()
            } else {
                branch
            },
            head,
            upstream,
            ahead,
            behind,
            staged: changes.staged,
            modified: changes.modified,
            untracked: changes.untracked,
            conflicts: changes.conflicts,
            diff_stat: RepoDiffStat { staged, unstaged },
        })
    }
}

#[async_trait]
impl Tool for RepoStatusTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn name(&self) -> &str {
        "repo_status"
    }

    fn description(&self) -> &str {
        "Return structured Git repository state for the scoped working directory, including branch, HEAD, upstream divergence, changed paths, conflicts, and diff statistics. Use shell for Git mutations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn is_available_to_model(&self) -> bool {
        self.is_repository()
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.is_repository() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "The scoped working directory is not a Git repository: {}",
                    self.security.workspace_dir.display()
                )),
            });
        }
        if self.security.is_rate_limited() || !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }
        match self.snapshot().await {
            Ok(status) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&status)?,
                error: None,
            }),
            Err(error) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            }),
        }
    }
}

fn ensure_repo_root_is_scoped(root: &Path, working_directory: &Path) -> anyhow::Result<()> {
    let root = root
        .canonicalize()
        .context("failed to resolve Git repository root")?;
    let working_directory = working_directory
        .canonicalize()
        .context("failed to resolve scoped working directory")?;
    if root != working_directory {
        anyhow::bail!(
            "Git repository root '{}' differs from scoped working directory '{}'",
            root.display(),
            working_directory.display()
        );
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ParsedChanges {
    staged: Vec<String>,
    modified: Vec<String>,
    untracked: Vec<String>,
    conflicts: Vec<String>,
}

fn parse_porcelain_status(status: &str) -> ParsedChanges {
    let mut changes = ParsedChanges::default();
    for line in status.lines().filter(|line| line.len() >= 3) {
        let bytes = line.as_bytes();
        let index = bytes[0] as char;
        let worktree = bytes[1] as char;
        let path = line[3..]
            .rsplit_once(" -> ")
            .map_or(&line[3..], |(_, target)| target)
            .to_owned();
        if index == '?' && worktree == '?' {
            changes.untracked.push(path);
        } else if is_conflict_status(index, worktree) {
            changes.conflicts.push(path);
        } else {
            if index != ' ' {
                changes.staged.push(path.clone());
            }
            if worktree != ' ' {
                changes.modified.push(path);
            }
        }
    }
    changes
}

fn is_conflict_status(index: char, worktree: char) -> bool {
    matches!(
        (index, worktree),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    )
}

fn parse_ahead_behind(counts: &str) -> Option<(u64, u64)> {
    let mut values = counts.split_whitespace();
    let ahead = values.next()?.parse().ok()?;
    let behind = values.next()?.parse().ok()?;
    Some((ahead, behind))
}

#[derive(Debug, Serialize)]
struct RepoStatus {
    root: String,
    branch: String,
    head: Option<String>,
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    staged: Vec<String>,
    modified: Vec<String>,
    untracked: Vec<String>,
    conflicts: Vec<String>,
    diff_stat: RepoDiffStat,
}

#[derive(Debug, Serialize)]
struct RepoDiffStat {
    staged: String,
    unstaged: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::security::AutonomyLevel;

    fn test_tool(workspace: &Path) -> RepoStatusTool {
        RepoStatusTool::new(Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.to_path_buf(),
            ..SecurityPolicy::default()
        }))
    }

    async fn git(workspace: &Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(workspace)
            .output()
            .await
            .unwrap();
        assert!(
            status.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&status.stderr)
        );
    }

    #[test]
    fn porcelain_status_is_grouped_by_state() {
        let changes = parse_porcelain_status(
            " M modified.rs\nA  staged.rs\n?? new.rs\nUU conflict.rs\nR  old.rs -> renamed.rs",
        );

        assert_eq!(changes.modified, ["modified.rs"]);
        assert_eq!(changes.staged, ["staged.rs", "renamed.rs"]);
        assert_eq!(changes.untracked, ["new.rs"]);
        assert_eq!(changes.conflicts, ["conflict.rs"]);
    }

    #[tokio::test]
    async fn repo_status_is_hidden_outside_repository() {
        let workspace = tempfile::tempdir().unwrap();
        let tool = test_tool(workspace.path());

        assert!(!tool.is_available_to_model().await);
        assert!(!tool.execute(json!({})).await.unwrap().success);
    }

    #[tokio::test]
    async fn repo_status_returns_structured_snapshot() {
        let workspace = tempfile::tempdir().unwrap();
        git(workspace.path(), &["init"]).await;
        git(workspace.path(), &["config", "user.name", "Nenjo Test"]).await;
        git(
            workspace.path(),
            &["config", "user.email", "nenjo@example.test"],
        )
        .await;
        tokio::fs::write(workspace.path().join("tracked.txt"), "initial\n")
            .await
            .unwrap();
        git(workspace.path(), &["add", "tracked.txt"]).await;
        git(workspace.path(), &["commit", "-m", "initial"]).await;
        tokio::fs::write(workspace.path().join("tracked.txt"), "changed\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.path().join("new.txt"), "new\n")
            .await
            .unwrap();
        let tool = test_tool(workspace.path());

        assert!(tool.is_available_to_model().await);
        let result = tool.execute(json!({})).await.unwrap();
        let status: serde_json::Value = serde_json::from_str(&result.output).unwrap();

        assert!(result.success, "{:?}", result.error);
        assert_eq!(status["modified"], json!(["tracked.txt"]));
        assert_eq!(status["untracked"], json!(["new.txt"]));
        assert_eq!(status["ahead"], 0);
        assert_eq!(status["behind"], 0);
    }

    #[tokio::test]
    async fn repo_status_supports_a_repository_without_commits() {
        let workspace = tempfile::tempdir().unwrap();
        git(workspace.path(), &["init"]).await;
        tokio::fs::write(workspace.path().join("new.txt"), "new\n")
            .await
            .unwrap();
        let tool = test_tool(workspace.path());

        let result = tool.execute(json!({})).await.unwrap();
        let status: serde_json::Value = serde_json::from_str(&result.output).unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(status["head"].is_null());
        assert_eq!(status["untracked"], json!(["new.txt"]));
    }
}
