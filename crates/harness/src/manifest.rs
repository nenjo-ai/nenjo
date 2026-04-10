//! Worker bootstrap — fetch and cache user data on startup.
//!
//! Calls `GET /api/v1/agents/bootstrap` and writes the response as individual
//! JSON files under `~/.nenjo/manifests/`. If the backend is unreachable the worker
//! continues with a warning; filesystem failures are hard errors.
//!
//! Abilities and context blocks are stored as directory trees:
//!   `manifests/abilities/{path}/{name}.json`
//!   `manifests/context_blocks/{path}/{name}.json`
//! Other resource types remain as flat JSON arrays.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

use crate::doc_sync;
use nenjo::client::NenjoClient;
use nenjo::manifest::LambdaManifest;
use nenjo::manifest::{ContextBlockManifest, Manifest, ManifestLoader};
use std::path::PathBuf;
use uuid::Uuid;

static TREE_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Trait for manifest items that can be stored as tree files.
pub trait TreeItem: serde::Serialize {
    fn path(&self) -> &str;
    fn name(&self) -> &str;
}

impl TreeItem for nenjo::manifest::AbilityManifest {
    fn path(&self) -> &str {
        &self.path
    }
    fn name(&self) -> &str {
        &self.name
    }
}

impl TreeItem for nenjo::manifest::DomainManifest {
    fn path(&self) -> &str {
        &self.path
    }
    fn name(&self) -> &str {
        &self.name
    }
}

impl TreeItem for ContextBlockManifest {
    fn path(&self) -> &str {
        &self.path
    }
    fn name(&self) -> &str {
        &self.name
    }
}

/// File-backed template source that reads context block templates from the
/// tree structure on disk: `{base_dir}/{path}/{name}.json`.
///
/// Used by [`ContextRenderer`] for lazy template loading — templates are read
/// from disk only when rendering is requested, rather than held in memory.
pub struct FileTemplateSource {
    base_dir: PathBuf,
}

impl FileTemplateSource {
    /// Create a new file template source rooted at the given directory.
    ///
    /// `base_dir` is typically `{manifests_dir}/context_blocks/`.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }
}

impl nenjo::context::TemplateSource for FileTemplateSource {
    fn load_template(&self, path: &str, name: &str) -> Option<String> {
        let file_path = tree_item_path(&self.base_dir, path, name);
        let content = std::fs::read_to_string(&file_path).ok()?;
        // Parse the JSON and extract the "template" field.
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
        value
            .get("template")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
}

/// Loads context blocks from a local `.nenjo/context/` directory.
///
/// Convention: every `.md` file in `.nenjo/context/` becomes a context block.
/// - **name** = filename without extension (e.g. `coding_standards`)
/// - **path** = `"local"`
/// - **template** = file contents
pub struct LocalManifestLoader {
    root: PathBuf,
}

impl LocalManifestLoader {
    /// Create a loader that scans `root/.nenjo/context/*.md`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait::async_trait]
impl ManifestLoader for LocalManifestLoader {
    async fn load(&self) -> Result<Manifest> {
        let context_dir = self.root.join(".nenjo").join("context");
        let mut blocks = Vec::new();

        if context_dir.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&context_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                .collect();
            entries.sort_by_key(|e| e.file_name());

            for entry in entries {
                let path = entry.path();
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let template = std::fs::read_to_string(&path)?;
                blocks.push(ContextBlockManifest {
                    id: Uuid::new_v4(),
                    name,
                    path: "local".to_string(),
                    display_name: None,
                    description: None,
                    template,
                    is_system: false,
                });
            }
        }

        Ok(Manifest {
            context_blocks: blocks,
            ..Default::default()
        })
    }
}

/// Fetch manifest data from the backend and cache it locally.
///
/// Creates `data` if it doesn't exist, calls the manifest endpoint, and
/// writes `projects.json`, `routines.json`, `agents.json`, etc.
///
/// On network / API errors the function logs a warning and returns `Ok(())`
/// so the worker can still start. Filesystem errors are propagated.
pub async fn sync(api: &NenjoClient, manifests_dir: &Path, workspace_dir: &Path) -> Result<()> {
    // Ensure the data directory exists (filesystem error = hard fail)
    std::fs::create_dir_all(manifests_dir).with_context(|| {
        format!(
            "Failed to create manifests directory: {}",
            manifests_dir.display()
        )
    })?;

    // Fetch bootstrap data — soft-fail on network/API errors
    let data = match api.fetch_manifest().await {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "Bootstrap fetch failed — worker will continue without cached data");
            return Ok(());
        }
    };

    info!(
        projects = data.projects.len(),
        routines = data.routines.len(),
        models = data.models.len(),
        agents = data.agents.len(),
        councils = data.councils.len(),
        domains = data.domains.len(),
        lambdas = data.lambdas.len(),
        mcp_servers = data.mcp_servers.len(),
        "Manifest fetched successfully"
    );

    // Write auth info (user_id + api_key_id) as a single file.
    debug!(user_id = %data.user_id, api_key_id = ?data.api_key_id, "Writing auth.json");
    atomic_write_json(
        manifests_dir,
        "auth.json",
        &serde_json::json!({
            "user_id": data.user_id,
            "api_key_id": data.api_key_id,
        }),
    )?;
    atomic_write_json(manifests_dir, "projects.json", &data.projects)?;
    atomic_write_json(manifests_dir, "routines.json", &data.routines)?;
    atomic_write_json(manifests_dir, "models.json", &data.models)?;
    atomic_write_json(manifests_dir, "agents.json", &data.agents)?;
    atomic_write_json(manifests_dir, "councils.json", &data.councils)?;
    sync_tree(&manifests_dir.join("domains"), &data.domains)?;
    atomic_write_json(manifests_dir, "lambdas.json", &data.lambdas)?;
    atomic_write_json(manifests_dir, "mcp_servers.json", &data.mcp_servers)?;
    sync_tree(&manifests_dir.join("abilities"), &data.abilities)?;
    sync_tree(&manifests_dir.join("context_blocks"), &data.context_blocks)?;

    // Sync lambda script files to workspace
    sync_lambdas(workspace_dir, &data.lambdas)?;

    // Sync project documents to workspace
    doc_sync::sync_all(api, workspace_dir, &data.projects).await?;

    Ok(())
}

