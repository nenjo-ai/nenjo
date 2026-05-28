//! Generic knowledge tool contracts and response shaping.
//!
//! Runtime-specific crates provide pack discovery and resolution. The SDK owns
//! the stable tool schemas and result payloads so builtin, project, and remote
//! packs present the same API to agents.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use nenjo_tool_api::{Tool, ToolCategory, ToolResult, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    KnowledgeDocFilter, KnowledgeDocManifest, KnowledgeDocNeighbor, KnowledgeDocSearchHit,
    KnowledgePack, KnowledgePackManifest,
};

#[async_trait]
pub trait KnowledgeRegistry: Send + Sync {
    async fn list_packs(&self) -> Result<Vec<KnowledgePackSummary>>;
    async fn resolve_pack(&self, selector: &str) -> Result<Arc<dyn KnowledgePack>>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KnowledgeName(String);

impl KnowledgeName {
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref().trim().to_ascii_lowercase();
        if value.is_empty() {
            return Err(anyhow!("knowledge name cannot be empty"));
        }
        if value.starts_with(['_', '-']) || value.ends_with(['_', '-']) {
            return Err(anyhow!(
                "knowledge name cannot start or end with a separator"
            ));
        }
        if !value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
        {
            return Err(anyhow!(
                "knowledge name may contain only lowercase letters, numbers, underscores, and hyphens"
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn prompt_segment(&self) -> String {
        normalize_var_segment(&self.0)
    }
}

impl fmt::Display for KnowledgeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageKnowledgeName(Vec<KnowledgeName>);

impl PackageKnowledgeName {
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        let raw = value.as_ref().trim();
        let raw = raw.strip_prefix('@').unwrap_or(raw);
        let segments = raw
            .split(['.', '/'])
            .map(KnowledgeName::parse)
            .collect::<Result<Vec<_>>>()?;
        if segments.is_empty() {
            return Err(anyhow!("package knowledge name cannot be empty"));
        }
        Ok(Self(segments))
    }

    pub fn prompt_path(&self) -> String {
        self.0
            .iter()
            .map(KnowledgeName::prompt_segment)
            .collect::<Vec<_>>()
            .join(".")
    }

    pub fn selector_name(&self) -> String {
        self.0
            .iter()
            .map(KnowledgeName::as_str)
            .collect::<Vec<_>>()
            .join(".")
    }
}

impl fmt::Display for PackageKnowledgeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.selector_name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KnowledgeRef {
    Library { pack: KnowledgeName },
    Package { package: PackageKnowledgeName },
    Local { pack: KnowledgeName },
}

impl KnowledgeRef {
    pub fn library(pack: impl AsRef<str>) -> Result<Self> {
        Ok(Self::Library {
            pack: KnowledgeName::parse(pack)?,
        })
    }

    pub fn package(package: impl AsRef<str>) -> Result<Self> {
        Ok(Self::Package {
            package: PackageKnowledgeName::parse(package)?,
        })
    }

    pub fn local(pack: impl AsRef<str>) -> Result<Self> {
        Ok(Self::Local {
            pack: KnowledgeName::parse(pack)?,
        })
    }

    pub fn selector(&self) -> String {
        self.to_string()
    }

    pub fn prompt_prefix(&self) -> String {
        match self {
            Self::Library { pack } => format!("lib.{}", pack.prompt_segment()),
            Self::Package { package } => format!("pkg.{}", package.prompt_path()),
            Self::Local { pack } => format!("local.{}", pack.prompt_segment()),
        }
    }
}

impl fmt::Display for KnowledgeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Library { pack } => write!(f, "lib:{pack}"),
            Self::Package { package } => write!(f, "pkg:{package}"),
            Self::Local { pack } => write!(f, "local:{pack}"),
        }
    }
}

