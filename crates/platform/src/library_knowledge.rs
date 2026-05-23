use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use nenjo::Slug;
use nenjo::client::{DocumentSyncEdge, DocumentSyncMeta};
use nenjo_knowledge::{
    KnowledgeDocEdge, KnowledgeDocEdgeType, KnowledgeDocFilter, KnowledgeDocKind,
    KnowledgeDocManifest, KnowledgePack, KnowledgePackManifest,
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
        let manifest = serde_json::from_str(&content).ok()?;
        Some(Self {
            library_dir,
            manifest,
        })
    }
}

pub const LIBRARY_KNOWLEDGE_MANIFEST_FILENAME: &str = "manifest.json";

/// Local library knowledge manifest stored as `manifest.json`.
///
/// This is the single source of truth for library knowledge sync state and
/// knowledge metadata. Do not add a second library knowledge manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryKnowledgePackManifest {
    pub pack_id: String,
    #[serde(default = "default_library_pack_version")]
    pub pack_version: String,
    pub schema_version: u32,
    pub root_uri: String,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub synced_at: String,
    pub docs: Vec<KnowledgeDocManifest>,
}

fn default_library_pack_version() -> String {
    "1".to_string()
}

impl KnowledgePackManifest for LibraryKnowledgePackManifest {
    fn pack_id(&self) -> &str {
        &self.pack_id
    }

    fn pack_version(&self) -> &str {
        &self.pack_version
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
            pack_id: pack_slug.to_string(),
            pack_version: "1".to_string(),
            schema_version: 1,
            root_uri: format!("library://{pack_slug}/"),
            content_hash: String::new(),
            synced_at: chrono::Utc::now().to_rfc3339(),
            docs: Vec::new(),
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
            .map(|doc| doc.path.clone())
            .collect();
        let original_len = self.docs.len();
        self.docs.retain(|doc| doc.id != doc_id);
        if self.docs.len() == original_len {
            return false;
        }
        for doc in &mut self.docs {
            doc.related
                .retain(|edge| !removed_paths.contains(&edge.target));
        }
        self.touch();
        true
    }

    pub fn doc_by_slug(&self, doc_slug: &Slug) -> Option<&KnowledgeDocManifest> {
        self.docs.iter().find(|doc| doc.id == doc_slug.as_str())
    }

    pub fn upsert_library_doc(
        &mut self,
        pack_slug: &str,
        metadata: &DocumentSyncMeta,
        edges: &[DocumentSyncEdge],
    ) {
        let path = library_doc_path(pack_slug, metadata);
        let next = library_knowledge_doc(pack_slug, metadata, edges, |target_id| {
            self.docs
                .iter()
                .find(|doc| doc.id == target_id.as_str())
                .map(|doc| doc.path.clone())
        });
        if let Some(pos) = self.docs.iter().position(|doc| doc.id == next.id) {
            self.docs[pos] = next;
        } else {
            self.docs.push(next);
        }
        for doc in &mut self.docs {
            doc.related.retain(|edge| edge.target != path);
        }
        for edge in edges {
            if edge.target_doc == Slug::derive(&metadata.slug)
                && let Some(source) = self
                    .docs
                    .iter_mut()
                    .find(|doc| doc.id == edge.source_doc.as_str())
            {
                let target = path.clone();
                if !source.related.iter().any(|existing| {
                    existing.edge_type.as_str() == edge.edge_type && existing.target == target
                }) {
                    source.related.push(KnowledgeDocEdge {
                        edge_type: parse_doc_edge_type(&edge.edge_type),
                        target,
                        description: edge.note.clone(),
                    });
                }
            }
        }
        self.docs.sort_by(|left, right| left.path.cmp(&right.path));
        self.touch();
    }
}