/// Write lambda script bodies to `{workspace_dir}/../lambdas/{path}` and set
/// the executable bit so scripts with shebangs can be run directly.
pub fn sync_lambdas(workspace_dir: &Path, lambdas: &[LambdaManifest]) -> Result<()> {
    let parent = workspace_dir.parent().with_context(|| {
        format!(
            "Workspace directory `{}` has no parent; cannot determine sibling `lambdas` directory",
            workspace_dir.display()
        )
    })?;
    let lambdas_dir = parent.join("lambdas");
    std::fs::create_dir_all(&lambdas_dir).with_context(|| {
        format!(
            "Failed to create lambdas directory: {}",
            lambdas_dir.display()
        )
    })?;

    for lambda in lambdas {
        let script_path = lambdas_dir.join(&lambda.path);

        // Ensure parent directories exist for nested paths
        if let Some(parent) = script_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create lambda parent dir: {}", parent.display())
            })?;
        }

        std::fs::write(&script_path, &lambda.body)
            .with_context(|| format!("Failed to write lambda script: {}", script_path.display()))?;

        // Set executable bit (unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&script_path, perms).with_context(|| {
                format!("Failed to set executable bit: {}", script_path.display())
            })?;
        }
    }

    debug!(count = lambdas.len(), dir = %lambdas_dir.display(), "Lambda scripts synced");
    Ok(())
}

/// Sync a list of tree items to a directory tree on disk.
///
/// Each item is written to `{base_dir}/{path}/{name}.json`. Stale files that
/// are not in the expected set are removed, and empty directories are cleaned up.
pub fn sync_tree<T: TreeItem>(base_dir: &Path, items: &[T]) -> Result<()> {
    std::fs::create_dir_all(base_dir)
        .with_context(|| format!("Failed to create tree dir: {}", base_dir.display()))?;

    // Build the set of expected file paths.
    let mut expected: HashSet<PathBuf> = HashSet::new();
    for item in items {
        let file_path = tree_item_path(base_dir, item.path(), item.name());
        expected.insert(file_path);
    }

    // Remove stale files.
    if base_dir.is_dir() {
        remove_stale_files(base_dir, &expected)?;
    }

    // Write each item.
    for item in items {
        let file_path = tree_item_path(base_dir, item.path(), item.name());
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create dir: {}", parent.display()))?;
        }
        let tmp = unique_tree_tmp_path(&file_path);
        let json = serde_json::to_string_pretty(item)
            .with_context(|| format!("Failed to serialize tree item: {}", file_path.display()))?;
        std::fs::write(&tmp, json.as_bytes())
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &file_path).with_context(|| {
            format!(
                "Failed to rename {} → {}",
                tmp.display(),
                file_path.display()
            )
        })?;
    }

    debug!(dir = %base_dir.display(), count = items.len(), "Tree synced");
    Ok(())
}

/// Compute the file path for a tree item: `{base_dir}/{path}/{name}.json`
pub fn tree_item_path(base_dir: &Path, path: &str, name: &str) -> PathBuf {
    if path.is_empty() {
        base_dir.join(format!("{name}.json"))
    } else {
        base_dir.join(path).join(format!("{name}.json"))
    }
}

fn unique_tree_tmp_path(file_path: &Path) -> PathBuf {
    let nonce = TREE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let filename = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("resource.json");
    file_path.with_file_name(format!(".{filename}.{pid}.{nonce}.tmp"))
}

/// Recursively remove files in `dir` that are not in the expected set, and clean up
/// empty directories.
fn remove_stale_files(dir: &Path, expected: &HashSet<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            remove_stale_files(&path, expected)?;
            // Remove directory if now empty.
            if path.read_dir().map_or(true, |mut d| d.next().is_none()) {
                let _ = std::fs::remove_dir(&path);
            }
        } else if path.extension().is_some_and(|ext| ext == "json") && !expected.contains(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Write `value` as pretty-printed JSON to `dir/filename` via a temporary file
/// and atomic rename, so readers never see a partial write.
fn atomic_write_json<T: serde::Serialize>(dir: &Path, filename: &str, value: &T) -> Result<()> {
    let target = dir.join(filename);
    let tmp = dir.join(format!(".{}.tmp", filename));

    let json = serde_json::to_string_pretty(value)
        .with_context(|| format!("Failed to serialize {filename}"))?;

    std::fs::write(&tmp, json.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;

    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;

    Ok(())
}
