//! Filesystem manifest loader — reads cached bootstrap JSON from `~/.nenjo/manifests/`.
//!
//! The bootstrap module fetches data from the backend API and writes JSON files.
//! This loader reads those files and assembles a [`Manifest`].

use std::path::PathBuf;

use anyhow::Result;
use tracing::warn;

use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    LambdaManifest, Manifest, McpServerManifest, ModelManifest, ProjectManifest, RoutineManifest,
    SkillManifest,
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
}

#[async_trait::async_trait]
impl nenjo::ManifestLoader for FileSystemManifestLoader {
    async fn load(&self) -> Result<Manifest> {
        // Read auth info (user_id + api_key_id).
        // Falls back to legacy user_id.json for backward compat.
        let (user_id, api_key_id) = {
            let auth_path = self.manifests_dir.join("auth.json");
            let legacy_path = self.manifests_dir.join("user_id.json");

            if let Ok(s) = std::fs::read_to_string(&auth_path) {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
                let uid = v
                    .get("user_id")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let kid = v
                    .get("api_key_id")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                (uid, kid)
            } else {
                let uid = std::fs::read_to_string(&legacy_path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                (uid, None)
            }
        };

        Ok(Manifest {
            user_id,
            api_key_id,
            projects: self.load_json::<ProjectManifest>("projects.json"),
            routines: self.load_json::<RoutineManifest>("routines.json"),
            models: self.load_json::<ModelManifest>("models.json"),
            agents: self.load_json::<AgentManifest>("agents.json"),
            councils: self.load_json::<CouncilManifest>("councils.json"),
            skills: self.load_json::<SkillManifest>("skills.json"),
            domains: self.load_json::<DomainManifest>("domains.json"),
            lambdas: self.load_json::<LambdaManifest>("lambdas.json"),
            mcp_servers: self.load_json::<McpServerManifest>("mcp_servers.json"),
            abilities: self.load_json::<AbilityManifest>("abilities.json"),
            context_blocks: self.load_json::<ContextBlockManifest>("context_blocks.json"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::ManifestLoader;
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
            tags: vec![],
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
            is_system: false,
            prompt_config: serde_json::json!({}),
            color: None,
            model_id: Some(model.id),
            model_name: Some("test-model".into()),
            skills: vec![],
            domains: vec![],
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            abilities: vec![],
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
            is_system: false,
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
            "skills.json",
            "domains.json",
            "lambdas.json",
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
