use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::manifest_contract::{
    KnowledgeDocumentEdgeRecord, KnowledgeDocumentRecord, parse_doc_edge_type,
    to_knowledge_manifest,
};
use anyhow::{Context, Result};
use nenjo::Slug;
use nenjo::manifest::{
    KnowledgePackManifest as RuntimeKnowledgePackManifest, KnowledgePackSource, ManifestResource,
};
use nenjo::{ManifestReader, ManifestWriter};
use nenjo_knowledge::{
    KnowledgeDocEdge, KnowledgeDocFilter, KnowledgeDocManifest, KnowledgePack,
    KnowledgePackManifest as KnowledgePackManifestTrait,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct LibraryKnowledgePack {
    library_dir: PathBuf,
    manifest: LibraryKnowledgePackManifest,
}

impl LibraryKnowledgePack {
    pub const MANIFEST_FILENAME: &'static str = LIBRARY_KNOWLEDGE_MANIFEST_FILENAME;

    pub fn load(library_dir: impl Into<PathBuf>) -> Option<Self> {
        let library_dir = library_dir.into();
        let manifest_path = library_dir.join(Self::MANIFEST_FILENAME);
        let content = std::fs::read_to_string(manifest_path).ok()?;
        let mut manifest: LibraryKnowledgePackManifest = serde_json::from_str(&content).ok()?;
        manifest.refresh_related_edges();
        Some(Self {
            library_dir,
            manifest,
        })
    }
}

pub const LIBRARY_KNOWLEDGE_MANIFEST_FILENAME: &str = "manifest.json";

/// Local library knowledge manifest stored at `~/.nenjo/library/<pack>/manifest.json`.
///
/// This file is the materialized document graph for one uploaded library pack.
/// It is not the pack registry. The worker only considers a pack available when
/// `~/.nenjo/manifests/knowledge_packs.json` also contains a matching
/// [`RuntimeKnowledgePackManifest`]. Keep that registry in sync by routing all
/// local library cache writes through [`ensure_library_knowledge_pack_cache`].
///
/// Document contents stay out of this manifest. Tools lazy-load content from
/// `~/.nenjo/library/<pack>/docs/...` using each document's `source_path`.
///
/// Edges are stored once in `edge_records` with platform UUIDs and slug fields.
/// `refresh_related_edges` projects those records into per-document `related`
/// lists for traversal and prompt metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryKnowledgePackManifest {
    pub pack_slug: String,
    #[serde(default = "default_library_version")]
    pub version: String,
    pub schema_version: u32,
    pub root_uri: String,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub synced_at: String,
    pub docs: Vec<KnowledgeDocManifest>,
    #[serde(default)]
    pub edge_records: Vec<KnowledgeDocumentEdgeRecord>,
}

fn default_library_version() -> String {
    "1".to_string()
}

impl KnowledgePackManifestTrait for LibraryKnowledgePackManifest {
    fn pack_id(&self) -> &str {
        &self.pack_slug
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn schema_version(&self) -> u32 {
        self.schema_version
    }

    fn root_uri(&self) -> &str {
        &self.root_uri
    }

    fn content_hash(&self) -> &str {
        &self.content_hash
    }

    fn docs(&self) -> &[KnowledgeDocManifest] {
        &self.docs
    }
}

impl LibraryKnowledgePackManifest {
    pub fn library_pack(pack_slug: &str) -> Self {
        Self {
            pack_slug: pack_slug.to_string(),
            version: "1".to_string(),
            schema_version: 1,
            root_uri: format!("library://{pack_slug}/"),
            content_hash: String::new(),
            synced_at: chrono::Utc::now().to_rfc3339(),
            docs: Vec::new(),
            edge_records: Vec::new(),
        }
    }

    pub fn touch(&mut self) {
        self.synced_at = chrono::Utc::now().to_rfc3339();
    }

