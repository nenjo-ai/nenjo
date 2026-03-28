//! Worker bootstrap — fetch and cache user data on startup.
//!
//! Calls `GET /api/v1/agents/bootstrap` and writes the response as individual
//! JSON files under `~/.nenjo/data/`. If the backend is unreachable the worker
//! continues with a warning; filesystem failures are hard errors.

use anyhow::{Context, Result};
use std::path::Path;
use tracing::{debug, info, warn};

use crate::doc_sync;
use nenjo::client::NenjoClient;
use nenjo::manifest::LambdaManifest;
use nenjo::manifest::{ContextBlockManifest, Manifest, ManifestLoader};
use std::path::PathBuf;
use uuid::Uuid;

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
pub async fn sync(api: &NenjoClient, data_dir: &Path, workspace_dir: &Path) -> Result<()> {
    // Ensure the data directory exists (filesystem error = hard fail)
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("Failed to create data directory: {}", data_dir.display()))?;

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
        skills = data.skills.len(),
        domains = data.domains.len(),
        lambdas = data.lambdas.len(),
        mcp_servers = data.mcp_servers.len(),
        "Manifest fetched successfully"
    );

    // Write auth info (user_id + api_key_id) as a single file.
    debug!(user_id = %data.user_id, api_key_id = ?data.api_key_id, "Writing auth.json");
    atomic_write_json(
        data_dir,
        "auth.json",
        &serde_json::json!({
            "user_id": data.user_id,
            "api_key_id": data.api_key_id,
        }),
    )?;
    atomic_write_json(data_dir, "projects.json", &data.projects)?;
    atomic_write_json(data_dir, "routines.json", &data.routines)?;
    atomic_write_json(data_dir, "models.json", &data.models)?;
    atomic_write_json(data_dir, "agents.json", &data.agents)?;
    atomic_write_json(data_dir, "councils.json", &data.councils)?;
    atomic_write_json(data_dir, "skills.json", &data.skills)?;
    atomic_write_json(data_dir, "domains.json", &data.domains)?;
    atomic_write_json(data_dir, "lambdas.json", &data.lambdas)?;
    atomic_write_json(data_dir, "mcp_servers.json", &data.mcp_servers)?;
    atomic_write_json(data_dir, "abilities.json", &data.abilities)?;
    atomic_write_json(data_dir, "context_blocks.json", &data.context_blocks)?;

    // Sync lambda script files to workspace
    sync_lambdas(workspace_dir, &data.lambdas)?;

    // Sync project documents to workspace
    doc_sync::sync_all(api, workspace_dir, &data.projects).await?;

    Ok(())
}

/// Write lambda script bodies to `{workspace_dir}/../lambdas/{path}` and set
/// the executable bit so scripts with shebangs can be run directly.
pub fn sync_lambdas(workspace_dir: &Path, lambdas: &[LambdaManifest]) -> Result<()> {
    let lambdas_dir = workspace_dir
        .parent()
        .unwrap_or(workspace_dir)
        .join("lambdas");
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

    info!(count = lambdas.len(), dir = %lambdas_dir.display(), "Lambda scripts synced");
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
