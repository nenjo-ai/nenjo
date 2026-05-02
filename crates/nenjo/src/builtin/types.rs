use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct BuiltinKnowledgePack {
    pub manifest: BuiltinKnowledgeManifest,
    pub docs: &'static [BuiltinKnowledgeDoc],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinKnowledgeManifest {
    pub pack_id: String,
    pub pack_version: String,
    pub schema_version: u32,
    pub root_uri: String,
    pub content_hash: String,
    pub docs: Vec<BuiltinDocManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinDocManifest {
    pub id: String,
    pub virtual_path: String,
    pub source_path: String,
    pub title: String,
    pub summary: String,
    pub description: Option<String>,
    pub kind: BuiltinDocKind,
    pub authority: BuiltinDocAuthority,
    pub status: BuiltinDocStatus,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
    pub related: Vec<BuiltinDocEdge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinDocEdgeType {
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
pub enum BuiltinDocKind {
    /// Explanatory or conceptual knowledge.
    Guide,

    /// Facts, constants, lookup tables, dependency rules.
    Reference,

    /// Classification systems, ontologies, categories.
    Taxonomy,

    /// High-level domain or vertical knowledge.
    Domain,

    /// Data models and structural definitions.
    Entity,

    /// Rules, policies, regulations, and compliance criteria.
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinDocAuthority {
    Canonical,
    Pattern,
    Reference,
    Advisory,
    Example,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinDocStatus {
    Stable,
    Draft,
    Deprecated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinDocEdge {
    #[serde(rename = "type", alias = "edge_type")]
    pub edge_type: BuiltinDocEdgeType,
    pub target: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuiltinKnowledgeDoc {
    pub id: &'static str,
    pub virtual_path: &'static str,
    pub content: &'static str,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuiltinDocFilter {
    pub tags: Vec<String>,
    pub kind: Option<BuiltinDocKind>,
    pub authority: Option<BuiltinDocAuthority>,
    pub status: Option<BuiltinDocStatus>,
    pub path_prefix: Option<String>,
    pub related_to: Option<String>,
    pub edge_type: Option<BuiltinDocEdgeType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinDocRead {
    pub manifest: BuiltinDocManifest,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinDocNeighbor {
    pub target: String,
    pub edges: Vec<BuiltinDocNeighborEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinDocNeighborEdge {
    pub edge_type: BuiltinDocEdgeType,
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinDocSearchHit {
    pub id: String,
    pub virtual_path: String,
    pub title: String,
    pub summary: String,
    pub kind: BuiltinDocKind,
    pub authority: BuiltinDocAuthority,
    pub tags: Vec<String>,
    pub score: usize,
    pub matched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinDocTree {
    pub root_uri: String,
    pub entries: Vec<BuiltinDocTreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinDocTreeEntry {
    pub path: String,
    pub title: String,
    pub kind: BuiltinDocKind,
    pub tags: Vec<String>,
}

impl BuiltinDocEdgeType {
    pub fn as_str(self) -> &'static str {
        match self {
            BuiltinDocEdgeType::PartOf => "part_of",
            BuiltinDocEdgeType::Defines => "defines",
            BuiltinDocEdgeType::Governs => "governs",
            BuiltinDocEdgeType::Classifies => "classifies",
            BuiltinDocEdgeType::References => "references",
            BuiltinDocEdgeType::DependsOn => "depends_on",
            BuiltinDocEdgeType::Extends => "extends",
            BuiltinDocEdgeType::RelatedTo => "related_to",
        }
    }
}