    pub fn remove_document(&mut self, doc: &Slug) -> bool {
        let doc_id = doc.as_str();
        let removed_paths: std::collections::HashSet<String> = self
            .docs
            .iter()
            .filter(|doc| doc.id == doc_id)
            .map(|doc| doc.selector.clone())
            .collect();
        let original_len = self.docs.len();
        self.docs.retain(|doc| doc.id != doc_id);
        if self.docs.len() == original_len {
            return false;
        }
        self.edge_records.retain(|edge| {
            edge.source_doc != doc_id
                && edge.target_doc != doc_id
                && !removed_paths.contains(&edge.source_doc)
                && !removed_paths.contains(&edge.target_doc)
        });
        self.refresh_related_edges();
        self.touch();
        true
    }

    pub fn doc_by_slug(&self, doc_slug: &Slug) -> Option<&KnowledgeDocManifest> {
        self.docs.iter().find(|doc| doc.id == doc_slug.as_str())
    }

    pub fn upsert_library_doc(
        &mut self,
        pack_slug: &str,
        record: &KnowledgeDocumentRecord,
        replace_edges: ReplaceDocumentEdges,
    ) {
        let mut next = library_knowledge_doc(pack_slug, record, |_| None);
        next.related = Vec::new();
        if let Some(pos) = self.docs.iter().position(|doc| doc.id == next.id) {
            self.docs[pos] = next;
        } else {
            self.docs.push(next);
        }

        if replace_edges == ReplaceDocumentEdges::Yes {
            let record_id = record.id;
            self.edge_records.retain(|edge| {
                edge.source_doc != record.slug
                    && edge.target_doc != record.slug
                    && edge.source_item_id != record_id
                    && edge.target_item_id != record_id
            });
            self.edge_records.extend(record.edges.iter().cloned());
        }

        self.refresh_related_edges();
        self.docs
            .sort_by(|left, right| left.selector.cmp(&right.selector));
        self.touch();
    }