impl FromStr for KnowledgeRef {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if let Some(pack) = value.strip_prefix("lib:") {
            return Self::library(pack);
        }
        if let Some(pack) = value.strip_prefix("local:") {
            return Self::local(pack);
        }
        if let Some(package) = value.strip_prefix("pkg:") {
            return Self::package(package);
        }
        Err(anyhow!(
            "invalid knowledge selector '{value}'; expected lib:<pack>, pkg:<package>, or local:<pack>"
        ))
    }
}

#[derive(Clone)]
pub struct KnowledgePackEntry {
    knowledge_ref: KnowledgeRef,
    pack: Arc<dyn KnowledgePack>,
}

impl KnowledgePackEntry {
    pub fn new(knowledge_ref: KnowledgeRef, pack: impl KnowledgePack + 'static) -> Self {
        Self {
            knowledge_ref,
            pack: Arc::new(pack),
        }
    }

    pub fn library(pack_name: impl AsRef<str>, pack: impl KnowledgePack + 'static) -> Result<Self> {
        Ok(Self::new(KnowledgeRef::library(pack_name)?, pack))
    }

    pub fn package(
        package_name: impl AsRef<str>,
        pack: impl KnowledgePack + 'static,
    ) -> Result<Self> {
        Ok(Self::new(KnowledgeRef::package(package_name)?, pack))
    }

    pub fn local(pack_name: impl AsRef<str>, pack: impl KnowledgePack + 'static) -> Result<Self> {
        Ok(Self::new(KnowledgeRef::local(pack_name)?, pack))
    }

    pub fn knowledge_ref(&self) -> &KnowledgeRef {
        &self.knowledge_ref
    }

    pub fn selector(&self) -> String {
        self.knowledge_ref.selector()
    }

    pub fn pack(&self) -> &Arc<dyn KnowledgePack> {
        &self.pack
    }

    fn into_parts(self) -> (KnowledgeRef, Arc<dyn KnowledgePack>) {
        (self.knowledge_ref, self.pack)
    }
}

#[derive(Clone, Default)]
pub struct StaticKnowledgeRegistry {
    packs: Arc<HashMap<String, Arc<dyn KnowledgePack>>>,
}

impl StaticKnowledgeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_pack(mut self, selector: impl Into<String>, pack: Arc<dyn KnowledgePack>) -> Self {
        Arc::make_mut(&mut self.packs).insert(selector.into(), pack);
        self
    }

    pub fn with_entry(self, entry: KnowledgePackEntry) -> Self {
        let (knowledge_ref, pack) = entry.into_parts();
        self.with_pack(knowledge_ref.selector(), pack)
    }

    pub fn with_entries(mut self, entries: impl IntoIterator<Item = KnowledgePackEntry>) -> Self {
        for entry in entries {
            self = self.with_entry(entry);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.packs.is_empty()
    }
}

#[async_trait]
impl KnowledgeRegistry for StaticKnowledgeRegistry {
    async fn list_packs(&self) -> Result<Vec<KnowledgePackSummary>> {
        let mut packs = self
            .packs
            .iter()
            .map(|(selector, pack)| KnowledgePackSummary::new(selector, pack.manifest()))
            .collect::<Vec<_>>();
        packs.sort_by(|a, b| a.pack.cmp(&b.pack));
        Ok(packs)
    }

