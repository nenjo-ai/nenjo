//! Filesystem manifest loader — reads cached bootstrap JSON from `~/.nenjo/manifests/`.
//!
//! The bootstrap module fetches data from the backend API and writes JSON files.
//! This loader reads those files and assembles a [`Manifest`].
//!
//! Abilities and context blocks are stored as directory trees (one JSON per item).
//! Other resource types remain as flat JSON arrays.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::warn;

use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    Manifest, ManifestAuth, McpServerManifest, ModelManifest, ProjectManifest, RoutineManifest,
};

/// Loads a [`Manifest`] from cached JSON files on disk.
///
/// Reads from a directory (typically `~/.nenjo/manifests/`) that was populated by
/// the bootstrap module. Each resource type is stored as a separate JSON file.
///
/// Note the naming quirk from bootstrap.rs:
/// - `agents.json` contains **models** (not agents)
/// - `agents.json` contains **agents**
pub struct FileSystemManifestLoader {
    manifests_dir: PathBuf,
}

impl FileSystemManifestLoader {
    pub fn new(manifests_dir: impl Into<PathBuf>) -> Self {
        Self {
            manifests_dir: manifests_dir.into(),
        }
    }

    fn load_json<T: serde::de::DeserializeOwned>(&self, filename: &str) -> Vec<T> {
        let path = self.manifests_dir.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!(file = %path.display(), error = %e, "Failed to parse cached JSON");
                Vec::new()
            }),
            Err(_) => Vec::new(), // file doesn't exist yet (first run)
        }
    }

    /// Walk a directory tree and load all `.json` files as manifest items.
    ///
    /// Falls back to loading a legacy flat JSON array file if the directory
    /// doesn't exist (backward compat with pre-tree storage).
    fn load_tree<T: serde::de::DeserializeOwned>(&self, subdir: &str, legacy_file: &str) -> Vec<T> {
        let dir = self.manifests_dir.join(subdir);
        if dir.is_dir() {
            let mut items = Vec::new();
            walk_json_files(&dir, &mut items);
            items
        } else {
            // Backward compat: try the old flat JSON file.
            self.load_json(legacy_file)
        }
    }
}

/// Recursively walk a directory tree, reading each `.json` file as type `T`.
fn walk_json_files<T: serde::de::DeserializeOwned>(dir: &Path, items: &mut Vec<T>) {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(e) => e.flatten().collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk_json_files(&path, items);
        } else if path.extension().is_some_and(|ext| ext == "json") {
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<T>(&content) {
                    Ok(item) => items.push(item),
                    Err(e) => {
                        warn!(file = %path.display(), error = %e, "Failed to parse tree item");
                    }
                },
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "Failed to read tree item");
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl nenjo::ManifestLoader for FileSystemManifestLoader {
    async fn load(&self) -> Result<Manifest> {
        // Read auth info (user_id + api_key_id).
        // Falls back to legacy user_id.json for backward compat.
        let auth = {
            let auth_path = self.manifests_dir.join("auth.json");

            if let Ok(s) = std::fs::read_to_string(&auth_path) {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
                let uid: uuid::Uuid = v
                    .get("user_id")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let org_id: uuid::Uuid = v
                    .get("org_id")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let kid = v
                    .get("api_key_id")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                if uid.is_nil() && org_id.is_nil() && kid.is_none() {
                    None
                } else {
                    Some(ManifestAuth {
                        user_id: uid,
                        org_id,
                        api_key_id: kid,
                    })
                }
            } else {
                None
            }
        };

        Ok(Manifest {
            auth,
            projects: self.load_json::<ProjectManifest>("projects.json"),
            routines: self.load_json::<RoutineManifest>("routines.json"),
            models: self.load_json::<ModelManifest>("models.json"),
            agents: self.load_json::<AgentManifest>("agents.json"),
            councils: self.load_json::<CouncilManifest>("councils.json"),
            domains: self.load_tree::<DomainManifest>("domains", "domains.json"),
            mcp_servers: self.load_json::<McpServerManifest>("mcp_servers.json"),
            abilities: self.load_tree::<AbilityManifest>("abilities", "abilities.json"),
            context_blocks: self
                .load_tree::<ContextBlockManifest>("context_blocks", "context_blocks.json"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::ManifestLoader;
    use nenjo::agents::prompts::PromptConfig;
    use uuid::Uuid;

    #[tokio::test]
    async fn loads_empty_manifest_from_missing_dir() {
        let loader = FileSystemManifestLoader::new("/tmp/nonexistent-dir-for-test");
        let manifest = loader.load().await.unwrap();

        assert!(manifest.agents.is_empty());
        assert!(manifest.models.is_empty());
        assert!(manifest.routines.is_empty());
    }

    #[tokio::test]
    async fn loads_manifest_from_cached_files() {
        let dir = tempfile::tempdir().unwrap();

        // Write test data matching bootstrap.rs naming
        let model = ModelManifest {
            id: Uuid::new_v4(),
            name: "test-model".into(),
            description: None,
            model: "gpt-4o".into(),
            model_provider: "openai".into(),
            temperature: Some(0.7),
            base_url: None,
        };
        std::fs::write(
            dir.path().join("models.json"),
            serde_json::to_string(&vec![&model]).unwrap(),
        )
        .unwrap();

        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "coder".into(),
            description: Some("A coder".into()),
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: Some(model.id),
            domain_ids: vec![],
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };
        std::fs::write(
            dir.path().join("agents.json"),
            serde_json::to_string(&vec![&agent]).unwrap(),
        )
        .unwrap();

        let project = ProjectManifest {
            id: Uuid::new_v4(),
            name: "test".into(),
            slug: "test".into(),
            description: None,
            settings: serde_json::Value::Null,
        };
        std::fs::write(
            dir.path().join("projects.json"),
            serde_json::to_string(&vec![&project]).unwrap(),
        )
        .unwrap();

        // Write empty arrays for the rest
        for file in &[
            "routines.json",
            "councils.json",
            "domains.json",
            "mcp_servers.json",
            "abilities.json",
            "context_blocks.json",
        ] {
            std::fs::write(dir.path().join(file), "[]").unwrap();
        }

        let loader = FileSystemManifestLoader::new(dir.path());
        let manifest = loader.load().await.unwrap();

        assert_eq!(manifest.models.len(), 1);
        assert_eq!(manifest.models[0].name, "test-model");
        assert_eq!(manifest.agents.len(), 1);
        assert_eq!(manifest.agents[0].name, "coder");
        assert_eq!(manifest.projects.len(), 1);
    }

    #[tokio::test]
    async fn handles_corrupt_json_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agents.json"), "not valid json").unwrap();

        let loader = FileSystemManifestLoader::new(dir.path());
        let manifest = loader.load().await.unwrap();

        // Should return empty, not error
        assert!(manifest.models.is_empty());
    }
}