    pub fn refresh_related_edges(&mut self) {
        let selectors_by_slug: HashMap<String, String> = self
            .docs
            .iter()
            .map(|doc| (doc.id.clone(), doc.selector.clone()))
            .collect();
        for doc in &mut self.docs {
            doc.related.clear();
        }
        for edge in &self.edge_records {
            let Some(target) = selectors_by_slug.get(&edge.target_doc).cloned() else {
                continue;
            };
            let Some(source) = self.docs.iter_mut().find(|doc| doc.id == edge.source_doc) else {
                continue;
            };
            if source.related.iter().any(|existing| {
                existing.edge_type.as_str() == edge.edge_type && existing.target == target
            }) {
                continue;
            }
            source.related.push(KnowledgeDocEdge {
                edge_type: parse_doc_edge_type(&edge.edge_type),
                target,
                description: edge.note.clone(),
            });
        }
    }
}

#[derive(Debug, Clone)]
/// Pack metadata used to register an uploaded library pack in the local cache.
///
/// Some call sites, such as `create_knowledge_pack`, have full platform pack
/// metadata. Others, such as document-change side effects, only know a pack
/// slug. Optional fields let the canonical helper preserve existing registry
/// metadata when the caller has only partial knowledge.
pub struct LibraryKnowledgePackCacheEntry {
    pub slug: Slug,
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub selector: Option<String>,
    pub version: Option<String>,
    pub read_only: Option<bool>,
    pub metadata: Option<serde_json::Value>,
}

impl LibraryKnowledgePackCacheEntry {
    pub fn from_slug(slug: Slug) -> Self {
        Self {
            slug,
            name: None,
            description: None,
            selector: None,
            version: None,
            read_only: None,
            metadata: None,
        }
    }
}

/// Ensure the local cache has both parts required for an uploaded library pack.
///
/// This is the canonical writer for library pack availability:
/// - `knowledge_packs.json` gets a [`RuntimeKnowledgePackManifest`] registry
///   entry that provider knowledge tools can resolve.
/// - `library/<pack>/manifest.json` exists as the per-pack document graph file.
///
/// Do not make providers rediscover `library/<pack>` directories independently.
/// Missing registry entries are cache corruption and should be repaired through
/// this helper from pack and document mutation/sync paths.
pub async fn ensure_library_knowledge_pack_cache<Store>(
    store: &Store,
    library_root: &Path,
    entry: LibraryKnowledgePackCacheEntry,
) -> Result<RuntimeKnowledgePackManifest>
where
    Store: ManifestReader + ManifestWriter + Send + Sync,
{
    let pack_slug = entry.slug.as_str().to_string();
    let pack_dir = library_root.join(&pack_slug);
    if LibraryKnowledgePack::load(&pack_dir).is_none() {
        write_library_knowledge_manifest(
            &pack_dir,
            &LibraryKnowledgePackManifest::library_pack(&pack_slug),
        )?;
    }

    let manifest = ManifestReader::load_manifest(store).await?;
    let existing = manifest
        .knowledge_packs
        .into_iter()
        .find(|pack| pack.source_type == KnowledgePackSource::Library && pack.slug == entry.slug);
    let resource = RuntimeKnowledgePackManifest {
        slug: entry.slug,
        name: entry
            .name
            .or_else(|| existing.as_ref().map(|pack| pack.name.clone()))
            .unwrap_or_else(|| pack_slug.clone()),
        description: entry
            .description
            .unwrap_or_else(|| existing.as_ref().and_then(|pack| pack.description.clone())),
        source_type: KnowledgePackSource::Library,
        selector: entry
            .selector
            .or_else(|| existing.as_ref().map(|pack| pack.selector.clone()))
            .unwrap_or_else(|| format!("lib:{pack_slug}")),
        version: entry
            .version
            .or_else(|| existing.as_ref().and_then(|pack| pack.version.clone())),
        root_uri: format!("library://{pack_slug}/"),
        root_path: Some(pack_dir),
        read_only: entry
            .read_only
            .or_else(|| existing.as_ref().map(|pack| pack.read_only))
            .unwrap_or(false),
        metadata: entry
            .metadata
            .or_else(|| existing.as_ref().map(|pack| pack.metadata.clone()))
            .unwrap_or_else(|| serde_json::json!({})),
    };
    ManifestWriter::upsert_resource(store, &ManifestResource::KnowledgePack(resource.clone()))
        .await?;
    Ok(resource)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceDocumentEdges {
    Yes,
    No,
}

/// Discover locally synced library knowledge packs under `library_root`.
pub fn discover_library_knowledge_packs(
    library_root: &Path,
) -> Vec<(String, LibraryKnowledgePack)> {
    let Ok(entries) = std::fs::read_dir(library_root) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let slug = entry.file_name().to_string_lossy().to_string();
            let pack = LibraryKnowledgePack::load(entry.path())?;
            Some((slug, pack))
        })
        .collect()
}

pub fn load_library_knowledge_manifest(library_dir: &Path) -> Option<LibraryKnowledgePackManifest> {
    let path = library_dir.join(LIBRARY_KNOWLEDGE_MANIFEST_FILENAME);
    let content = std::fs::read_to_string(&path).ok()?;
    let mut manifest: LibraryKnowledgePackManifest = serde_json::from_str(&content).ok()?;
    manifest.refresh_related_edges();
    Some(manifest)
}

