use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use nenjo::client::{DocumentSyncEdge, DocumentSyncMeta};
use nenjo_knowledge::{
    KnowledgeDocAuthority, KnowledgeDocEdge, KnowledgeDocEdgeType, KnowledgeDocFilter,
    KnowledgeDocKind, KnowledgeDocManifest, KnowledgeDocStatus, KnowledgeDocTree,
    KnowledgeDocTreeEntry, KnowledgePack, KnowledgePackManifest,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    pub fn library_pack(_pack_id: Uuid, pack_slug: &str) -> Self {
        Self {
            pack_id: format!("library-knowledge-{pack_slug}"),
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

    pub fn remove_document(&mut self, document_id: Uuid) -> bool {
        let doc_id = document_id.to_string();
        let removed_virtual_paths: std::collections::HashSet<String> = self
            .docs
            .iter()
            .filter(|doc| doc.id == doc_id)
            .map(|doc| doc.virtual_path.clone())
            .collect();
        let original_len = self.docs.len();
        self.docs.retain(|doc| doc.id != doc_id);
        if self.docs.len() == original_len {
            return false;
        }
        for doc in &mut self.docs {
            doc.related
                .retain(|edge| !removed_virtual_paths.contains(&edge.target));
        }
        self.touch();
        true
    }

    pub fn doc_by_id(&self, document_id: Uuid) -> Option<&KnowledgeDocManifest> {
        let doc_id = document_id.to_string();
        self.docs.iter().find(|doc| doc.id == doc_id)
    }

    pub fn upsert_library_item(
        &mut self,
        pack_slug: &str,
        metadata: &DocumentSyncMeta,
        edges: &[DocumentSyncEdge],
    ) {
        let virtual_path = library_item_virtual_path(pack_slug, metadata);
        let next = library_knowledge_doc(pack_slug, metadata, edges, |target_id| {
            self.docs
                .iter()
                .find(|doc| doc.id == target_id.to_string())
                .map(|doc| doc.virtual_path.clone())
        });
        if let Some(pos) = self.docs.iter().position(|doc| doc.id == next.id) {
            self.docs[pos] = next;
        } else {
            self.docs.push(next);
        }
        for doc in &mut self.docs {
            doc.related.retain(|edge| edge.target != virtual_path);
        }
        for edge in edges {
            if edge.target_item_id == metadata.id
                && let Some(source) = self
                    .docs
                    .iter_mut()
                    .find(|doc| doc.id == edge.source_item_id.to_string())
            {
                let target = virtual_path.clone();
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
        self.docs
            .sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
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
    _pack_id: Uuid,
    pack_slug: &str,
    docs: &[DocumentSyncMeta],
    edges_by_doc: &HashMap<Uuid, Vec<DocumentSyncEdge>>,
) -> LibraryKnowledgePackManifest {
    let virtual_paths_by_id: HashMap<Uuid, String> = docs
        .iter()
        .map(|doc| (doc.id, library_item_virtual_path(pack_slug, doc)))
        .collect();
    let mut entries = docs
        .iter()
        .map(|doc| {
            let edges = edges_by_doc.get(&doc.id).map(Vec::as_slice).unwrap_or(&[]);
            library_knowledge_doc(pack_slug, doc, edges, |target_id| {
                virtual_paths_by_id.get(&target_id).cloned()
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
    LibraryKnowledgePackManifest {
        pack_id: format!("library-knowledge-{pack_slug}"),
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
    pack_id: Uuid,
    pack_slug: &str,
    metadata: &DocumentSyncMeta,
    edges: &[DocumentSyncEdge],
) -> Result<()> {
    let mut manifest = load_library_knowledge_manifest(pack_dir)
        .unwrap_or_else(|| LibraryKnowledgePackManifest::library_pack(pack_id, pack_slug));
    manifest.upsert_library_item(pack_slug, metadata, edges);
    write_library_knowledge_manifest(pack_dir, &manifest)
}

pub fn remove_library_knowledge_entry(library_dir: &Path, document_id: Uuid) -> Result<()> {
    let Some(mut manifest) = load_library_knowledge_manifest(library_dir) else {
        return Ok(());
    };
    if manifest.remove_document(document_id) {
        write_library_knowledge_manifest(library_dir, &manifest)?;
    }
    Ok(())
}

pub fn library_knowledge_item_relative_path(
    library_dir: &Path,
    document_id: Uuid,
) -> Option<String> {
    load_library_knowledge_manifest(library_dir).and_then(|manifest| {
        manifest
            .doc_by_id(document_id)
            .map(knowledge_doc_relative_path)
    })
}

pub fn library_item_relative_path(doc: &DocumentSyncMeta) -> String {
    let mut path = doc.path.clone().unwrap_or_default();
    path = path.trim_matches('/').to_string();
    if path.is_empty() {
        doc.filename.clone()
    } else {
        format!("{path}/{}", doc.filename)
    }
}

fn library_item_virtual_path(pack_slug: &str, doc: &DocumentSyncMeta) -> String {
    let relative = library_item_relative_path(doc);
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
    resolve_target: impl Fn(Uuid) -> Option<String>,
) -> KnowledgeDocManifest {
    let relative_path = library_item_relative_path(doc);
    KnowledgeDocManifest {
        id: doc.id.to_string(),
        virtual_path: library_item_virtual_path(pack_slug, doc),
        source_path: format!("docs/{relative_path}"),
        title: doc.title.clone().unwrap_or_else(|| doc.filename.clone()),
        summary: doc
            .summary
            .clone()
            .unwrap_or_else(|| format!("Knowledge item {relative_path}")),
        description: None,
        kind: parse_doc_kind(doc.kind.as_deref()),
        authority: parse_doc_authority(doc.authority.as_deref()),
        status: parse_doc_status(doc.status.as_deref()),
        tags: doc.tags.clone(),
        aliases: doc.aliases.clone(),
        keywords: doc.keywords.clone(),
        related: edges
            .iter()
            .filter(|edge| edge.source_item_id == doc.id)
            .filter_map(|edge| {
                resolve_target(edge.target_item_id).map(|target| KnowledgeDocEdge {
                    edge_type: parse_doc_edge_type(&edge.edge_type),
                    target,
                    description: edge.note.clone(),
                })
            })
            .collect(),
        size_bytes: doc.size_bytes,
        updated_at: doc.updated_at.clone(),
    }
}

fn parse_doc_kind(value: Option<&str>) -> KnowledgeDocKind {
    match value.unwrap_or("reference").trim() {
        "guide" => KnowledgeDocKind::Guide,
        "taxonomy" => KnowledgeDocKind::Taxonomy,
        "domain" => KnowledgeDocKind::Domain,
        "entity" => KnowledgeDocKind::Entity,
        "policy" => KnowledgeDocKind::Policy,
        _ => KnowledgeDocKind::Reference,
    }
}

fn parse_doc_authority(value: Option<&str>) -> KnowledgeDocAuthority {
    match value.unwrap_or("reference").trim() {
        "canonical" => KnowledgeDocAuthority::Canonical,
        "supporting" => KnowledgeDocAuthority::Supporting,
        "pattern" => KnowledgeDocAuthority::Pattern,
        "advisory" => KnowledgeDocAuthority::Advisory,
        "example" => KnowledgeDocAuthority::Example,
        "draft" => KnowledgeDocAuthority::Draft,
        "deprecated" => KnowledgeDocAuthority::Deprecated,
        _ => KnowledgeDocAuthority::Reference,
    }
}

fn parse_doc_status(value: Option<&str>) -> KnowledgeDocStatus {
    match value.unwrap_or("stable").trim() {
        "draft" => KnowledgeDocStatus::Draft,
        "deprecated" => KnowledgeDocStatus::Deprecated,
        _ => KnowledgeDocStatus::Stable,
    }
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
        let normalized = normalize_library_item_lookup(path, &self.manifest.root_uri);
        self.manifest.docs.iter().find(|doc| {
            doc.id == path
                || doc.virtual_path == path
                || normalize_library_item_lookup(&doc.virtual_path, &self.manifest.root_uri)
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
        filter.path_prefix = filter
            .path_prefix
            .as_deref()
            .map(|prefix| normalize_library_path_prefix(prefix, &self.manifest.root_uri));
        if let Some(related_to) = filter.related_to.as_deref()
            && let Some(target) = self.read_manifest(related_to)
        {
            filter.related_to = Some(target.virtual_path.clone());
        }
        self.manifest
            .docs
            .iter()
            .filter(|doc| matches_library_filter(self, doc, &filter))
            .collect()
    }

    fn list_tree(&self, prefix: Option<&str>) -> KnowledgeDocTree {
        let prefix =
            prefix.map(|prefix| normalize_library_path_prefix(prefix, &self.manifest.root_uri));
        let mut entries: Vec<_> = self
            .manifest
            .docs
            .iter()
            .filter(|doc| {
                prefix
                    .as_deref()
                    .map(|prefix| doc.virtual_path.starts_with(prefix))
                    .unwrap_or(true)
            })
            .map(|doc| KnowledgeDocTreeEntry {
                path: doc.virtual_path.clone(),
                title: doc.title.clone(),
                kind: doc.kind,
                tags: doc.tags.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        KnowledgeDocTree {
            root_uri: self.manifest.root_uri.clone(),
            entries,
        }
    }
}

fn matches_library_filter(
    pack: &LibraryKnowledgePack,
    doc: &KnowledgeDocManifest,
    filter: &KnowledgeDocFilter,
) -> bool {
    if let Some(kind) = filter.kind
        && doc.kind != kind
    {
        return false;
    }
    if let Some(authority) = filter.authority
        && doc.authority != authority
    {
        return false;
    }
    if let Some(status) = filter.status
        && doc.status != status
    {
        return false;
    }
    if let Some(prefix) = &filter.path_prefix
        && !doc.virtual_path.starts_with(prefix)
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
                    .map(|edge_target| {
                        edge_target.id == *target || edge_target.virtual_path == *target
                    })
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

fn normalize_library_item_lookup(value: &str, root_uri: &str) -> String {
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
    use nenjo_knowledge::{KnowledgeDocAuthority, KnowledgeDocKind, KnowledgeDocStatus};

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
                virtual_path: "library://test/architecture.md".into(),
                source_path: "docs/architecture.md".into(),
                title: "Architecture".into(),
                summary: "System architecture".into(),
                description: None,
                kind: KnowledgeDocKind::Reference,
                authority: KnowledgeDocAuthority::Reference,
                status: KnowledgeDocStatus::Stable,
                tags: vec!["architecture".into()],
                aliases: vec!["architecture.md".into()],
                keywords: vec!["system".into()],
                related: Vec::new(),
                size_bytes: 0,
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

        let hits = pack.search_paths("Architecture", Default::default());
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.is_none());

        let doc = pack.read_doc("library://test/architecture.md").unwrap();
        assert_eq!(doc.content, "# Architecture");
    }

    #[test]
    fn library_pack_accepts_library_item_authority_values() {
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
                  "virtual_path": "library://test/draft.md",
                  "source_path": "docs/draft.md",
                  "title": "Draft",
                  "summary": "Draft document",
                  "description": null,
                  "kind": "guide",
                  "authority": "draft",
                  "status": "draft",
                  "tags": [],
                  "aliases": [],
                  "keywords": [],
                  "related": []
                }
              ]
            }"#,
        )
        .unwrap();

        let pack = LibraryKnowledgePack::load(dir.path()).unwrap();

        assert_eq!(
            pack.manifest().docs()[0].authority,
            KnowledgeDocAuthority::Draft
        );
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
    fn library_knowledge_manifests_use_library_virtual_paths() {
        let item_id = Uuid::new_v4();
        let manifest = build_library_knowledge_manifest(
            Uuid::new_v4(),
            "product",
            &[DocumentSyncMeta {
                id: item_id,
                pack_id: Uuid::new_v4(),
                slug: "overview".into(),
                filename: "overview.md".into(),
                path: Some("docs".into()),
                title: Some("Overview".into()),
                kind: Some("guide".into()),
                authority: Some("canonical".into()),
                summary: Some("Product overview".into()),
                status: Some("stable".into()),
                tags: Vec::new(),
                aliases: Vec::new(),
                keywords: Vec::new(),
                content_type: "text/markdown".into(),
                size_bytes: 0,
                updated_at: String::new(),
            }],
            &HashMap::new(),
        );

        assert_eq!(manifest.root_uri, "library://product/");
        assert_eq!(
            manifest.docs[0].virtual_path,
            "library://product/docs/overview.md"
        );
    }
}
