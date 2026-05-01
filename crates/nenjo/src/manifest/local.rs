use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::warn;
use uuid::Uuid;

use super::store::{ManifestReader, ManifestWriter};
use super::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    Manifest, ManifestAuth, ManifestLoader, ManifestResource, ManifestResourceKind,
    McpServerManifest, ModelManifest, ProjectManifest, RoutineManifest,
};

static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Filesystem-backed manifest reader/writer that preserves the current worker cache layout.
#[derive(Debug, Clone)]
pub struct LocalManifestStore {
    root: PathBuf,
}

impl LocalManifestStore {
    /// Create a filesystem-backed store rooted at the worker manifest cache directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Return the on-disk root used for manifest cache files.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn load_json<T: serde::de::DeserializeOwned>(&self, filename: &str) -> Vec<T> {
        let path = self.root.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!(file = %path.display(), error = %e, "Failed to parse cached manifest JSON");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        }
    }

    fn load_tree<T: serde::de::DeserializeOwned>(&self, subdir: &str, legacy_file: &str) -> Vec<T> {
        let dir = self.root.join(subdir);
        if dir.is_dir() {
            let mut items = Vec::new();
            walk_json_files(&dir, &mut items);
            items
        } else {
            self.load_json(legacy_file)
        }
    }

    fn auth(&self) -> Option<ManifestAuth> {
        let auth_path = self.root.join("auth.json");

        if let Ok(s) = std::fs::read_to_string(&auth_path) {
            let value: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
            let user_id: Uuid = value
                .get("user_id")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let org_id: Uuid = value
                .get("org_id")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let api_key_id = value
                .get("api_key_id")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            if user_id.is_nil() && org_id.is_nil() && api_key_id.is_none() {
                return None;
            } else {
                return Some(ManifestAuth {
                    user_id,
                    org_id,
                    api_key_id,
                });
            }
        }
        None
    }
}

#[async_trait]
impl ManifestReader for LocalManifestStore {
    async fn load_manifest(&self) -> Result<Manifest> {
        Ok(Manifest {
            auth: self.auth(),
            projects: self.load_json("projects.json"),
            routines: self.load_json("routines.json"),
            models: self.load_json("models.json"),
            agents: self.load_json("agents.json"),
            councils: self.load_json("councils.json"),
            domains: self.load_tree("domains", "domains.json"),
            mcp_servers: self.load_json("mcp_servers.json"),
            abilities: self.load_tree("abilities", "abilities.json"),
            context_blocks: self.load_tree("context_blocks", "context_blocks.json"),
        })
    }

    async fn list_agents(&self) -> Result<Vec<AgentManifest>> {
        Ok(self.load_manifest().await?.agents)
    }

