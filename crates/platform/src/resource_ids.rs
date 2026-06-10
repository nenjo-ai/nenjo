use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nenjo::Slug;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const RESOURCE_IDS_FILENAME: &str = "platform_resource_ids.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformResourceKind {
    Agent,
    Ability,
    Domain,
    ContextBlock,
    Project,
    Routine,
    Model,
    Council,
    McpServer,
}

impl PlatformResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Ability => "ability",
            Self::Domain => "domain",
            Self::ContextBlock => "context_block",
            Self::Project => "project",
            Self::Routine => "routine",
            Self::Model => "model",
            Self::Council => "council",
            Self::McpServer => "mcp_server",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformResourceIdSnapshot {
    #[serde(default)]
    entries: BTreeMap<String, BTreeMap<String, Uuid>>,
}

impl PlatformResourceIdSnapshot {
    pub fn insert(&mut self, kind: PlatformResourceKind, slug: &Slug, id: Uuid) {
        self.entries
            .entry(kind.as_str().to_owned())
            .or_default()
            .insert(slug.as_str().to_owned(), id);
    }

    pub fn remove(&mut self, kind: PlatformResourceKind, slug: &Slug) {
        if let Some(entries) = self.entries.get_mut(kind.as_str()) {
            entries.remove(slug.as_str());
        }
    }

    pub fn remove_id(&mut self, kind: PlatformResourceKind, id: Uuid) {
        let Some(entries) = self.entries.get_mut(kind.as_str()) else {
            return;
        };
        entries.retain(|_, existing_id| *existing_id != id);
    }

    pub fn get(&self, kind: PlatformResourceKind, slug: &Slug) -> Option<Uuid> {
        self.entries
            .get(kind.as_str())
            .and_then(|entries| entries.get(slug.as_str()))
            .copied()
    }
}

#[derive(Debug, Clone)]
pub struct PlatformResourceIdStore {
    path: PathBuf,
}

impl PlatformResourceIdStore {
    pub fn new(manifests_dir: impl AsRef<Path>) -> Self {
        Self {
            path: manifests_dir.as_ref().join(RESOURCE_IDS_FILENAME),
        }
    }

    pub fn load(&self) -> Result<PlatformResourceIdSnapshot> {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(PlatformResourceIdSnapshot::default())
            }
            Err(error) => {
                Err(error).with_context(|| format!("failed to read {}", self.path.display()))
            }
        }
    }

    pub fn replace(&self, snapshot: &PlatformResourceIdSnapshot) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(snapshot)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }

    pub fn upsert(&self, kind: PlatformResourceKind, slug: &Slug, id: Uuid) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.remove_id(kind, id);
        snapshot.insert(kind, slug, id);
        self.replace(&snapshot)
    }

    pub fn remove(&self, kind: PlatformResourceKind, slug: &Slug) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.remove(kind, slug);
        self.replace(&snapshot)
    }

    pub fn remove_by_id(&self, kind: PlatformResourceKind, id: Uuid) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.remove_id(kind, id);
        self.replace(&snapshot)
    }

    pub fn get(&self, kind: PlatformResourceKind, slug: &Slug) -> Result<Option<Uuid>> {
        Ok(self.load()?.get(kind, slug))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn upsert_replaces_stale_slug_alias_for_same_id() {
        let dir = TempDir::new().unwrap();
        let store = PlatformResourceIdStore::new(dir.path());
        let id = Uuid::new_v4();
        let old_slug = Slug::parse("old_routine").unwrap();
        let new_slug = Slug::parse("new_routine").unwrap();

        store.upsert(PlatformResourceKind::Routine, &old_slug, id).unwrap();
        store.upsert(PlatformResourceKind::Routine, &new_slug, id).unwrap();

        assert_eq!(
            store.get(PlatformResourceKind::Routine, &old_slug).unwrap(),
            None
        );
        assert_eq!(
            store.get(PlatformResourceKind::Routine, &new_slug).unwrap(),
            Some(id)
        );
    }

    #[test]
    fn remove_by_id_clears_all_slug_aliases() {
        let dir = TempDir::new().unwrap();
        let store = PlatformResourceIdStore::new(dir.path());
        let id = Uuid::new_v4();
        let old_slug = Slug::parse("old_routine").unwrap();
        let new_slug = Slug::parse("new_routine").unwrap();

        store.upsert(PlatformResourceKind::Routine, &old_slug, id).unwrap();
        store.upsert(PlatformResourceKind::Routine, &new_slug, id).unwrap();
        store.remove_by_id(PlatformResourceKind::Routine, id).unwrap();

        assert_eq!(
            store.get(PlatformResourceKind::Routine, &old_slug).unwrap(),
            None
        );
        assert_eq!(
            store.get(PlatformResourceKind::Routine, &new_slug).unwrap(),
            None
        );
    }
}
