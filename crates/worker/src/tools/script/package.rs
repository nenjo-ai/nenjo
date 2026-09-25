//! Package-shipped script tools (`nenjo.script_tool.v1`).
//!
//! A `ScriptToolManifest` assigned to an agent becomes a first-class model
//! tool backed by the same QuickJS engine as the interactive `script` tool.
//! The manifest's `command.path` resolves relative to the package's
//! `root_dir`/`root_path` (traversal-guarded, mirroring skill entry paths),
//! and the script receives the model's tool arguments as its `args`
//! parameter. Package scripts get the same dispatch surface the agent
//! already has (`ctx.mcp`, `ctx.runtime`) — they are trusted code shipped
//! through packages, but they gain no privileges the agent lacks.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nenjo::manifest::ScriptToolManifest;
use nenjo_tool_api::{Tool, ToolCategory, ToolOrigin, ToolResult};
use tokio_util::sync::CancellationToken;

use super::engine::{self, LogBuffer, ScriptLimits, ScriptNamespaces};

/// A model-facing tool executing a package-declared script.
pub struct PackageScriptTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
    source: String,
    timeout: std::time::Duration,
    read_only: bool,
    mcp_tools: Vec<Arc<dyn Tool>>,
    runtime_tools: Vec<Arc<dyn Tool>>,
    limits: ScriptLimits,
}

impl PackageScriptTool {
    /// Resolve and load a script tool from its manifest.
    ///
    /// Fails when the manifest lacks a package root, the entry path escapes
    /// the package, or the script file cannot be read.
    ///
    /// The manifest's `command.args` and `command.cwd` fields are ignored:
    /// they exist for a future subprocess backend, while this tool executes
    /// the script directly in the QuickJS engine. Any non-default values are
    /// surfaced as a warning rather than silently dropped.
    pub fn from_manifest(
        manifest: ScriptToolManifest,
        mcp_tools: Vec<Arc<dyn Tool>>,
        runtime_tools: Vec<Arc<dyn Tool>>,
        limits: ScriptLimits,
    ) -> anyhow::Result<Self> {
        let entry = script_entry_path(&manifest)?;
        let source = std::fs::read_to_string(&entry)
            .map_err(|error| anyhow::anyhow!("failed to read script tool `{}`: {error}", manifest.slug))?;
        if source.len() > 512 * 1024 {
            anyhow::bail!("script tool `{}` exceeds 512 KiB size limit", manifest.slug);
        }
        if !manifest.command.args.is_empty() || manifest.command.cwd != "workspace" {
            tracing::warn!(
                slug = %manifest.slug,
                args = ?manifest.command.args,
                cwd = %manifest.command.cwd,
                "Script tool manifest declares command.args/command.cwd, which the \
                 QuickJS backend ignores"
            );
        }
        let description = manifest
            .description
            .clone()
            .unwrap_or_else(|| format!("Package script tool {}.", manifest.slug));
        Ok(Self {
            name: nenjo_tool_api::sanitize_tool_name(&manifest.name),
            description,
            parameters: manifest.parameters.clone(),
            source,
            timeout: manifest
                .timeout_seconds
                .map(std::time::Duration::from_secs)
                .unwrap_or(limits.default_timeout),
            read_only: manifest.read_only,
            mcp_tools,
            runtime_tools,
            limits,
        })
    }

    fn namespaces(&self) -> ScriptNamespaces {
        ScriptNamespaces {
            mcp: self.mcp_tools.clone(),
            runtime: self.runtime_tools.clone(),
            harness: None,
        }
    }
}

