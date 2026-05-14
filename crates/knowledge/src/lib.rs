//! Shared knowledge pack primitives and embedded Nenjo knowledge.
//!
//! Knowledge packs expose a common metadata/search/read API for builtin,
//! project, filesystem, or remote document sets.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[cfg(feature = "nenjo")]
pub mod builtin;
pub mod tools;

/// Shared read-only metadata contract for any knowledge pack manifest.
///
/// This trait intentionally covers only pack identity and document metadata.
/// Concrete pack manifests, such as project or remote manifests, should expose
/// their own sync/cache mutation methods on their concrete types.
pub trait KnowledgePackManifest: Send + Sync {
    fn pack_id(&self) -> &str;
    fn pack_version(&self) -> &str;
    fn schema_version(&self) -> u32;
    fn root_uri(&self) -> &str;
    fn content_hash(&self) -> &str;
    fn docs(&self) -> &[KnowledgeDocManifest];

    fn read_doc_manifest(&self, path: &str) -> Option<&KnowledgeDocManifest> {
        self.docs()
            .iter()
            .find(|doc| doc.id == path || doc.virtual_path == path || doc.source_path == path)
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
    pub pack_version: String,
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

/// Shared document metadata visible through knowledge pack APIs.
///
/// `size_bytes` and `updated_at` are sync hints used by local project caches.
/// Builtin and remote manifests may leave them empty/defaulted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocManifest {
    pub id: String,
    pub virtual_path: String,
    pub source_path: String,
    pub title: String,
    pub summary: String,
    pub description: Option<String>,
    pub kind: KnowledgeDocKind,
    pub authority: KnowledgeDocAuthority,
    pub status: KnowledgeDocStatus,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
    pub related: Vec<KnowledgeDocEdge>,
    #[serde(default)]
    pub size_bytes: i64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeDocKind {
    Guide,
    Reference,
    Taxonomy,
    Domain,
    Entity,
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeDocAuthority {
    Canonical,
    Supporting,
    Pattern,
    Reference,
    Advisory,
    Example,
    Draft,
    Deprecated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeDocStatus {
    Stable,
    Draft,
    Deprecated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocEdge {
    #[serde(rename = "type", alias = "edge_type")]
    pub edge_type: KnowledgeDocEdgeType,
    pub target: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeDocFilter {
    pub tags: Vec<String>,
    pub kind: Option<KnowledgeDocKind>,
    pub authority: Option<KnowledgeDocAuthority>,
    pub status: Option<KnowledgeDocStatus>,
    pub path_prefix: Option<String>,
    pub related_to: Option<String>,
    pub edge_type: Option<KnowledgeDocEdgeType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocRead {
    pub manifest: KnowledgeDocManifest,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeDocNeighbor {
    pub target: String,
    pub edges: Vec<KnowledgeDocNeighborEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeDocNeighborEdge {
    pub edge_type: KnowledgeDocEdgeType,
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocSearchHit {
    pub id: String,
    pub virtual_path: String,
    pub title: String,
    pub summary: String,
    pub kind: KnowledgeDocKind,
    pub authority: KnowledgeDocAuthority,
    pub tags: Vec<String>,
    pub score: usize,
    pub matched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocTree {
    pub root_uri: String,
    pub entries: Vec<KnowledgeDocTreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocTreeEntry {
    pub path: String,
    pub title: String,
    pub kind: KnowledgeDocKind,
    pub tags: Vec<String>,
}

enum SearchMode {
    MetadataOnly,
    FullText,
}

/// Runtime access to a knowledge pack's metadata and lazy document content.
pub trait KnowledgePack: Send + Sync {
    fn manifest(&self) -> &dyn KnowledgePackManifest;

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>>;

    fn list_tree(&self, prefix: Option<&str>) -> KnowledgeDocTree {
        let mut entries: Vec<_> = self
            .manifest()
            .docs()
            .iter()
            .filter(|doc| {
                prefix
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
            root_uri: self.manifest().root_uri().to_string(),
            entries,
        }
    }

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

    fn search_paths(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        search_pack(self, query, filter, SearchMode::MetadataOnly)
    }

    fn search_docs(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        search_pack(self, query, filter, SearchMode::FullText)
    }

    fn neighbors(
        &self,
        path: &str,
        edge_type: Option<KnowledgeDocEdgeType>,
    ) -> Vec<KnowledgeDocNeighbor> {
        let Some(source) = self.read_manifest(path) else {
            return Vec::new();
        };

        let mut neighbors: BTreeMap<String, KnowledgeDocNeighbor> = BTreeMap::new();

        for edge in &source.related {
            if let Some(expected) = edge_type
                && edge.edge_type != expected
            {
                continue;
            }
            if let Some(target) = self.read_manifest(&edge.target) {
                push_neighbor_edge(
                    &mut neighbors,
                    target.virtual_path.clone(),
                    KnowledgeDocNeighborEdge {
                        edge_type: edge.edge_type,
                        source: source.virtual_path.clone(),
                        target: target.virtual_path.clone(),
                        note: edge.description.clone(),
                    },
                );
            }
        }

        for candidate in self.manifest().docs() {
            for edge in &candidate.related {
                let points_to_source = self
                    .read_manifest(&edge.target)
                    .map(|target| {
                        target.id == source.id || target.virtual_path == source.virtual_path
                    })
                    .unwrap_or_else(|| {
                        edge.target == source.id || edge.target == source.virtual_path
                    });
                if !points_to_source {
                    continue;
                }
                if let Some(expected) = edge_type
                    && edge.edge_type != expected
                {
                    continue;
                }
                push_neighbor_edge(
                    &mut neighbors,
                    candidate.virtual_path.clone(),
                    KnowledgeDocNeighborEdge {
                        edge_type: edge.edge_type,
                        source: candidate.virtual_path.clone(),
                        target: source.virtual_path.clone(),
                        note: edge.description.clone(),
                    },
                );
            }
        }

        neighbors.into_values().collect()
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
    pub fn as_str(self) -> &'static str {
        match self {
            KnowledgeDocKind::Guide => "guide",
            KnowledgeDocKind::Reference => "reference",
            KnowledgeDocKind::Taxonomy => "taxonomy",
            KnowledgeDocKind::Domain => "domain",
            KnowledgeDocKind::Entity => "entity",
            KnowledgeDocKind::Policy => "policy",
        }
    }
}

impl KnowledgeDocAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            KnowledgeDocAuthority::Canonical => "canonical",
            KnowledgeDocAuthority::Supporting => "supporting",
            KnowledgeDocAuthority::Pattern => "pattern",
            KnowledgeDocAuthority::Reference => "reference",
            KnowledgeDocAuthority::Advisory => "advisory",
            KnowledgeDocAuthority::Example => "example",
            KnowledgeDocAuthority::Draft => "draft",
            KnowledgeDocAuthority::Deprecated => "deprecated",
        }
    }
}

impl KnowledgeDocStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            KnowledgeDocStatus::Stable => "stable",
            KnowledgeDocStatus::Draft => "draft",
            KnowledgeDocStatus::Deprecated => "deprecated",
        }
    }
}

fn search_pack<P: KnowledgePack + ?Sized>(
    pack: &P,
    query: &str,
    filter: KnowledgeDocFilter,
    mode: SearchMode,
) -> Vec<KnowledgeDocSearchHit> {
    let needle = normalize(query);
    let mut hits = Vec::new();

    for manifest in pack.list_docs(filter) {
        let mut score = 0;
        let mut matched = BTreeSet::new();

        score += score_field(&needle, &manifest.id, 100, "id", &mut matched);
        score += score_field(
            &needle,
            &manifest.virtual_path,
            90,
            "virtual_path",
            &mut matched,
        );
        score += score_field(&needle, &manifest.title, 80, "title", &mut matched);
        score += score_field(&needle, &manifest.summary, 60, "summary", &mut matched);

        for alias in &manifest.aliases {
            score += score_field(&needle, alias, 75, "alias", &mut matched);
        }
        for tag in &manifest.tags {
            score += score_field(&needle, tag, 70, "tag", &mut matched);
        }
        for keyword in &manifest.keywords {
            score += score_field(&needle, keyword, 65, "keyword", &mut matched);
        }

        let content = match mode {
            SearchMode::MetadataOnly => None,
            SearchMode::FullText => pack.doc_content(manifest),
        };
        if let Some(content) = content.as_ref() {
            score += score_field(&needle, content, 20, "content", &mut matched);
        }

        if score > 0 || needle.is_empty() {
            hits.push(KnowledgeDocSearchHit {
                id: manifest.id.clone(),
                virtual_path: manifest.virtual_path.clone(),
                title: manifest.title.clone(),
                summary: manifest.summary.clone(),
                kind: manifest.kind,
                authority: manifest.authority,
                tags: manifest.tags.clone(),
                score,
                matched: matched.into_iter().collect(),
                content: matches!(mode, SearchMode::FullText)
                    .then(|| content.map(Cow::into_owned).unwrap_or_default()),
            });
        }
    }

    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.virtual_path.cmp(&b.virtual_path))
    });
    hits
}

fn matches_filter<P: KnowledgePack + ?Sized>(
    pack: &P,
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

fn push_neighbor_edge(
    neighbors: &mut BTreeMap<String, KnowledgeDocNeighbor>,
    neighbor_target: String,
    edge: KnowledgeDocNeighborEdge,
) {
    let neighbor =
        neighbors
            .entry(neighbor_target.clone())
            .or_insert_with(|| KnowledgeDocNeighbor {
                target: neighbor_target,
                edges: Vec::new(),
            });
    if !neighbor.edges.contains(&edge) {
        neighbor.edges.push(edge);
        neighbor.edges.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.target.cmp(&right.target))
                .then_with(|| left.edge_type.as_str().cmp(right.edge_type.as_str()))
                .then_with(|| left.note.cmp(&right.note))
        });
    }
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
