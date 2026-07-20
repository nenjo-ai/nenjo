//! Delete files or directories through a capability-scoped workspace handle.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use cap_std::fs::Dir;
use serde::Deserialize;
use serde_json::json;

use crate::tools::file_mutation::{FileMutationCoordinator, WorkspacePath, open_workspace_parent};
use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};

/// Project-layout paths that agents must not remove.
#[derive(Clone, Debug)]
pub(crate) struct ProtectedProjectPaths {
    workspace_root: PathBuf,
}

impl ProtectedProjectPaths {
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    fn refusal_reason(&self, target: &Path, is_dir: bool) -> Option<&'static str> {
        let workspace_root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        let relative = target.strip_prefix(workspace_root).ok()?;
        let components = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value),
                Component::Prefix(_)
                | Component::RootDir
                | Component::CurDir
                | Component::ParentDir => None,
            })
            .collect::<Vec<_>>();

        if is_dir && components.len() == 1 && components[0] != ".nenjo" {
            return Some("Refusing to delete a Nenjo project directory");
        }
        if components.len() == 2 && components[1] == "repo" {
            return Some("Refusing to delete a project's canonical repo directory");
        }
        None
    }
}

/// Delete a file or directory in the workspace.
pub struct FileDeleteTool {
    security: Arc<SecurityPolicy>,
    mutations: Arc<FileMutationCoordinator>,
    protected_projects: Option<ProtectedProjectPaths>,
}

impl FileDeleteTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self::with_coordinator(security, Arc::new(FileMutationCoordinator::default()), None)
    }

    pub(crate) fn with_coordinator(
        security: Arc<SecurityPolicy>,
        mutations: Arc<FileMutationCoordinator>,
        protected_projects: Option<ProtectedProjectPaths>,
    ) -> Self {
        Self {
            security,
            mutations,
            protected_projects,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDeleteArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

#[async_trait]
impl Tool for FileDeleteTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn name(&self) -> &str {
        "remove"
    }

    fn description(&self) -> &str {
        "Delete a file in the scoped workspace. Directories require recursive=true. \
        Nenjo project directories, canonical repo directories, .git paths, and symlinks \
        are protected."
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
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let FileDeleteArgs { path, recursive } = serde_json::from_value(args)
            .map_err(|error| anyhow::anyhow!("Invalid remove arguments: {error}"))?;

        if !self.security.can_act() {
            return Ok(failure("Action blocked: autonomy is read-only"));
        }
        if self.security.is_rate_limited() {
            return Ok(failure(
                "Rate limit exceeded: too many actions in the last hour",
            ));
        }
        if path.trim().is_empty() || path == "." {
            return Ok(failure("Refusing to delete workspace root"));
        }
        if !self.security.is_path_allowed(&path) {
            return Ok(failure(format!(
                "Path not allowed by security policy: {path}"
            )));
        }

        let target = WorkspacePath::parse(&path)
            .map_err(|error| anyhow::anyhow!("Invalid remove path: {error}"))?;
        if target
            .as_path()
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            return Ok(failure("Refusing to delete .git paths"));
        }

        let mutation_key = target.mutation_key(&self.security.workspace_dir);
        let _mutation_guard = self.mutations.lock(&mutation_key).await;
        if self.security.is_managed_runtime_path(&mutation_key) {
            return Ok(failure(
                "Path is managed by Nenjo runtime installs and is read-only",
            ));
        }

        let workspace_root = self.security.workspace_dir.clone();
        let security = self.security.clone();
        let protected_projects = self.protected_projects.clone();
        let delete_result = tokio::task::spawn_blocking(move || -> anyhow::Result<ToolResult> {
            let parent = open_workspace_parent(&workspace_root, &target, false)?;
            let target_name = Path::new(&parent.file_name);
            let metadata = parent
                .dir
                .symlink_metadata(target_name)
                .context("failed to inspect delete target")?;

            if metadata.is_symlink() {
                return Ok(failure(format!(
                    "Refusing to delete symlink: {}",
                    mutation_key.display()
                )));
            }
            if let Some(reason) = protected_projects
                .as_ref()
                .and_then(|protected| protected.refusal_reason(&mutation_key, metadata.is_dir()))
            {
                return Ok(failure(reason));
            }

            if metadata.is_dir() {
                if !recursive {
                    return Ok(failure(
                        "Refusing to delete directory without recursive=true",
                    ));
                }
                let target_dir = parent
                    .dir
                    .open_dir(target_name)
                    .context("failed to open delete target directory")?;
                if contains_dot_git(&target_dir)? {
                    return Ok(failure(
                        "Refusing to recursively delete a directory containing .git",
                    ));
                }
                if !security.record_action() {
                    return Ok(failure("Rate limit exceeded: action budget exhausted"));
                }
                target_dir
                    .remove_open_dir_all()
                    .context("failed to delete directory")?;
                Ok(success(format!("Deleted directory {path}")))
            } else {
                if !security.record_action() {
                    return Ok(failure("Rate limit exceeded: action budget exhausted"));
                }
                parent
                    .dir
                    .remove_file(target_name)
                    .context("failed to delete file")?;
                Ok(success(format!("Deleted file {path}")))
            }
        })
        .await
        .map_err(|error| anyhow::anyhow!("Remove task failed: {error}"))?;

        match delete_result {
            Ok(result) => Ok(result),
            Err(error) => Ok(failure(format!("Failed to delete path: {error:#}"))),
        }
    }
}