pub fn load_library_knowledge_manifest(library_dir: &Path) -> Option<LibraryKnowledgePackManifest> {
    let path = library_dir.join(LIBRARY_KNOWLEDGE_MANIFEST_FILENAME);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_library_knowledge_manifest(
    library_dir: &Path,
    manifest: &LibraryKnowledgePackManifest,
) -> Result<()> {
    let target = library_dir.join(LIBRARY_KNOWLEDGE_MANIFEST_FILENAME);
    let tmp = library_dir.join(format!(".{LIBRARY_KNOWLEDGE_MANIFEST_FILENAME}.tmp"));
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

pub fn build_library_knowledge_manifest(
    pack_slug: &str,
    docs: &[DocumentSyncMeta],
    edges_by_doc: &HashMap<Slug, Vec<DocumentSyncEdge>>,
) -> LibraryKnowledgePackManifest {
    let paths_by_slug: HashMap<Slug, String> = docs
        .iter()
        .map(|doc| (Slug::derive(&doc.slug), library_doc_path(pack_slug, doc)))
        .collect();
    let mut entries = docs
        .iter()
        .map(|doc| {
            let doc_slug = Slug::derive(&doc.slug);
            let edges = edges_by_doc
                .get(&doc_slug)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            library_knowledge_doc(pack_slug, doc, edges, |target_doc| {
                paths_by_slug.get(&target_doc).cloned()
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    LibraryKnowledgePackManifest {
        pack_id: pack_slug.to_string(),
        pack_version: "1".to_string(),
        schema_version: 1,
        root_uri: format!("library://{pack_slug}/"),
        content_hash: String::new(),
        synced_at: chrono::Utc::now().to_rfc3339(),
        docs: entries,
    }
}

pub fn upsert_library_knowledge_entry(
    pack_dir: &Path,
    pack_slug: &str,
    metadata: &DocumentSyncMeta,
    edges: &[DocumentSyncEdge],
) -> Result<()> {
    let mut manifest = load_library_knowledge_manifest(pack_dir)
        .unwrap_or_else(|| LibraryKnowledgePackManifest::library_pack(pack_slug));
    manifest.upsert_library_doc(pack_slug, metadata, edges);
    write_library_knowledge_manifest(pack_dir, &manifest)
}

pub fn remove_library_knowledge_entry(library_dir: &Path, doc: &Slug) -> Result<()> {
    let Some(mut manifest) = load_library_knowledge_manifest(library_dir) else {
        return Ok(());
    };
    if manifest.remove_document(doc) {
        write_library_knowledge_manifest(library_dir, &manifest)?;
    }
    Ok(())
}

pub fn library_knowledge_doc_relative_path(library_dir: &Path, doc: &Slug) -> Option<String> {
    load_library_knowledge_manifest(library_dir)
        .and_then(|manifest| manifest.doc_by_slug(doc).map(knowledge_doc_relative_path))
}

pub fn library_doc_relative_path(doc: &DocumentSyncMeta) -> String {
    let mut path = doc.path.clone().unwrap_or_default();
    path = path.trim_matches('/').to_string();
    if path.is_empty() {
        doc.filename.clone()
    } else {
        format!("{path}/{}", doc.filename)
    }
}

fn library_doc_path(pack_slug: &str, doc: &DocumentSyncMeta) -> String {
    let relative = library_doc_relative_path(doc);
    format!("library://{pack_slug}/{relative}")
}

fn knowledge_doc_relative_path(doc: &KnowledgeDocManifest) -> String {
    doc.source_path
        .strip_prefix("docs/")
        .unwrap_or(&doc.source_path)
        .trim_matches('/')
        .to_string()
}

fn library_knowledge_doc(
    pack_slug: &str,
    doc: &DocumentSyncMeta,
    edges: &[DocumentSyncEdge],
    resolve_target: impl Fn(Slug) -> Option<String>,
) -> KnowledgeDocManifest {
    let relative_path = library_doc_relative_path(doc);
    KnowledgeDocManifest {
        id: doc.slug.clone(),
        path: library_doc_path(pack_slug, doc),
        source_path: format!("docs/{relative_path}"),
        title: doc.title.clone().unwrap_or_else(|| doc.filename.clone()),
        summary: doc
            .summary
            .clone()
            .unwrap_or_else(|| format!("Knowledge document {relative_path}")),
        kind: parse_doc_kind(doc.kind.as_deref()),
        tags: doc.tags.clone(),
        related: edges
            .iter()
            .filter(|edge| edge.source_doc == Slug::derive(&doc.slug))
            .filter_map(|edge| {
                resolve_target(edge.target_doc.clone()).map(|target| KnowledgeDocEdge {
                    edge_type: parse_doc_edge_type(&edge.edge_type),
                    target,
                    description: edge.note.clone(),
                })
            })
            .collect(),
        updated_at: doc.updated_at.clone(),
    }
}

fn parse_doc_kind(value: Option<&str>) -> KnowledgeDocKind {
    KnowledgeDocKind::new(value.unwrap_or("reference"))
}

fn parse_doc_edge_type(value: &str) -> KnowledgeDocEdgeType {
    match value.trim() {
        "part_of" => KnowledgeDocEdgeType::PartOf,
        "defines" => KnowledgeDocEdgeType::Defines,
        "governs" => KnowledgeDocEdgeType::Governs,
        "classifies" => KnowledgeDocEdgeType::Classifies,
        "depends_on" => KnowledgeDocEdgeType::DependsOn,
        "extends" => KnowledgeDocEdgeType::Extends,
        "related_to" => KnowledgeDocEdgeType::RelatedTo,
        _ => KnowledgeDocEdgeType::References,
    }
}

impl KnowledgePack for LibraryKnowledgePack {
    fn manifest(&self) -> &dyn KnowledgePackManifest {
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
                || doc.path == path
                || normalize_library_doc_lookup(&doc.path, &self.manifest.root_uri) == normalized
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
        filter.path_prefix = filter
            .path_prefix
            .as_deref()
            .map(|prefix| normalize_library_path_prefix(prefix, &self.manifest.root_uri));
        if let Some(related_to) = filter.related_to.as_deref()
            && let Some(target) = self.read_manifest(related_to)
        {
            filter.related_to = Some(target.path.clone());
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
    if let Some(prefix) = &filter.path_prefix
        && !doc.path.starts_with(prefix)
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
                    .map(|edge_target| edge_target.id == *target || edge_target.path == *target)
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

fn normalize_library_path_prefix(value: &str, root_uri: &str) -> String {
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
    use nenjo_knowledge::KnowledgeDocKind;
    use uuid::Uuid;

    fn library_manifest() -> LibraryKnowledgePackManifest {
        LibraryKnowledgePackManifest {
            pack_id: "library-test".into(),
            pack_version: "1".into(),
            schema_version: 1,
            root_uri: "library://test/".into(),
            content_hash: String::new(),
            synced_at: String::new(),
            docs: vec![KnowledgeDocManifest {
                id: "doc-1".into(),
                path: "library://test/architecture.md".into(),
                source_path: "docs/architecture.md".into(),
                title: "Architecture".into(),
                summary: "System architecture".into(),
                kind: KnowledgeDocKind::new("reference"),
                tags: vec!["architecture".into()],
                related: Vec::new(),
                updated_at: String::new(),
            }],
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
    fn library_pack_ignores_legacy_manifest_metadata_fields() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LibraryKnowledgePack::MANIFEST_FILENAME),
            r#"{
              "pack_id": "library-test",
              "pack_version": "1",
              "schema_version": 1,
              "root_uri": "library://test/",
              "content_hash": "",
              "docs": [
                {
                  "id": "doc-1",
                  "path": "library://test/draft.md",
                  "source_path": "docs/draft.md",
                  "title": "Draft",
                  "summary": "Draft document",
                  "kind": "guide",
                  "tags": [],
                  "related": []
                }
              ]
            }"#,
        )
        .unwrap();

        let pack = LibraryKnowledgePack::load(dir.path()).unwrap();

        assert_eq!(pack.manifest().docs()[0].kind.as_str(), "guide");
        assert_eq!(pack.manifest().docs()[0].title, "Draft");
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
        let item_id = Uuid::new_v4();
        let manifest = build_library_knowledge_manifest(
            "product",
            &[DocumentSyncMeta {
                id: Some(item_id),
                pack_id: Some(Uuid::new_v4()),
                pack_slug: "product".into(),
                slug: "overview".into(),
                filename: "overview.md".into(),
                path: Some("docs".into()),
                title: Some("Overview".into()),
                kind: Some("guide".into()),
                summary: Some("Product overview".into()),
                tags: Vec::new(),
                content_type: "text/markdown".into(),
                updated_at: String::new(),
            }],
            &HashMap::new(),
        );

        assert_eq!(manifest.root_uri, "library://product/");
        assert_eq!(manifest.docs[0].path, "library://product/docs/overview.md");
    }
}