    async fn resolve_pack(&self, selector: &str) -> Result<Arc<dyn KnowledgePack>> {
        self.packs
            .get(selector)
            .cloned()
            .ok_or_else(|| anyhow!("unknown knowledge pack '{selector}'"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgePackSummary {
    pub pack: String,
    pub pack_id: String,
    pub version: String,
    pub root_uri: String,
    pub document_count: usize,
}

impl KnowledgePackSummary {
    pub fn new(pack: impl Into<String>, manifest: &dyn KnowledgePackManifest) -> Self {
        Self {
            pack: pack.into(),
            pack_id: manifest.pack_id().to_string(),
            version: manifest.version().to_string(),
            root_uri: manifest.root_uri().to_string(),
            document_count: manifest.docs().len(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeReadArgs {
    pub pack: String,
    pub selector: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeSearchArgs {
    pub pack: String,
    pub query: String,
    #[serde(flatten)]
    pub filter: KnowledgeFilterArgs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeNeighborArgs {
    pub pack: String,
    pub selector: String,
    pub edge_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct KnowledgeFilterArgs {
    #[serde(default)]
    pub tags: Vec<String>,
    pub kind: Option<String>,
    pub selector_prefix: Option<String>,
    pub related_to: Option<String>,
    pub edge_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocMetadataResult {
    /// Stable document identifier within the pack.
    pub id: String,
    /// Agent-visible selector used for lookup and traversal.
    pub selector: String,
    /// Human-readable title.
    pub title: String,
    /// Short summary used for selection.
    pub summary: String,
    /// Open-ended document category.
    pub kind: String,
    /// Lightweight classification labels.
    pub tags: Vec<String>,
    /// Outbound graph edge pointers. Call `list_knowledge_neighbors` to expand them.
    pub related: Vec<KnowledgeDocRelatedResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocRelatedResult {
    #[serde(rename = "type")]
    pub edge_type: String,
    /// Target document id or path.
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocReadResult {
    /// Slim document metadata.
    pub document: KnowledgeDocMetadataResult,
    /// Full document body.
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocSearchResult {
    /// Slim document metadata for the matched document.
    pub document: KnowledgeDocMetadataResult,
    /// Simple relevance score derived from metadata matches.
    pub score: usize,
    /// Metadata fields that matched the query.
    pub matched: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocNeighborsResult {
    /// Source document metadata.
    pub document: KnowledgeDocMetadataResult,
    /// Resolved outbound neighbor edges.
    pub edges: Vec<KnowledgeDocNeighborEdgeResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocNeighborEdgeResult {
    #[serde(rename = "type")]
    pub edge_type: String,
    /// Resolved target document metadata.
    pub target: KnowledgeDocMetadataResult,
}

pub fn knowledge_filter(filter: KnowledgeFilterArgs) -> Result<KnowledgeDocFilter> {
    Ok(KnowledgeDocFilter {
        tags: filter.tags,
        kind: parse_knowledge_enum(filter.kind)?,
        selector_prefix: filter.selector_prefix,
        related_to: filter.related_to,
        edge_type: parse_knowledge_enum(filter.edge_type)?,
    })
}

pub fn parse_knowledge_enum<T>(value: Option<String>) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    value
        .map(|value| {
            serde_json::from_value(serde_json::Value::String(value.to_lowercase()))
                .with_context(|| "invalid knowledge filter value")
        })
        .transpose()
}

pub fn knowledge_document_metadata(doc: &KnowledgeDocManifest) -> KnowledgeDocMetadataResult {
    KnowledgeDocMetadataResult {
        id: doc.id.clone(),
        selector: doc.selector.clone(),
        title: doc.title.clone(),
        summary: doc.summary.clone(),
        kind: doc.kind.as_str().to_string(),
        tags: doc.tags.clone(),
        related: doc
            .related
            .iter()
            .map(|edge| KnowledgeDocRelatedResult {
                edge_type: edge.edge_type.as_str().to_string(),
                target: edge.target.clone(),
            })
            .collect(),
    }
}

pub fn knowledge_search_result(hit: KnowledgeDocSearchHit) -> KnowledgeDocSearchResult {
    KnowledgeDocSearchResult {
        document: knowledge_document_metadata(&hit.document),
        score: hit.score,
        matched: hit.matched,
    }
}

pub fn knowledge_neighbors_result(neighbors: KnowledgeDocNeighbor) -> KnowledgeDocNeighborsResult {
    KnowledgeDocNeighborsResult {
        document: knowledge_document_metadata(&neighbors.document),
        edges: neighbors
            .edges
            .into_iter()
            .map(|edge| KnowledgeDocNeighborEdgeResult {
                edge_type: edge.edge_type.as_str().to_string(),
                target: knowledge_document_metadata(&edge.target),
            })
            .collect(),
    }
}

pub fn knowledge_document_metadata_vars(
    knowledge_ref: &KnowledgeRef,
    pack: &dyn KnowledgePack,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for doc in pack.manifest().docs() {
        let metadata = doc_metadata(doc);
        vars.insert(
            knowledge_document_var_key(knowledge_ref, doc),
            metadata.clone(),
        );
        for key in knowledge_document_alias_var_keys(knowledge_ref, doc) {
            vars.entry(key).or_insert_with(|| metadata.clone());
        }
    }
    vars
}

pub fn knowledge_pack_prompt_vars(
    knowledge_ref: &KnowledgeRef,
    pack: &dyn KnowledgePack,
) -> HashMap<String, String> {
    let prefix = knowledge_ref.prompt_prefix();
    let mut vars = HashMap::new();
    vars.insert(prefix, knowledge_pack_summary(knowledge_ref, pack));
    vars.extend(knowledge_document_metadata_vars(knowledge_ref, pack));
    vars
}

pub fn knowledge_pack_summary(knowledge_ref: &KnowledgeRef, pack: &dyn KnowledgePack) -> String {
    let manifest = pack.manifest();
    let selector = knowledge_ref.selector();
    let namespace = match knowledge_ref {
        KnowledgeRef::Library { .. } => "lib",
        KnowledgeRef::Package { .. } => "pkg",
        KnowledgeRef::Local { .. } => "local",
    };
    let ctx = KnowledgePackSummaryContext {
        selector: selector.as_str(),
        namespace,
        name: manifest.pack_id(),
        root: manifest.root_uri(),
        usage: "Use the knowledge tools to search, inspect metadata, expand graph neighbors, and read documents from this pack when relevant.",
        docs: manifest
            .docs()
            .iter()
            .map(|doc| KnowledgeDocumentSummaryContext {
                selector: doc.selector.as_str(),
                id: doc.id.as_str(),
                kind: doc.kind.as_str(),
                title: doc.title.as_str(),
                summary: doc.summary.as_str(),
                related: doc
                    .related
                    .iter()
                    .map(|edge| KnowledgeDocumentRelatedSummaryContext {
                        edge_type: edge.edge_type.as_str(),
                        target: edge.target.as_str(),
                    })
                    .collect(),
            })
            .collect(),
    };

    nenjo_xml::to_xml_pretty(&ctx, 2)
}

#[derive(Debug, Serialize)]
#[serde(rename = "knowledge_pack")]
struct KnowledgePackSummaryContext<'a> {
    #[serde(rename = "@selector")]
    selector: &'a str,
    #[serde(rename = "@namespace")]
    namespace: &'a str,
    #[serde(rename = "@name")]
    name: &'a str,
    #[serde(rename = "@root")]
    root: &'a str,
    usage: &'a str,
    #[serde(rename = "doc")]
    docs: Vec<KnowledgeDocumentSummaryContext<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename = "doc")]
struct KnowledgeDocumentSummaryContext<'a> {
    #[serde(rename = "@selector")]
    selector: &'a str,
    #[serde(rename = "@id")]
    id: &'a str,
    #[serde(rename = "@kind")]
    kind: &'a str,
    title: &'a str,
    summary: &'a str,
    #[serde(rename = "related", skip_serializing_if = "Vec::is_empty", default)]
    related: Vec<KnowledgeDocumentRelatedSummaryContext<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename = "related")]
struct KnowledgeDocumentRelatedSummaryContext<'a> {
    #[serde(rename = "@type")]
    edge_type: &'a str,
    #[serde(rename = "@target")]
    target: &'a str,
}

pub fn knowledge_document_var_key(
    knowledge_ref: &KnowledgeRef,
    doc: &KnowledgeDocManifest,
) -> String {
    let pack_prefix = knowledge_ref.prompt_prefix();
    let selector = prompt_doc_selector(doc);
    let path = selector
        .strip_suffix(".md")
        .unwrap_or(selector.as_str())
        .split(['.', '/'])
        .filter(|segment| !segment.is_empty())
        .map(normalize_var_segment)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(".");
    if path.is_empty() {
        pack_prefix
    } else {
        format!("{pack_prefix}.{path}")
    }
}

fn knowledge_document_alias_var_keys(
    knowledge_ref: &KnowledgeRef,
    doc: &KnowledgeDocManifest,
) -> Vec<String> {
    let mut keys = Vec::new();
    let pack_prefix = knowledge_ref.prompt_prefix();
    let selector = prompt_doc_selector(doc);
    let Some((parent, _leaf)) = selector
        .strip_suffix(".md")
        .unwrap_or(selector.as_str())
        .rsplit_once(['.', '/'])
    else {
        return keys;
    };
    let parent = parent
        .split(['.', '/'])
        .filter(|segment| !segment.is_empty())
        .map(normalize_var_segment)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(".");

    if let Some(stripped) = doc.id.strip_prefix("nenjo.") {
        let id_segments = stripped
            .split('.')
            .map(normalize_var_segment)
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if id_segments.len() >= 2
            && id_segments
                .first()
                .is_some_and(|segment| segment == &parent)
        {
            let basename = id_segments[1..].join("_");
            keys.push(format!("{pack_prefix}.{parent}.nenjo_{basename}"));
        }
    }

    keys
}

fn normalize_var_segment(segment: &str) -> String {
    let mut normalized = String::new();
    let mut last_was_underscore = false;
    for ch in segment.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
            last_was_underscore = false;
        } else if !last_was_underscore {
            normalized.push('_');
            last_was_underscore = true;
        }
    }
    normalized.trim_matches('_').to_string()
}

#[derive(Debug, Serialize)]
#[serde(rename = "knowledge_doc")]
struct KnowledgeDocMetadataContext<'a> {
    #[serde(rename = "@selector")]
    selector: &'a str,
    #[serde(rename = "@title")]
    title: &'a str,
    #[serde(rename = "@kind")]
    kind: &'a str,
    summary: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tags: Vec<&'a str>,
}

fn doc_metadata(doc: &KnowledgeDocManifest) -> String {
    let selector = prompt_doc_selector(doc);
    let ctx = KnowledgeDocMetadataContext {
        selector: &selector,
        title: &doc.title,
        summary: &doc.summary,
        kind: doc.kind.as_str(),
        tags: doc.tags.iter().map(String::as_str).collect(),
    };
    nenjo_xml::to_xml_pretty(&ctx, 2)
}

fn prompt_doc_selector(doc: &KnowledgeDocManifest) -> String {
    if doc.selector.starts_with("library://") {
        doc.selector
            .splitn(4, '/')
            .nth(3)
            .unwrap_or(&doc.selector)
            .to_string()
    } else {
        doc.selector.clone()
    }
}

fn pack_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Knowledge pack selector: lib:<pack>, pkg:<package>, or local:<pack>."
    })
}

fn knowledge_filter_schema(
    extra_properties: Option<serde_json::Value>,
    required: &[&str],
) -> serde_json::Value {
    let mut properties = json!({
        "pack": pack_schema(),
        "tags": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional tags that all returned docs must have"
        },
        "kind": {
            "type": "string",
            "description": "Optional kind filter such as guide or reference"
        },
        "selector_prefix": {
            "type": "string",
            "description": "Optional virtual or pack-relative selector prefix"
        },
        "related_to": {
            "type": "string",
            "description": "Optional selector of a document this result must be related to"
        },
        "edge_type": {
            "type": "string",
            "description": "Optional relationship type used with related_to or neighbors"
        }
    });

    if let Some(extra) = extra_properties
        && let Some(map) = properties.as_object_mut()
        && let Some(extra_map) = extra.as_object()
    {
        for (key, value) in extra_map {
            map.insert(key.clone(), value.clone());
        }
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn knowledge_lookup_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "pack": pack_schema(),
            "selector": {
                "type": "string",
                "description": "Document selector or id within the selected pack"
            }
        },
        "required": ["pack", "selector"],
        "additionalProperties": false
    })
}

pub fn knowledge_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_knowledge_packs".into(),
            description: "List locally available knowledge packs. Use this before reading or searching knowledge when you need to discover available sources.".into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "read_knowledge_doc".into(),
            description: "Read one full document body from a knowledge pack by path.".into(),
            parameters: knowledge_lookup_schema(),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "search_knowledge".into(),
            description: "Search a knowledge pack and return candidate document metadata without loading document bodies.".into(),
            parameters: knowledge_filter_schema(
                Some(json!({
                    "query": {
                        "type": "string",
                        "description": "Search query, path, title, tag, or summary"
                    }
                })),
                &["pack", "query"],
            ),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_knowledge_neighbors".into(),
            description: "List outbound graph neighbors for one document in a knowledge pack.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pack": pack_schema(),
                    "selector": {
                        "type": "string",
                        "description": "Document selector or id within the selected pack"
                    },
                    "edge_type": {
                        "type": "string",
                        "description": "Optional relationship type filter such as references or depends_on"
                    }
                },
                "required": ["pack", "selector"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
    ]
}