/// Resolve the absolute script entry path from a manifest.
///
/// `root_dir` is the package's local install directory; `root_path` and
/// `command.path` are package-relative segments. Absolute paths and `..`
/// components are rejected to keep execution inside the package.
fn script_entry_path(manifest: &ScriptToolManifest) -> anyhow::Result<PathBuf> {
    if manifest.root_dir.as_os_str().is_empty() {
        anyhow::bail!(
            "script tool `{}` does not declare root_dir",
            manifest.slug
        );
    }
    let relative = [manifest.root_path.as_str(), manifest.command.path.as_str()]
        .iter()
        .filter(|segment| !segment.trim().is_empty())
        .try_fold(PathBuf::new(), |base, segment| {
            safe_entry_segment(base, segment)
        })?;
    Ok(manifest.root_dir.join(relative))
}

fn safe_entry_segment(base: PathBuf, segment: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(segment);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        anyhow::bail!("script tool entry path must be relative and must not contain '..'");
    }
    Ok(base.join(path))
}

#[async_trait]
impl Tool for PackageScriptTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    fn category(&self) -> ToolCategory {
        match self.read_only {
            true => ToolCategory::Read,
            false => ToolCategory::ReadWrite,
        }
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Host
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let stop = CancellationToken::new();
        let logs = Arc::new(Mutex::new(LogBuffer::default()));
        let outcome = engine::run_with_input(
            &self.source,
            self.namespaces(),
            &self.limits,
            self.timeout,
            stop,
            logs.clone(),
            Some(args),
        )
        .await?;
        let drained = logs.lock().expect("log mutex poisoned").drain_all();
        Ok(super::outcome_to_tool_result(&outcome, &drained))
    }
}

