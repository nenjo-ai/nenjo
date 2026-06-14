//! Shared knowledge pack primitives.
//!
//! Knowledge packs expose a common metadata/search/read API for
//! filesystem, or remote document sets.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub mod tools;

/// Shared read-only metadata contract for any knowledge pack manifest.
///
/// This trait intentionally covers only pack identity and document metadata.
/// Concrete pack manifests, such as project or remote manifests, should expose
/// their own sync/cache mutation methods on their concrete types.
pub trait KnowledgePackManifest: Send + Sync {
    fn pack_id(&self) -> &str;
    fn version(&self) -> &str;
    fn schema_version(&self) -> u32;
    fn root_uri(&self) -> &str;
    fn content_hash(&self) -> &str;
    fn docs(&self) -> &[KnowledgeDocManifest];

    fn read_doc_manifest(&self, selector: &str) -> Option<&KnowledgeDocManifest> {
        self.docs().iter().find(|doc| {
            doc.id == selector || doc.selector == selector || doc.source_path == selector
        })
    }
}

/// Serializable base manifest used by read-only packs and generic consumers.
///
/// Project and remote packs may deserialize into richer concrete types, but
/// their document entries should still use [`KnowledgeDocManifest`] so agents
/// and MCP tools see one metadata schema across all pack sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgePackManifestData {
    pub pack_id: String,
    pub version: String,
    pub schema_version: u32,
    pub root_uri: String,
    #[serde(default)]
    pub content_hash: String,
    pub docs: Vec<KnowledgeDocManifest>,
}