pub fn knowledge_toolbelt(registry: Arc<dyn KnowledgeRegistry>) -> Vec<Arc<dyn Tool>> {
    knowledge_tools()
        .into_iter()
        .map(|spec| Arc::new(KnowledgeTool::new(spec, registry.clone())) as Arc<dyn Tool>)
        .collect()
}

struct KnowledgeTool {
    spec: ToolSpec,
    registry: Arc<dyn KnowledgeRegistry>,
}

impl KnowledgeTool {
    fn new(spec: ToolSpec, registry: Arc<dyn KnowledgeRegistry>) -> Self {
        Self { spec, registry }
    }
}

#[async_trait]
impl Tool for KnowledgeTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }

    fn category(&self) -> ToolCategory {
        self.spec.category
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let output = match self.name() {
            "list_knowledge_packs" => serde_json::to_value(self.registry.list_packs().await?)?,
            "read_knowledge_doc" => {
                let args: KnowledgeReadArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let doc = pack.read_doc(&args.selector).ok_or_else(|| {
                    anyhow!(
                        "knowledge document '{}' not found in pack '{}'",
                        args.selector,
                        args.pack
                    )
                })?;
                serde_json::to_value(KnowledgeDocReadResult {
                    document: knowledge_document_metadata(&doc.manifest),
                    content: doc.content,
                })?
            }
            "search_knowledge" => {
                let args: KnowledgeSearchArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let filter = knowledge_filter(args.filter)?;
                let hits = pack
                    .search(&args.query, filter)
                    .into_iter()
                    .map(knowledge_search_result)
                    .collect::<Vec<_>>();
                serde_json::to_value(hits)?
            }
            "list_knowledge_neighbors" => {
                let args: KnowledgeNeighborArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let edge_type = parse_knowledge_enum(args.edge_type)?;
                let neighbors = pack.neighbors(&args.selector, edge_type).ok_or_else(|| {
                    anyhow!(
                        "knowledge document '{}' not found in pack '{}'",
                        args.selector,
                        args.pack
                    )
                })?;
                serde_json::to_value(knowledge_neighbors_result(neighbors))?
            }
            name => return Err(anyhow!("unknown knowledge tool '{name}'")),
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{
        KnowledgeDocReadResult, KnowledgeRef, knowledge_document_var_key,
        knowledge_neighbors_result, knowledge_search_result, knowledge_tools,
    };
    use crate::{
        KnowledgeDocEdge, KnowledgeDocEdgeType, KnowledgeDocKind, KnowledgeDocManifest,
        KnowledgePack, KnowledgePackManifest, KnowledgePackManifestData,
    };
    use serde_json::json;

    struct TestPack {
        manifest: KnowledgePackManifestData,
    }

    impl KnowledgePack for TestPack {
        fn manifest(&self) -> &dyn KnowledgePackManifest {
            &self.manifest
        }

        fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>> {
            Some(Cow::Owned(format!("body for {}", manifest.title)))
        }
    }

    fn test_doc(
        id: &str,
        path: &str,
        title: &str,
        related: Vec<KnowledgeDocEdge>,
    ) -> KnowledgeDocManifest {
        KnowledgeDocManifest {
            id: id.into(),
            selector: path.into(),
            source_path: path.trim_start_matches("library://test/").into(),
            title: title.into(),
            summary: format!("{title} summary"),
            kind: KnowledgeDocKind::new("routing-guide"),
            tags: vec!["core".into()],
            related,
            updated_at: String::new(),
        }
    }

    fn test_pack() -> TestPack {
        TestPack {
            manifest: KnowledgePackManifestData {
                pack_id: "test".into(),
                version: "1".into(),
                schema_version: 1,
                root_uri: "library://test/".into(),
                content_hash: String::new(),
                docs: vec![
                    test_doc(
                        "root",
                        "library://test/root.md",
                        "Root",
                        vec![KnowledgeDocEdge {
                            edge_type: KnowledgeDocEdgeType::DependsOn,
                            target: "library://test/leaf.md".into(),
                            description: Some("root to leaf".into()),
                        }],
                    ),
                    test_doc(
                        "leaf",
                        "library://test/leaf.md",
                        "Leaf",
                        vec![KnowledgeDocEdge {
                            edge_type: KnowledgeDocEdgeType::References,
                            target: "library://test/root.md".into(),
                            description: Some("reverse edge".into()),
                        }],
                    ),
                ],
            },
        }
    }

    #[test]
    fn default_knowledge_tool_registry_exposes_graph_first_tools_only() {
        let names = knowledge_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "list_knowledge_packs",
                "read_knowledge_doc",
                "search_knowledge",
                "list_knowledge_neighbors",
            ]
        );
    }

    #[test]
    fn pack_prompt_summary_includes_compact_related_edges() {
        let pack = TestPack {
            manifest: KnowledgePackManifestData {
                pack_id: "test".into(),
                version: "1".into(),
                schema_version: 1,
                root_uri: "file:///tmp/test/".into(),
                content_hash: String::new(),
                docs: vec![
                    test_doc(
                        "root",
                        "docs/root.md",
                        "Root",
                        vec![KnowledgeDocEdge {
                            edge_type: KnowledgeDocEdgeType::DependsOn,
                            target: "docs/leaf.md".into(),
                            description: Some("root to leaf".into()),
                        }],
                    ),
                    test_doc(
                        "leaf",
                        "docs/leaf.md",
                        "Leaf",
                        vec![KnowledgeDocEdge {
                            edge_type: KnowledgeDocEdgeType::References,
                            target: "docs/root.md".into(),
                            description: Some("reverse edge".into()),
                        }],
                    ),
                ],
            },
        };
        let knowledge_ref = KnowledgeRef::local("test").unwrap();
        let summary = super::knowledge_pack_summary(&knowledge_ref, &pack);

        assert!(summary.contains(r#"selector="local:test""#));
        assert!(summary.contains(r#"<related type="depends_on" target="docs/leaf.md""#));
        assert!(summary.contains(r#"<related type="references" target="docs/root.md""#));
        assert!(!summary.contains("root to leaf"));
        assert!(!summary.contains("reverse edge"));
    }

    #[test]
    fn neighbor_traversal_returns_outbound_edges_with_slim_target_metadata() {
        let pack = test_pack();
        let result = pack
            .neighbors("root", None)
            .map(knowledge_neighbors_result)
            .expect("root neighbors");
        let value = serde_json::to_value(result).unwrap();

        assert_eq!(value["document"]["selector"], "library://test/root.md");
        assert_eq!(value["document"]["related"][0]["type"], "depends_on");
        assert_eq!(
            value["document"]["related"][0]["target"],
            "library://test/leaf.md"
        );
        assert_eq!(value["edges"].as_array().unwrap().len(), 1);
        assert_eq!(value["edges"][0]["type"], "depends_on");
        assert_eq!(
            value["edges"][0]["target"]["selector"],
            "library://test/leaf.md"
        );
        assert_eq!(value["edges"][0]["target"]["kind"], "routing_guide");
        assert!(value["edges"][0]["target"].get("source_path").is_none());
        assert!(value["edges"][0].get("note").is_none());
    }

    #[test]
    fn search_returns_slim_metadata_without_content() {
        let pack = test_pack();
        let value = serde_json::to_value(
            pack.search("Leaf", Default::default())
                .into_iter()
                .map(knowledge_search_result)
                .collect::<Vec<_>>(),
        )
        .unwrap();

        assert_eq!(value[0]["document"]["selector"], "library://test/leaf.md");
        assert_eq!(value[0]["document"]["related"][0]["type"], "references");
        assert_eq!(
            value[0]["document"]["related"][0]["target"],
            "library://test/root.md"
        );
        assert!(
            value[0]["matched"]
                .as_array()
                .unwrap()
                .contains(&json!("title"))
        );
        assert!(
            !value[0]["matched"]
                .as_array()
                .unwrap()
                .contains(&json!("content"))
        );
        assert!(value[0].get("content").is_none());
        assert!(value[0]["document"].get("aliases").is_none());
    }

    #[test]
    fn read_knowledge_doc_result_keeps_full_content_explicit() {
        let pack = test_pack();
        let doc = pack.read_doc("leaf").expect("leaf doc");
        let value = serde_json::to_value(KnowledgeDocReadResult {
            document: super::knowledge_document_metadata(&doc.manifest),
            content: doc.content,
        })
        .unwrap();

        assert_eq!(value["document"]["selector"], "library://test/leaf.md");
        assert_eq!(
            value["document"]["related"][0]["target"],
            "library://test/root.md"
        );
        assert_eq!(value["content"], "body for Leaf");
    }

    #[test]
    fn library_knowledge_uses_lib_template_namespace() {
        let knowledge_ref = KnowledgeRef::library("product-docs").unwrap();
        assert_eq!(knowledge_ref.selector(), "lib:product-docs");
        assert_eq!(knowledge_ref.prompt_prefix(), "lib.product_docs");
    }

    #[test]
    fn pkg_knowledge_uses_package_template_namespace() {
        let knowledge_ref = KnowledgeRef::package("@nenjo/core").unwrap();
        assert_eq!(knowledge_ref.selector(), "pkg:nenjo.core");
        assert_eq!(knowledge_ref.prompt_prefix(), "pkg.nenjo.core");
    }

    #[test]
    fn pkg_knowledge_document_vars_use_package_relative_paths() {
        let knowledge_ref = KnowledgeRef::package("nenjo.core").unwrap();
        let doc = KnowledgeDocManifest {
            id: "nenjo.resources.agents".into(),
            selector: "resources.agents".into(),
            source_path: "docs/resources/agents.md".into(),
            title: "Agents".into(),
            summary: String::new(),
            kind: KnowledgeDocKind::new("guide"),
            tags: Vec::new(),
            related: Vec::new(),
            updated_at: String::new(),
        };

        assert_eq!(
            knowledge_document_var_key(&knowledge_ref, &doc),
            "pkg.nenjo.core.resources.agents"
        );
    }
}