static LIBRARY_MANIFEST_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LIBRARY_MANIFEST_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn library_manifest_lock(pack_dir: &Path) -> Arc<Mutex<()>> {
    let canonical = pack_dir
        .canonicalize()
        .unwrap_or_else(|_| pack_dir.to_path_buf());
    let mut locks = LIBRARY_MANIFEST_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(canonical)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn with_library_manifest_lock<R>(pack_dir: &Path, write: impl FnOnce() -> Result<R>) -> Result<R> {
    let lock = library_manifest_lock(pack_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    write()
}

fn unique_library_manifest_tmp_path(pack_dir: &Path) -> PathBuf {
    let nonce = LIBRARY_MANIFEST_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    pack_dir.join(format!(
        ".{LIBRARY_KNOWLEDGE_MANIFEST_FILENAME}.{pid}.{nonce}.tmp"
    ))
}

fn write_library_knowledge_manifest_unlocked(
    library_dir: &Path,
    manifest: &LibraryKnowledgePackManifest,
) -> Result<()> {
    let target = library_dir.join(LIBRARY_KNOWLEDGE_MANIFEST_FILENAME);
    let tmp = unique_library_manifest_tmp_path(library_dir);
    std::fs::create_dir_all(library_dir).with_context(|| {
        format!(
            "Failed to create library directory {}",
            library_dir.display()
        )
    })?;
    let json = serde_json::to_string_pretty(manifest)
        .context("Failed to serialize library knowledge manifest")?;
    std::fs::write(&tmp, json.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

pub fn write_library_knowledge_manifest(
    library_dir: &Path,
    manifest: &LibraryKnowledgePackManifest,
) -> Result<()> {
    with_library_manifest_lock(library_dir, || {
        write_library_knowledge_manifest_unlocked(library_dir, manifest)
    })
}

pub fn build_library_knowledge_manifest(
    pack_slug: &str,
    records: &[KnowledgeDocumentRecord],
) -> LibraryKnowledgePackManifest {
    let paths_by_slug: HashMap<Slug, String> = records
        .iter()
        .map(|record| {
            (
                Slug::derive(&record.slug),
                record.library_selector(pack_slug),
            )
        })
        .collect();
    let mut entries = records
        .iter()
        .map(|record| {
            library_knowledge_doc(pack_slug, record, |target_doc| {
                paths_by_slug.get(&target_doc).cloned()
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.selector.cmp(&right.selector));
    let mut manifest = LibraryKnowledgePackManifest {
        pack_slug: pack_slug.to_string(),
        version: "1".to_string(),
        schema_version: 1,
        root_uri: format!("library://{pack_slug}/"),
        content_hash: String::new(),
        synced_at: chrono::Utc::now().to_rfc3339(),
        docs: entries,
        edge_records: records
            .iter()
            .flat_map(|record| record.edges.iter().cloned())
            .collect(),
    };
    manifest.refresh_related_edges();
    manifest
}

pub fn upsert_library_knowledge_entry(
    pack_dir: &Path,
    pack_slug: &str,
    record: &KnowledgeDocumentRecord,
) -> Result<()> {
    upsert_library_knowledge_entry_with_edges(
        pack_dir,
        pack_slug,
        record,
        ReplaceDocumentEdges::Yes,
    )
}

pub fn upsert_library_knowledge_entry_with_edges(
    pack_dir: &Path,
    pack_slug: &str,
    record: &KnowledgeDocumentRecord,
    replace_edges: ReplaceDocumentEdges,
) -> Result<()> {
    with_library_manifest_lock(pack_dir, || {
        let mut manifest = load_library_knowledge_manifest(pack_dir)
            .unwrap_or_else(|| LibraryKnowledgePackManifest::library_pack(pack_slug));
        manifest.upsert_library_doc(pack_slug, record, replace_edges);
        write_library_knowledge_manifest_unlocked(pack_dir, &manifest)
    })
}

pub fn write_library_document_content(
    pack_dir: &Path,
    relative_path: &str,
    content: &str,
) -> Result<()> {
    let docs_dir = pack_dir.join("docs");
    let target = docs_dir.join(relative_path.trim_matches('/'));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create docs dir: {}", parent.display()))?;
    }
    let tmp = target.with_file_name(format!(
        ".{}.tmp",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document")
    ));
    std::fs::write(&tmp, content.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

pub fn remove_library_knowledge_entry(library_dir: &Path, doc: &Slug) -> Result<()> {
    with_library_manifest_lock(library_dir, || {
        let Some(mut manifest) = load_library_knowledge_manifest(library_dir) else {
            return Ok(());
        };
        if manifest.remove_document(doc) {
            write_library_knowledge_manifest_unlocked(library_dir, &manifest)?;
        }
        Ok(())
    })
}

pub fn library_knowledge_doc_relative_path(library_dir: &Path, doc: &Slug) -> Option<String> {
    load_library_knowledge_manifest(library_dir)
        .and_then(|manifest| manifest.doc_by_slug(doc).map(manifest_doc_relative_path))
}

pub fn manifest_doc_relative_path(doc: &KnowledgeDocManifest) -> String {
    doc.source_path
        .strip_prefix("docs/")
        .unwrap_or(&doc.source_path)
        .trim_matches('/')
        .to_string()
}

fn library_knowledge_doc(
    pack_slug: &str,
    record: &KnowledgeDocumentRecord,
    resolve_target: impl Fn(Slug) -> Option<String>,
) -> KnowledgeDocManifest {
    to_knowledge_manifest(pack_slug, record, resolve_target)
}

impl KnowledgePack for LibraryKnowledgePack {
    fn manifest(&self) -> &dyn KnowledgePackManifestTrait {
        &self.manifest
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>> {
        let path = safe_relative_path(&manifest.source_path)?;
        std::fs::read_to_string(self.library_dir.join(path))
            .ok()
            .map(Cow::Owned)
    }

    fn read_manifest(&self, path: &str) -> Option<&KnowledgeDocManifest> {
        let normalized = normalize_library_doc_lookup(path, &self.manifest.root_uri);
        self.manifest.docs.iter().find(|doc| {
            doc.id == path
                || doc.selector == path
                || normalize_library_doc_lookup(&doc.selector, &self.manifest.root_uri)
                    == normalized
                || doc
                    .source_path
                    .strip_prefix("docs/")
                    .is_some_and(|source_path| source_path == normalized)
                || doc
                    .source_path
                    .rsplit('/')
                    .next()
                    .is_some_and(|filename| filename == normalized)
        })
    }

    fn list_docs(&self, mut filter: KnowledgeDocFilter) -> Vec<&KnowledgeDocManifest> {
        filter.selector_prefix = filter
            .selector_prefix
            .as_deref()
            .map(|prefix| normalize_library_selector_prefix(prefix, &self.manifest.root_uri));
        if let Some(related_to) = filter.related_to.as_deref()
            && let Some(target) = self.read_manifest(related_to)
        {
            filter.related_to = Some(target.selector.clone());
        }
        self.manifest
            .docs
            .iter()
            .filter(|doc| matches_library_filter(self, doc, &filter))
            .collect()
    }
}

fn matches_library_filter(
    pack: &LibraryKnowledgePack,
    doc: &KnowledgeDocManifest,
    filter: &KnowledgeDocFilter,
) -> bool {
    if let Some(kind) = &filter.kind
        && doc.kind != *kind
    {
        return false;
    }
    if let Some(prefix) = &filter.selector_prefix
        && !doc.selector.starts_with(prefix)
    {
        return false;
    }
    if !filter.tags.is_empty()
        && !filter
            .tags
            .iter()
            .all(|tag| doc.tags.iter().any(|doc_tag| doc_tag == tag))
    {
        return false;
    }
    if let Some(target) = &filter.related_to {
        let has_edge = doc.related.iter().any(|edge| {
            let edge_matches_target = edge.target == *target
                || pack
                    .read_manifest(&edge.target)
                    .map(|edge_target| edge_target.id == *target || edge_target.selector == *target)
                    .unwrap_or(false);
            edge_matches_target
                && filter
                    .edge_type
                    .as_ref()
                    .map(|expected| edge.edge_type == *expected)
                    .unwrap_or(true)
        });
        if !has_edge {
            return false;
        }
    }
    true
}

fn normalize_library_doc_lookup(value: &str, root_uri: &str) -> String {
    value
        .trim()
        .strip_prefix(root_uri)
        .unwrap_or(value.trim())
        .trim_matches('/')
        .to_string()
}

fn normalize_library_selector_prefix(value: &str, root_uri: &str) -> String {
    let trimmed = value.trim().trim_matches('/');
    if trimmed.is_empty() {
        return root_uri.to_string();
    }
    if value.trim().starts_with(root_uri) || value.trim().contains("://") {
        return value.trim().to_string();
    }
    format!("{root_uri}{trimmed}")
}

fn safe_relative_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nenjo::ManifestReader;
    use nenjo_knowledge::KnowledgeDocKind;
    use uuid::Uuid;

    fn library_manifest() -> LibraryKnowledgePackManifest {
        LibraryKnowledgePackManifest {
            pack_slug: "library-test".into(),
            version: "1".into(),
            schema_version: 1,
            root_uri: "library://test/".into(),
            content_hash: String::new(),
            synced_at: String::new(),
            docs: vec![KnowledgeDocManifest {
                id: "doc-1".into(),
                selector: "library://test/architecture.md".into(),
                source_path: "docs/architecture.md".into(),
                title: "Architecture".into(),
                summary: "System architecture".into(),
                kind: KnowledgeDocKind::new("reference"),
                tags: vec!["architecture".into()],
                related: Vec::new(),
                updated_at: String::new(),
            }],
            edge_records: Vec::new(),
        }
    }

    #[tokio::test]
    async fn ensure_library_knowledge_pack_cache_registers_pack_and_writes_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let manifests_dir = temp.path().join("manifests");
        let library_dir = temp.path().join("library");
        let store = nenjo::manifest::local::LocalManifestStore::new(&manifests_dir);

        ensure_library_knowledge_pack_cache(
            &store,
            &library_dir,
            LibraryKnowledgePackCacheEntry {
                slug: Slug::derive("humanizer"),
                name: Some("Humanizer".to_string()),
                description: Some(Some("Writing style knowledge".to_string())),
                selector: Some("lib:humanizer".to_string()),
                version: Some("1".to_string()),
                read_only: Some(false),
                metadata: Some(serde_json::json!({ "source": "test" })),
            },
        )
        .await
        .unwrap();

        let manifest = store.load_manifest().await.unwrap();
        let pack = manifest
            .knowledge_packs
            .iter()
            .find(|pack| pack.slug.as_str() == "humanizer")
            .unwrap();
        assert_eq!(pack.selector, "lib:humanizer");
        assert_eq!(pack.root_path, Some(library_dir.join("humanizer")));
        assert!(library_dir.join("humanizer/manifest.json").exists());
    }

    fn sample_record() -> KnowledgeDocumentRecord {
        let now = Utc::now();
        KnowledgeDocumentRecord {
            id: Uuid::new_v4(),
            org_id: Uuid::new_v4(),
            pack_id: Uuid::new_v4(),
            pack_slug: "product".into(),
            slug: "overview".into(),
            filename: "overview.md".into(),
            path: Some("docs".into()),
            title: Some("Overview".into()),
            kind: Some("guide".into()),
            summary: Some("Product overview".into()),
            tags: Vec::new(),
            content_type: "text/markdown".into(),
            created_at: now,
            updated_at: now,
            edges: Vec::new(),
        }
    }

    fn record_with_slug(id: u128, slug: &str, filename: &str) -> KnowledgeDocumentRecord {
        KnowledgeDocumentRecord {
            id: Uuid::from_u128(id),
            slug: slug.into(),
            filename: filename.into(),
            title: Some(slug.into()),
            ..sample_record()
        }
    }

    fn edge_record(
        id: u128,
        source_id: u128,
        source_doc: &str,
        target_id: u128,
        target_doc: &str,
    ) -> KnowledgeDocumentEdgeRecord {
        let now = Utc::now();
        KnowledgeDocumentEdgeRecord {
            id: Uuid::from_u128(id),
            org_id: Uuid::from_u128(9),
            source_item_id: Uuid::from_u128(source_id),
            source_doc: source_doc.into(),
            target_item_id: Uuid::from_u128(target_id),
            target_doc: target_doc.into(),
            edge_type: "references".into(),
            note: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn library_pack_reads_manifest_metadata_and_lazy_content() {
        let dir = tempfile::tempdir().unwrap();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("architecture.md"), "# Architecture").unwrap();

        let manifest = library_manifest();
        std::fs::write(
            dir.path().join(LibraryKnowledgePack::MANIFEST_FILENAME),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let pack = LibraryKnowledgePack::load(dir.path()).unwrap();

        let hits = pack.search("Architecture", Default::default());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document.title, "Architecture");

        let doc = pack.read_doc("library://test/architecture.md").unwrap();
        assert_eq!(doc.content, "# Architecture");
    }

    #[test]
    fn library_pack_serializes_pack_slug_field() {
        let manifest = library_manifest();
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("\"pack_slug\":\"library-test\""));
        assert!(json.contains("\"edge_records\":"));
        assert!(!json.contains("\"edges\":"));
    }

    #[test]
    fn library_pack_rejects_unsafe_source_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = library_manifest();
        manifest.docs[0].source_path = "../secret.md".into();
        let pack = LibraryKnowledgePack {
            library_dir: dir.path().into(),
            manifest,
        };

        assert!(pack.read_doc("doc-1").is_none());
    }

    #[test]
    fn library_knowledge_manifests_use_library_paths() {
        let manifest = build_library_knowledge_manifest("product", &[sample_record()]);

        assert_eq!(manifest.root_uri, "library://product/");
        assert_eq!(
            manifest.docs[0].selector,
            "library://product/docs/overview.md"
        );
    }

    #[test]
    fn concurrent_library_manifest_upserts_do_not_lose_documents() {
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("humanizer");
        std::fs::create_dir_all(&pack_dir).unwrap();
        write_library_knowledge_manifest(
            &pack_dir,
            &LibraryKnowledgePackManifest::library_pack("humanizer"),
        )
        .unwrap();

        let handles = (0..32)
            .map(|index| {
                let pack_dir = pack_dir.clone();
                thread::spawn(move || {
                    let mut record = sample_record();
                    record.slug = format!("pattern-{index}");
                    record.filename = format!("pattern-{index}.md");
                    upsert_library_knowledge_entry(&pack_dir, "humanizer", &record)
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        let manifest = load_library_knowledge_manifest(&pack_dir).unwrap();
        assert_eq!(manifest.docs.len(), 32);
    }

    #[test]
    fn edge_projection_tracks_target_doc_rename_without_rewriting_edge_identity() {
        let mut source = record_with_slug(1, "source", "source.md");
        source.edges = vec![edge_record(3, 1, "source", 2, "target")];
        let target = record_with_slug(2, "target", "target.md");
        let mut manifest = build_library_knowledge_manifest("product", &[source, target]);

        let source_doc = manifest.doc_by_slug(&Slug::derive("source")).unwrap();
        assert_eq!(
            source_doc.related[0].target,
            "library://product/docs/target.md"
        );

        let renamed_target = KnowledgeDocumentRecord {
            filename: "renamed.md".into(),
            edges: Vec::new(),
            ..record_with_slug(2, "target", "target.md")
        };
        manifest.upsert_library_doc("product", &renamed_target, ReplaceDocumentEdges::No);

        let source_doc = manifest.doc_by_slug(&Slug::derive("source")).unwrap();
        assert_eq!(manifest.edge_records[0].id, Uuid::from_u128(3));
        assert_eq!(
            source_doc.related[0].target,
            "library://product/docs/renamed.md"
        );
    }
}