impl KnowledgePackManifest for KnowledgePackManifestData {
    fn pack_id(&self) -> &str {
        &self.pack_id
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

/// Stored metadata for one knowledge document.
///
/// Tool responses expose a slimmer projection of this type. `source_path` and
/// `updated_at` are retained for pack hydration and local sync, not for agent
/// selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocManifest {
    /// Stable document identifier within the pack.
    pub id: String,
    /// Agent-visible selector used for lookup and graph traversal.
    pub selector: String,
    /// Pack-local file path used to load the document body.
    pub source_path: String,
    /// Human-readable title.
    pub title: String,
    /// Short summary used for search and selection.
    pub summary: String,
    /// Open-ended document category normalized to a slug.
    pub kind: KnowledgeDocKind,
    /// Lightweight classification labels.
    pub tags: Vec<String>,
    /// Outbound graph edges authored on this document.
    pub related: Vec<KnowledgeDocEdge>,
    /// Sync timestamp for local library packs.
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeDocEdgeType {
    PartOf,
    Defines,
    Governs,
    Classifies,
    References,
    DependsOn,
    Extends,
    RelatedTo,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KnowledgeDocKind(String);

/// Authored outbound edge from one document to another document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocEdge {
    #[serde(rename = "type", alias = "edge_type")]
    pub edge_type: KnowledgeDocEdgeType,
    /// Target document id or path.
    pub target: String,
    /// Optional authoring note. Tool metadata omits this to keep traversal compact.
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeDocFilter {
    pub tags: Vec<String>,
    pub kind: Option<KnowledgeDocKind>,
    pub selector_prefix: Option<String>,
    pub related_to: Option<String>,
    pub edge_type: Option<KnowledgeDocEdgeType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocRead {
    pub manifest: KnowledgeDocManifest,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocNeighbor {
    /// Source document for the neighbor request.
    pub document: KnowledgeDocManifest,
    /// Resolved outbound edges from the source document.
    pub edges: Vec<KnowledgeDocNeighborEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocNeighborEdge {
    #[serde(rename = "type")]
    pub edge_type: KnowledgeDocEdgeType,
    /// Resolved target document metadata.
    pub target: KnowledgeDocManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocSearchHit {
    /// Matched document metadata.
    pub document: KnowledgeDocManifest,
    /// Simple relevance score derived from metadata matches.
    pub score: usize,
    /// Metadata fields that matched the query.
    pub matched: Vec<String>,
}

/// Runtime access to a knowledge pack's metadata and lazy document content.
pub trait KnowledgePack: Send + Sync {
    fn manifest(&self) -> &dyn KnowledgePackManifest;

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>>;

    fn list_docs(&self, filter: KnowledgeDocFilter) -> Vec<&KnowledgeDocManifest> {
        self.manifest()
            .docs()
            .iter()
            .filter(|doc| matches_filter(self, doc, &filter))
            .collect()
    }

    fn read_manifest(&self, path: &str) -> Option<&KnowledgeDocManifest> {
        self.manifest().read_doc_manifest(path)
    }

    fn read_doc(&self, path: &str) -> Option<KnowledgeDocRead> {
        let manifest = self.read_manifest(path)?.clone();
        let content = self.doc_content(&manifest)?.into_owned();
        Some(KnowledgeDocRead { manifest, content })
    }

    fn search(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        search_pack(self, query, filter)
    }

    fn neighbors(
        &self,
        path: &str,
        edge_type: Option<KnowledgeDocEdgeType>,
    ) -> Option<KnowledgeDocNeighbor> {
        let source = self.read_manifest(path)?;

        let mut edges = Vec::new();

        for edge in &source.related {
            if let Some(expected) = edge_type
                && edge.edge_type != expected
            {
                continue;
            }
            if let Some(target) = self.read_manifest(&edge.target) {
                edges.push(KnowledgeDocNeighborEdge {
                    edge_type: edge.edge_type,
                    target: target.clone(),
                });
            }
        }

        edges.sort_by(|left, right| {
            left.target
                .selector
                .cmp(&right.target.selector)
                .then_with(|| left.edge_type.as_str().cmp(right.edge_type.as_str()))
        });
        edges.dedup_by(|left, right| {
            left.edge_type == right.edge_type && left.target.selector == right.target.selector
        });

        Some(KnowledgeDocNeighbor {
            document: source.clone(),
            edges,
        })
    }
}

/// Filesystem-backed package knowledge pack loaded from an installed package
/// knowledge manifest.
#[derive(Debug, Clone)]
pub struct PackageKnowledgePack {
    content_root: PathBuf,
    selector: Option<String>,
    manifest: KnowledgePackManifestData,
}

impl PackageKnowledgePack {
    pub fn load(path: &Path, package_version: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read knowledge manifest {}", path.display()))?;
        let file: PackageKnowledgeManifestFile =
            serde_yaml::from_str(&content).context("invalid package knowledge manifest")?;
        let root_uri = file
            .manifest
            .root_uri
            .or(file.root_uri)
            .unwrap_or_else(|| format!("pkg://{}/", file.manifest.pack_id));
        let pack_id = file.manifest.pack_id;
        let docs = file
            .manifest
            .docs
            .into_iter()
            .map(|doc| doc.into_manifest(&pack_id))
            .collect();
        Ok(Self {
            content_root: path.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
            selector: file.manifest.selector.or(file.selector),
            manifest: KnowledgePackManifestData {
                pack_id,
                version: file
                    .manifest
                    .version
                    .unwrap_or_else(|| package_version.to_string()),
                schema_version: file.manifest.schema_version.unwrap_or(1),
                root_uri,
                content_hash: file.manifest.content_hash.unwrap_or_default(),
                docs,
            },
        })
    }

    pub fn selector(&self) -> Option<&str> {
        self.selector.as_deref()
    }
}

impl KnowledgePack for PackageKnowledgePack {
    fn manifest(&self) -> &dyn KnowledgePackManifest {
        &self.manifest
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>> {
        let content =
            std::fs::read_to_string(self.content_root.join(&manifest.source_path)).ok()?;
        Some(Cow::Owned(content))
    }
}

#[derive(Debug, Deserialize)]
struct PackageKnowledgeManifestFile {
    selector: Option<String>,
    root_uri: Option<String>,
    manifest: PackageKnowledgeManifestBody,
}

#[derive(Debug, Deserialize)]
struct PackageKnowledgeManifestBody {
    pack_id: String,
    selector: Option<String>,
    version: Option<String>,
    schema_version: Option<u32>,
    root_uri: Option<String>,
    content_hash: Option<String>,
    #[serde(default)]
    docs: Vec<PackageKnowledgeDoc>,
}

#[derive(Debug, Deserialize)]
struct PackageKnowledgeDoc {
    id: Option<String>,
    selector: Option<String>,
    source_path: String,
    title: String,
    summary: String,
    #[serde(default)]
    kind: KnowledgeDocKind,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    related: Vec<KnowledgeDocEdge>,
    #[serde(default)]
    updated_at: String,
}

impl PackageKnowledgeDoc {
    fn into_manifest(self, pack_id: &str) -> KnowledgeDocManifest {
        let id_hint = self.id.as_deref().unwrap_or_default();
        let selector = self
            .selector
            .unwrap_or_else(|| selector_from_source_path(&self.source_path, pack_id, id_hint));
        let id = self.id.unwrap_or_else(|| format!("{pack_id}.{selector}"));
        KnowledgeDocManifest {
            id,
            selector,
            source_path: self.source_path,
            title: self.title,
            summary: self.summary,
            kind: self.kind,
            tags: self.tags,
            related: self.related,
            updated_at: self.updated_at,
        }
    }
}

fn selector_from_source_path(source_path: &str, pack_id: &str, id: &str) -> String {
    let trimmed = source_path.strip_prefix("docs/").unwrap_or(source_path);
    let trimmed = trimmed.strip_suffix(".md").unwrap_or(trimmed);
    let selector = trimmed.replace('/', ".");
    if selector.is_empty() {
        id.strip_prefix(&format!("{pack_id}."))
            .unwrap_or(id)
            .to_string()
    } else {
        selector
    }
}

impl KnowledgeDocEdgeType {
    pub fn as_str(self) -> &'static str {
        match self {
            KnowledgeDocEdgeType::PartOf => "part_of",
            KnowledgeDocEdgeType::Defines => "defines",
            KnowledgeDocEdgeType::Governs => "governs",
            KnowledgeDocEdgeType::Classifies => "classifies",
            KnowledgeDocEdgeType::References => "references",
            KnowledgeDocEdgeType::DependsOn => "depends_on",
            KnowledgeDocEdgeType::Extends => "extends",
            KnowledgeDocEdgeType::RelatedTo => "related_to",
        }
    }
}

impl KnowledgeDocKind {
    pub fn new(value: impl AsRef<str>) -> Self {
        let value = value.as_ref().trim().to_ascii_lowercase();
        let mut slug = String::new();
        let mut last_was_separator = false;
        for ch in value.chars() {
            if ch.is_ascii_alphanumeric() {
                slug.push(ch);
                last_was_separator = false;
            } else if !last_was_separator {
                slug.push('_');
                last_was_separator = true;
            }
        }
        let slug = slug.trim_matches('_');
        if slug.is_empty() {
            Self("reference".to_string())
        } else {
            Self(slug.to_string())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for KnowledgeDocKind {
    fn default() -> Self {
        Self::new("reference")
    }
}

impl From<&str> for KnowledgeDocKind {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for KnowledgeDocKind {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl Serialize for KnowledgeDocKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for KnowledgeDocKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

fn search_pack<P: KnowledgePack + ?Sized>(
    pack: &P,
    query: &str,
    filter: KnowledgeDocFilter,
) -> Vec<KnowledgeDocSearchHit> {
    let needle = normalize(query);
    let mut hits = Vec::new();

    for manifest in pack.list_docs(filter) {
        let mut score = 0;
        let mut matched = BTreeSet::new();

        score += score_field(&needle, &manifest.id, 100, "id", &mut matched);
        score += score_field(&needle, &manifest.selector, 90, "selector", &mut matched);
        score += score_field(&needle, &manifest.title, 80, "title", &mut matched);
        score += score_field(&needle, &manifest.summary, 60, "summary", &mut matched);

        for tag in &manifest.tags {
            score += score_field(&needle, tag, 70, "tag", &mut matched);
        }

        if score > 0 || needle.is_empty() {
            hits.push(KnowledgeDocSearchHit {
                document: manifest.clone(),
                score,
                matched: matched.into_iter().collect(),
            });
        }
    }

    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.document.selector.cmp(&b.document.selector))
    });
    hits
}

fn matches_filter<P: KnowledgePack + ?Sized>(
    pack: &P,
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

fn score_field(
    needle: &str,
    haystack: &str,
    weight: usize,
    label: &str,
    matched: &mut BTreeSet<String>,
) -> usize {
    if needle.is_empty() {
        return 1;
    }
    let haystack = normalize(haystack);
    if haystack == needle {
        matched.insert(label.to_string());
        weight * 2
    } else if haystack.contains(needle) {
        matched.insert(label.to_string());
        weight
    } else {
        0
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn package_knowledge_manifest_accepts_selector_without_doc_id() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "nenjo-knowledge-package-manifest-{pid}-{unique}",
            pid = std::process::id()
        ));
        let docs_dir = dir.join("docs/domain");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(
            dir.join("manifest.yaml"),
            r#"
schema: nenjo.knowledge.v1
manifest:
  pack_id: nenjo.core
  version: 0.1.0
  docs:
    - selector: domain.nenjo
      source_path: docs/domain/nenjo.md
      title: Nenjo
      summary: Platform overview.
      kind: domain
      tags: [domain:nenjo]
      related: []
"#,
        )
        .unwrap();
        std::fs::write(docs_dir.join("nenjo.md"), "# Nenjo\n\nKnowledge content.").unwrap();

        let pack = PackageKnowledgePack::load(&dir.join("manifest.yaml"), "0.1.0").unwrap();
        let doc = pack.read_doc("domain.nenjo").unwrap();

        assert_eq!(doc.manifest.selector, "domain.nenjo");
        assert_eq!(doc.manifest.id, "nenjo.core.domain.nenjo");
        assert!(doc.content.contains("Knowledge content"));

        std::fs::remove_dir_all(dir).unwrap();
    }
}
