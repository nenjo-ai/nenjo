use std::collections::{BTreeMap, HashSet};
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
    Command,
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
            Self::Command => "command",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformResourceIdSnapshot {
    #[serde(default)]
    entries: BTreeMap<String, BTreeMap<String, Uuid>>,
    /// Pack slug -> document slug -> platform item id.
    #[serde(default)]
    knowledge_documents: BTreeMap<String, BTreeMap<String, Uuid>>,
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

    pub fn insert_knowledge_document(&mut self, pack: &str, doc: &str, id: Uuid) {
        self.knowledge_documents
            .entry(pack.to_owned())
            .or_default()
            .insert(doc.to_owned(), id);
    }

    pub fn remove_knowledge_document(&mut self, pack: &str, doc: &str) {
        if let Some(entries) = self.knowledge_documents.get_mut(pack) {
            entries.remove(doc);
            if entries.is_empty() {
                self.knowledge_documents.remove(pack);
            }
        }
    }

    pub fn remove_knowledge_document_by_id(&mut self, id: Uuid) {
        self.knowledge_documents.retain(|_, entries| {
            entries.retain(|_, existing_id| *existing_id != id);
            !entries.is_empty()
        });
    }

    pub fn get_knowledge_document(&self, pack: &str, doc: &str) -> Option<Uuid> {
        self.knowledge_documents
            .get(pack)
            .and_then(|entries| entries.get(doc))
            .copied()
    }

    pub fn remove_knowledge_pack(&mut self, pack: &str) {
        self.knowledge_documents.remove(pack);
    }

    pub fn reconcile_knowledge_packs(&mut self, remote_packs: &HashSet<String>) {
        self.knowledge_documents
            .retain(|pack, _| remote_packs.contains(pack));
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

    pub fn upsert_knowledge_document(&self, pack: &Slug, doc: &Slug, id: Uuid) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.remove_knowledge_document_by_id(id);
        snapshot.insert_knowledge_document(pack.as_str(), doc.as_str(), id);
        self.replace(&snapshot)
    }

    pub fn remove_knowledge_document(&self, pack: &Slug, doc: &Slug) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.remove_knowledge_document(pack.as_str(), doc.as_str());
        self.replace(&snapshot)
    }

    pub fn remove_knowledge_document_by_id(&self, id: Uuid) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.remove_knowledge_document_by_id(id);
        self.replace(&snapshot)
    }

    pub fn get_knowledge_document(&self, pack: &Slug, doc: &Slug) -> Result<Option<Uuid>> {
        Ok(self
            .load()?
            .get_knowledge_document(pack.as_str(), doc.as_str()))
    }

    pub fn remove_knowledge_pack(&self, pack: &Slug) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.remove_knowledge_pack(pack.as_str());
        self.replace(&snapshot)
    }

    pub fn reconcile_knowledge_packs(&self, remote_packs: &HashSet<String>) -> Result<()> {
        let mut snapshot = self.load()?;
        snapshot.reconcile_knowledge_packs(remote_packs);
        self.replace(&snapshot)
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

        store
            .upsert(PlatformResourceKind::Routine, &old_slug, id)
            .unwrap();
        store
            .upsert(PlatformResourceKind::Routine, &new_slug, id)
            .unwrap();

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
    fn knowledge_document_ids_are_pack_scoped() {
        let dir = TempDir::new().unwrap();
        let store = PlatformResourceIdStore::new(dir.path());
        let pack = Slug::parse("product").unwrap();
        let doc = Slug::parse("overview").unwrap();
        let id = Uuid::new_v4();

        store.upsert_knowledge_document(&pack, &doc, id).unwrap();

        assert_eq!(store.get_knowledge_document(&pack, &doc).unwrap(), Some(id));

        store.remove_knowledge_document(&pack, &doc).unwrap();
        assert_eq!(store.get_knowledge_document(&pack, &doc).unwrap(), None);
    }

    #[test]
    fn reconcile_knowledge_packs_removes_stale_pack_entries() {
        let dir = TempDir::new().unwrap();
        let store = PlatformResourceIdStore::new(dir.path());
        let kept = Slug::parse("product").unwrap();
        let removed = Slug::parse("removed").unwrap();
        let doc = Slug::parse("overview").unwrap();
        let id = Uuid::new_v4();

        store.upsert_knowledge_document(&kept, &doc, id).unwrap();
        store
            .upsert_knowledge_document(&removed, &doc, Uuid::new_v4())
            .unwrap();

        let mut remote = HashSet::new();
        remote.insert(kept.as_str().to_string());
        store.reconcile_knowledge_packs(&remote).unwrap();

        assert_eq!(store.get_knowledge_document(&kept, &doc).unwrap(), Some(id));
        assert_eq!(store.get_knowledge_document(&removed, &doc).unwrap(), None);
    }

    #[test]
    fn remove_knowledge_pack_clears_all_document_ids() {
        let dir = TempDir::new().unwrap();
        let store = PlatformResourceIdStore::new(dir.path());
        let pack = Slug::parse("product").unwrap();
        let doc = Slug::parse("overview").unwrap();

        store
            .upsert_knowledge_document(&pack, &doc, Uuid::new_v4())
            .unwrap();
        store.remove_knowledge_pack(&pack).unwrap();

        assert_eq!(store.get_knowledge_document(&pack, &doc).unwrap(), None);
    }

    #[test]
    fn remove_by_id_clears_all_slug_aliases() {
        let dir = TempDir::new().unwrap();
        let store = PlatformResourceIdStore::new(dir.path());
        let id = Uuid::new_v4();
        let old_slug = Slug::parse("old_routine").unwrap();
        let new_slug = Slug::parse("new_routine").unwrap();

        store
            .upsert(PlatformResourceKind::Routine, &old_slug, id)
            .unwrap();
        store
            .upsert(PlatformResourceKind::Routine, &new_slug, id)
            .unwrap();
        store
            .remove_by_id(PlatformResourceKind::Routine, id)
            .unwrap();

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