fn contains_dot_git(dir: &Dir) -> anyhow::Result<bool> {
    for entry in dir
        .entries()
        .context("failed to inspect directory contents")?
    {
        let entry = entry.context("failed to inspect directory entry")?;
        if entry.file_name() == ".git" {
            return Ok(true);
        }
        if entry
            .file_type()
            .context("failed to inspect directory entry type")?
            .is_dir()
            && contains_dot_git(&entry.open_dir().context("failed to open child directory")?)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn success(output: String) -> ToolResult {
    ToolResult {
        success: true,
        output,
        error: None,
    }
}

fn failure(error: impl Into<String>) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::security::{AutonomyLevel, SecurityPolicy};

    fn test_security_with(
        workspace: PathBuf,
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

    fn test_security(workspace: PathBuf) -> Arc<SecurityPolicy> {
        test_security_with(workspace, AutonomyLevel::Supervised, 1000)
    }

    fn protected_tool(workspace: &Path) -> FileDeleteTool {
        FileDeleteTool::with_coordinator(
            test_security(workspace.to_path_buf()),
            Arc::new(FileMutationCoordinator::default()),
            Some(ProtectedProjectPaths::new(workspace.to_path_buf())),
        )
    }

    #[test]
    fn file_delete_name_and_schema() {
        let tool = FileDeleteTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "remove");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn file_delete_removes_file_and_recursive_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("delete-me.txt"), "remove").unwrap();
        std::fs::create_dir_all(dir.path().join("subdir/nested")).unwrap();
        std::fs::write(dir.path().join("subdir/nested/file.txt"), "remove").unwrap();
        let tool = FileDeleteTool::new(test_security(dir.path().to_path_buf()));

        assert!(
            tool.execute(json!({"path": "delete-me.txt"}))
                .await
                .unwrap()
                .success
        );
        assert!(
            tool.execute(json!({"path": "subdir", "recursive": true}))
                .await
                .unwrap()
                .success
        );
        assert!(!dir.path().join("subdir").exists());
    }

    #[tokio::test]
    async fn file_delete_requires_recursive_for_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        let result = FileDeleteTool::new(test_security(dir.path().to_path_buf()))
            .execute(json!({"path": "subdir"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("recursive=true"));
    }

    #[tokio::test]
    async fn file_delete_protects_project_root_and_repo() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("demo/repo/src")).unwrap();
        let tool = protected_tool(workspace.path());

        for path in ["demo", "demo/repo"] {
            let result = tool
                .execute(json!({"path": path, "recursive": true}))
                .await
                .unwrap();
            assert!(!result.success, "{path} must be protected");
            assert!(workspace.path().join(path).exists());
        }
    }

    #[tokio::test]
    async fn file_delete_protects_nested_dot_git() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("scratch/nested/.git")).unwrap();
        let tool = FileDeleteTool::new(test_security(workspace.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "scratch", "recursive": true}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(workspace.path().join("scratch/nested/.git").exists());
    }

    #[tokio::test]
    async fn file_delete_blocks_workspace_root_readonly_and_rate_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root_result = FileDeleteTool::new(test_security(dir.path().to_path_buf()))
            .execute(json!({"path": "."}))
            .await
            .unwrap();
        assert!(!root_result.success);

        for (autonomy, limit) in [
            (AutonomyLevel::ReadOnly, 1000),
            (AutonomyLevel::Supervised, 0),
        ] {
            let path = dir.path().join(format!("{autonomy:?}.txt"));
            std::fs::write(&path, "keep").unwrap();
            let result = FileDeleteTool::new(test_security_with(
                dir.path().to_path_buf(),
                autonomy,
                limit,
            ))
            .execute(json!({"path": path.file_name().unwrap().to_string_lossy()}))
            .await
            .unwrap();
            assert!(!result.success);
            assert!(path.exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_delete_refuses_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let result = FileDeleteTool::new(test_security(dir.path().to_path_buf()))
            .execute(json!({"path": "link.txt"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(target.exists());
        assert!(link.exists());
    }

    #[tokio::test]
    async fn file_delete_waits_for_descendant_mutation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tree/nested")).unwrap();
        let descendant = dir.path().join("tree/nested/file.txt");
        std::fs::write(&descendant, "keep").unwrap();
        let mutations = Arc::new(FileMutationCoordinator::default());
        let held = mutations.lock(&descendant).await;
        let tool = FileDeleteTool::with_coordinator(
            test_security(dir.path().to_path_buf()),
            mutations,
            None,
        );
        let delete = tokio::spawn(async move {
            tool.execute(json!({"path": "tree", "recursive": true}))
                .await
        });

        tokio::task::yield_now().await;
        assert!(!delete.is_finished());
        drop(held);
        assert!(delete.await.unwrap().unwrap().success);
    }
}