/// Registry resolving assigned script tool slugs against the cached catalog.
pub fn resolve_script_tools(
    assigned: &[nenjo::Slug],
    catalog: &[ScriptToolManifest],
    mcp_tools: Vec<Arc<dyn Tool>>,
    runtime_tools: Vec<Arc<dyn Tool>>,
    limits: ScriptLimits,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut resolved = Vec::new();
    let mut problems = Vec::new();
    for slug in assigned {
        let Some(manifest) = catalog.iter().find(|manifest| &manifest.slug == slug) else {
            problems.push(format!("script tool `{slug}` assigned but not found in cache"));
            continue;
        };
        match PackageScriptTool::from_manifest(
            manifest.clone(),
            mcp_tools.clone(),
            runtime_tools.clone(),
            limits.clone(),
        ) {
            Ok(tool) => resolved.push(Arc::new(tool) as Arc<dyn Tool>),
            Err(error) => problems.push(error.to_string()),
        }
    }
    (resolved, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    struct FakeTool;

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            "runtime_test__read_file"
        }

        fn description(&self) -> &str {
            "fake"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::success(args.to_string()))
        }
    }

    fn manifest_with(path: &str, root_dir: &Path) -> ScriptToolManifest {
        ScriptToolManifest {
            slug: "pkg_demo-tool".parse().unwrap(),
            name: "demo_tool".into(),
            description: Some("demos package scripts".into()),
            category: "read_write".into(),
            parameters: json!({"type": "object"}),
            command: nenjo::manifest::ScriptToolCommandManifest {
                path: path.into(),
                args: vec![],
                cwd: "workspace".into(),
            },
            root_path: String::new(),
            root_dir: root_dir.to_path_buf(),
            timeout_seconds: Some(5),
            source_type: "package".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        }
    }

    fn write_script(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let file = dir.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, body).unwrap();
        file
    }

    #[tokio::test]
    async fn package_script_receives_args_and_dispatches_runtime_tools() {
        let root = tempfile::tempdir().unwrap();
        write_script(
            root.path(),
            "tools/demo.js",
            "ctx.log('running'); const res = await ctx.runtime.runtime_test__read_file({ path: args.path }); return { echo: args, file: res.content };",
        );
        let manifest = manifest_with("tools/demo.js", root.path());
        let tool = PackageScriptTool::from_manifest(
            manifest,
            vec![],
            vec![Arc::new(FakeTool)],
            ScriptLimits::default(),
        )
        .unwrap();

        assert_eq!(tool.name(), "demo_tool");
        assert_eq!(tool.category(), ToolCategory::ReadWrite);
        let result = tool
            .execute(json!({"path": "a.txt"}))
            .await
            .expect("execute should not error at transport level");
        assert!(result.success, "script failed: {:?}", result.error);
        let envelope: serde_json::Value =
            serde_json::from_str(&result.output.text_content()).unwrap();
        assert_eq!(envelope["result"]["echo"], json!({"path": "a.txt"}));
        assert_eq!(
            envelope["result"]["file"],
            json!(r#"{"path":"a.txt"}"#)
        );
        assert_eq!(envelope["log"], json!(["running"]));
    }

    #[tokio::test]
    async fn read_only_manifest_is_a_read_category_tool() {
        let root = tempfile::tempdir().unwrap();
        write_script(root.path(), "demo.js", "return 1;");
        let mut manifest = manifest_with("demo.js", root.path());
        manifest.read_only = true;
        let tool =
            PackageScriptTool::from_manifest(manifest, vec![], vec![], ScriptLimits::default())
                .unwrap();
        assert_eq!(tool.category(), ToolCategory::Read);
    }

    #[tokio::test]
    async fn traversal_entry_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let manifest = manifest_with("../outside.js", root.path());
        let Err(error) = PackageScriptTool::from_manifest(
            manifest,
            vec![],
            vec![],
            ScriptLimits::default(),
        ) else {
            panic!("traversal path must be rejected")
        };
        assert!(error.to_string().contains("'..'"), "{error}");
    }

    #[tokio::test]
    async fn missing_root_dir_is_rejected() {
        let mut manifest = manifest_with("demo.js", Path::new("/nonexistent-package"));
        manifest.root_dir = PathBuf::new();
        let Err(error) = PackageScriptTool::from_manifest(
            manifest,
            vec![],
            vec![],
            ScriptLimits::default(),
        ) else {
            panic!("missing root_dir must be rejected")
        };
        assert!(error.to_string().contains("root_dir"), "{error}");
    }

    #[tokio::test]
    async fn missing_script_file_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let manifest = manifest_with("nope.js", root.path());
        let Err(error) = PackageScriptTool::from_manifest(
            manifest,
            vec![],
            vec![],
            ScriptLimits::default(),
        ) else {
            panic!("missing script file must be rejected")
        };
        assert!(error.to_string().contains("failed to read"), "{error}");
    }

    #[tokio::test]
    async fn model_facing_name_is_sanitized() {
        let root = tempfile::tempdir().unwrap();
        write_script(root.path(), "demo.js", "return 1;");
        let mut manifest = manifest_with("demo.js", root.path());
        manifest.name = "My Fancy Tool!*".into();
        let tool =
            PackageScriptTool::from_manifest(manifest, vec![], vec![], ScriptLimits::default())
                .unwrap();
        assert!(
            tool.name()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "name must match the provider tool-name pattern: {}",
            tool.name()
        );
    }

    #[tokio::test]
    async fn resolver_skips_unknown_slugs_with_problems() {
        let (tools, problems) = resolve_script_tools(
            &[nenjo::Slug::try_from("pkg_missing").unwrap()],
            &[],
            vec![],
            vec![],
            ScriptLimits::default(),
        );
        assert!(tools.is_empty());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("not found"));
    }

    #[tokio::test]
    async fn manifest_timeout_is_applied() {
        let root = tempfile::tempdir().unwrap();
        write_script(root.path(), "demo.js", "while (true) {}");
        let mut manifest = manifest_with("demo.js", root.path());
        manifest.timeout_seconds = Some(1);
        let tool = PackageScriptTool::from_manifest(
            manifest,
            vec![],
            vec![],
            ScriptLimits {
                default_timeout: Duration::from_secs(60),
                ..ScriptLimits::default()
            },
        )
        .unwrap();
        let result = tool
            .execute(json!({}))
            .await
            .expect("execute should not error at transport level");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("timeout"),
            "unexpected error: {:?}",
            result.error
        );
    }
}