    async fn get_agent(&self, id: Uuid) -> Result<Option<AgentManifest>> {
        Ok(self
            .list_agents()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_models(&self) -> Result<Vec<ModelManifest>> {
        Ok(self.load_manifest().await?.models)
    }

    async fn get_model(&self, id: Uuid) -> Result<Option<ModelManifest>> {
        Ok(self
            .list_models()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_routines(&self) -> Result<Vec<RoutineManifest>> {
        Ok(self.load_manifest().await?.routines)
    }

    async fn get_routine(&self, id: Uuid) -> Result<Option<RoutineManifest>> {
        Ok(self
            .list_routines()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_projects(&self) -> Result<Vec<ProjectManifest>> {
        Ok(self.load_manifest().await?.projects)
    }

    async fn get_project(&self, id: Uuid) -> Result<Option<ProjectManifest>> {
        Ok(self
            .list_projects()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_councils(&self) -> Result<Vec<CouncilManifest>> {
        Ok(self.load_manifest().await?.councils)
    }

    async fn get_council(&self, id: Uuid) -> Result<Option<CouncilManifest>> {
        Ok(self
            .list_councils()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_domains(&self) -> Result<Vec<DomainManifest>> {
        Ok(self.load_manifest().await?.domains)
    }

    async fn get_domain(&self, id: Uuid) -> Result<Option<DomainManifest>> {
        Ok(self
            .list_domains()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_mcp_servers(&self) -> Result<Vec<McpServerManifest>> {
        Ok(self.load_manifest().await?.mcp_servers)
    }

    async fn get_mcp_server(&self, id: Uuid) -> Result<Option<McpServerManifest>> {
        Ok(self
            .list_mcp_servers()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_abilities(&self) -> Result<Vec<AbilityManifest>> {
        Ok(self.load_manifest().await?.abilities)
    }

    async fn get_ability(&self, id: Uuid) -> Result<Option<AbilityManifest>> {
        Ok(self
            .list_abilities()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }

    async fn list_context_blocks(&self) -> Result<Vec<ContextBlockManifest>> {
        Ok(self.load_manifest().await?.context_blocks)
    }

    async fn get_context_block(&self, id: Uuid) -> Result<Option<ContextBlockManifest>> {
        Ok(self
            .list_context_blocks()
            .await?
            .into_iter()
            .find(|item| item.id == id))
    }
}

#[async_trait]
impl ManifestWriter for LocalManifestStore {
    async fn replace_manifest(&self, manifest: &Manifest) -> Result<()> {
        std::fs::create_dir_all(&self.root).with_context(|| {
            format!(
                "Failed to create manifest directory: {}",
                self.root.display()
            )
        })?;

        atomic_write_json(
            &self.root,
            "auth.json",
            &serde_json::json!({
                "user_id": manifest.auth.as_ref().map(|auth| auth.user_id),
                "org_id": manifest.auth.as_ref().map(|auth| auth.org_id),
                "api_key_id": manifest.auth.as_ref().and_then(|auth| auth.api_key_id),
            }),
        )?;
        atomic_write_json(&self.root, "projects.json", &manifest.projects)?;
        atomic_write_json(&self.root, "routines.json", &manifest.routines)?;
        atomic_write_json(&self.root, "models.json", &manifest.models)?;
        atomic_write_json(&self.root, "agents.json", &manifest.agents)?;
        atomic_write_json(&self.root, "councils.json", &manifest.councils)?;
        sync_tree(&self.root.join("domains"), &manifest.domains)?;
        atomic_write_json(&self.root, "mcp_servers.json", &manifest.mcp_servers)?;
        sync_tree(&self.root.join("abilities"), &manifest.abilities)?;
        sync_tree(&self.root.join("context_blocks"), &manifest.context_blocks)?;
        Ok(())
    }

    async fn upsert_resource(&self, resource: &ManifestResource) -> Result<ManifestResource> {
        let mut manifest = self.load_manifest().await?;
        manifest.upsert_resource(resource.clone());
        self.replace_manifest(&manifest).await?;
        Ok(resource.clone())
    }

    async fn delete_resource(&self, kind: ManifestResourceKind, id: Uuid) -> Result<()> {
        let mut manifest = self.load_manifest().await?;
        manifest.delete_resource(kind, id);
        self.replace_manifest(&manifest).await
    }
}

#[async_trait]
impl ManifestLoader for LocalManifestStore {
    async fn load(&self) -> Result<Manifest> {
        self.load_manifest().await
    }
}

trait TreeItem: serde::Serialize {
    fn path(&self) -> &str;
    fn name(&self) -> &str;
}

impl TreeItem for AbilityManifest {
    fn path(&self) -> &str {
        &self.path
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl TreeItem for DomainManifest {
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

fn walk_json_files<T: serde::de::DeserializeOwned>(dir: &Path, items: &mut Vec<T>) {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().collect(),
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
                    Err(error) => {
                        warn!(file = %path.display(), error = %error, "Failed to parse manifest tree item")
                    }
                },
                Err(error) => {
                    warn!(file = %path.display(), error = %error, "Failed to read manifest tree item")
                }
            }
        }
    }
}

fn sync_tree<T: TreeItem>(root: &Path, items: &[T]) -> Result<()> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("Failed to create manifest tree dir: {}", root.display()))?;

    let mut expected = std::collections::HashSet::new();
    for item in items {
        let path = tree_item_path(root, item.path(), item.name());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory: {}", parent.display())
            })?;
        }
        write_json_file(&path, item)?;
        expected.insert(path);
    }

    prune_tree(root, &expected)?;
    Ok(())
}

fn prune_tree(root: &Path, expected: &std::collections::HashSet<PathBuf>) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(root)
        .with_context(|| format!("Failed to read manifest tree dir: {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            prune_tree(&path, expected)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)
                    .with_context(|| format!("Failed to remove empty dir: {}", path.display()))?;
            }
        } else if path.extension().is_some_and(|ext| ext == "json") && !expected.contains(&path) {
            std::fs::remove_file(&path).with_context(|| {
                format!("Failed to remove stale manifest file: {}", path.display())
            })?;
        }
    }

    Ok(())
}

fn tree_item_path(root: &Path, item_path: &str, name: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    if !item_path.is_empty() {
        path = path.join(item_path);
    }
    path.join(format!("{name}.json"))
}

fn atomic_write_json<T: serde::Serialize>(dir: &Path, filename: &str, value: &T) -> Result<()> {
    let path = dir.join(filename);
    write_json_file(&path, value)
}

fn write_json_file<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_json::to_vec_pretty(value)?;
    let temp = temp_path(path);
    std::fs::write(&temp, content)
        .with_context(|| format!("Failed to write temp manifest file: {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| {
        format!(
            "Failed to atomically replace manifest file: {}",
            path.display()
        )
    })?;
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let counter = WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("manifest.json");
    path.with_file_name(format!(".{file_name}.{counter}.tmp"))
}

#[cfg(test)]
mod tests {
    use crate::manifest::{AbilityPromptConfig, PromptConfig};
    use tempfile::tempdir;

    use super::*;

    fn sample_manifest() -> Manifest {
        let model = ModelManifest {
            id: Uuid::new_v4(),
            name: "test-model".into(),
            description: None,
            model: "gpt-4o".into(),
            model_provider: "openai".into(),
            temperature: Some(0.3),
            base_url: None,
        };

        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "coder".into(),
            description: None,
            prompt_config: PromptConfig::default(),
            color: Some("blue".into()),
            model_id: Some(model.id),
            domain_ids: vec![],
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };

        let ability = AbilityManifest {
            id: Uuid::new_v4(),
            name: "research".into(),
            tool_name: "research".into(),
            path: "team/core".into(),
            display_name: None,
            description: None,
            activation_condition: "always".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "Use research".into(),
            },
            platform_scopes: vec![],
            mcp_server_ids: vec![],
        };

        Manifest {
            auth: Some(ManifestAuth {
                user_id: Uuid::new_v4(),
                org_id: Uuid::new_v4(),
                api_key_id: Some(Uuid::new_v4()),
            }),
            models: vec![model],
            agents: vec![agent],
            abilities: vec![ability],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn round_trips_manifest() {
        let dir = tempdir().unwrap();
        let store = LocalManifestStore::new(dir.path());
        let manifest = sample_manifest();

        store.replace_manifest(&manifest).await.unwrap();
        let loaded = store.load_manifest().await.unwrap();

        assert_eq!(
            loaded.auth.as_ref().map(|auth| auth.user_id),
            manifest.auth.as_ref().map(|auth| auth.user_id)
        );
        assert_eq!(
            loaded.auth.as_ref().and_then(|auth| auth.api_key_id),
            manifest.auth.as_ref().and_then(|auth| auth.api_key_id)
        );
        assert_eq!(
            loaded.auth.as_ref().map(|auth| auth.org_id),
            manifest.auth.as_ref().map(|auth| auth.org_id)
        );
        assert_eq!(loaded.models.len(), 1);
        assert_eq!(loaded.agents.len(), 1);
        assert_eq!(loaded.abilities.len(), 1);
    }

    #[tokio::test]
    async fn upserts_and_deletes_resource() {
        let dir = tempdir().unwrap();
        let store = LocalManifestStore::new(dir.path());
        let manifest = sample_manifest();
        store.replace_manifest(&manifest).await.unwrap();

        let mut agent = manifest.agents[0].clone();
        agent.name = "reviewer".into();
        store
            .upsert_resource(&ManifestResource::Agent(agent.clone()))
            .await
            .unwrap();

        assert_eq!(
            store.get_agent(agent.id).await.unwrap().unwrap().name,
            "reviewer"
        );

        store
            .delete_resource(ManifestResourceKind::Agent, agent.id)
            .await
            .unwrap();

        assert!(store.get_agent(agent.id).await.unwrap().is_none());
    }
}
