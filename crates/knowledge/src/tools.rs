//! Generic knowledge tool contracts and response shaping.
//!
//! Runtime-specific crates provide pack discovery and resolution. The SDK owns
//! the stable tool schemas and result payloads so builtin, project, and remote
//! packs present the same API to agents.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tool_api::{Tool, ToolCategory, ToolResult, ToolSpec};

use crate::{
    KnowledgeDocAuthority, KnowledgeDocFilter, KnowledgeDocKind, KnowledgeDocManifest,
    KnowledgeDocSearchHit, KnowledgeDocStatus, KnowledgePack, KnowledgePackManifest,
};

#[async_trait]
pub trait KnowledgeRegistry: Send + Sync {
    async fn list_packs(&self) -> Result<Vec<KnowledgePackSummary>>;
    async fn resolve_pack(&self, selector: &str) -> Result<Arc<dyn KnowledgePack>>;
}

#[derive(Clone)]
pub struct KnowledgePackEntry {
    selector: String,
    pack: Arc<dyn KnowledgePack>,
}

impl KnowledgePackEntry {
    pub fn new(selector: impl Into<String>, pack: impl KnowledgePack + 'static) -> Self {
        Self {
            selector: selector.into(),
            pack: Arc::new(pack),
        }
    }

    pub fn selector(&self) -> &str {
        &self.selector
    }

    pub fn pack(&self) -> &Arc<dyn KnowledgePack> {
        &self.pack
    }

    fn into_parts(self) -> (String, Arc<dyn KnowledgePack>) {
        (self.selector, self.pack)
    }
}

impl<P> From<(&str, P)> for KnowledgePackEntry
where
    P: KnowledgePack + 'static,
{
    fn from((selector, pack): (&str, P)) -> Self {
        Self::new(selector, pack)
    }
}

impl<P> From<(String, P)> for KnowledgePackEntry
where
    P: KnowledgePack + 'static,
{
    fn from((selector, pack): (String, P)) -> Self {
        Self::new(selector, pack)
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
        let (selector, pack) = entry.into_parts();
        self.with_pack(selector, pack)
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
    pub pack_version: String,
    pub root_uri: String,
    pub document_count: usize,
}

impl KnowledgePackSummary {
    pub fn new(pack: impl Into<String>, manifest: &dyn KnowledgePackManifest) -> Self {
        Self {
            pack: pack.into(),
            pack_id: manifest.pack_id().to_string(),
            pack_version: manifest.pack_version().to_string(),
            root_uri: manifest.root_uri().to_string(),
            document_count: manifest.docs().len(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeListArgs {
    pub pack: String,
    #[serde(flatten)]
    pub filter: KnowledgeFilterArgs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeReadArgs {
    pub pack: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeSearchArgs {
    pub pack: String,
    pub query: String,
    #[serde(flatten)]
    pub filter: KnowledgeFilterArgs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeTreeArgs {
    pub pack: String,
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeNeighborArgs {
    pub pack: String,
    pub path: String,
    pub edge_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct KnowledgeFilterArgs {
    #[serde(default)]
    pub tags: Vec<String>,
    pub kind: Option<String>,
    pub authority: Option<String>,
    pub status: Option<String>,
    pub path_prefix: Option<String>,
    pub related_to: Option<String>,
    pub edge_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocManifestResult {
    pub id: String,
    pub pack: String,
    pub virtual_path: String,
    pub source_path: String,
    pub title: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: String,
    pub authority: String,
    pub status: String,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocReadResult {
    pub manifest: KnowledgeDocManifestResult,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeDocSearchResult {
    pub id: String,
    pub pack: String,
    pub virtual_path: String,
    pub title: String,
    pub summary: String,
    pub kind: String,
    pub authority: String,
    pub tags: Vec<String>,
    pub score: usize,
    pub matched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

pub fn knowledge_filter(filter: KnowledgeFilterArgs) -> Result<KnowledgeDocFilter> {
    Ok(KnowledgeDocFilter {
        tags: filter.tags,
        kind: parse_knowledge_enum(filter.kind)?,
        authority: parse_knowledge_enum(filter.authority)?,
        status: parse_knowledge_enum(filter.status)?,
        path_prefix: filter.path_prefix,
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

pub fn knowledge_manifest_result(
    pack: &str,
    doc: &KnowledgeDocManifest,
) -> KnowledgeDocManifestResult {
    KnowledgeDocManifestResult {
        id: doc.id.clone(),
        pack: pack.to_string(),
        virtual_path: doc.virtual_path.clone(),
        source_path: doc.source_path.clone(),
        title: doc.title.clone(),
        summary: doc.summary.clone(),
        description: doc.description.clone(),
        kind: doc.kind.as_str().to_string(),
        authority: doc.authority.as_str().to_string(),
        status: doc.status.as_str().to_string(),
        tags: doc.tags.clone(),
        aliases: doc.aliases.clone(),
        keywords: doc.keywords.clone(),
    }
}

pub fn knowledge_search_result(pack: &str, hit: KnowledgeDocSearchHit) -> KnowledgeDocSearchResult {
    KnowledgeDocSearchResult {
        id: hit.id,
        pack: pack.to_string(),
        virtual_path: hit.virtual_path,
        title: hit.title,
        summary: hit.summary,
        kind: hit.kind.as_str().to_string(),
        authority: hit.authority.as_str().to_string(),
        tags: hit.tags,
        score: hit.score,
        matched: hit.matched,
        content: hit.content,
    }
}

pub fn knowledge_document_metadata_vars(
    pack_prefix: &str,
    pack: &dyn KnowledgePack,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for doc in pack.manifest().docs() {
        let metadata = doc_metadata(doc);
        vars.insert(
            knowledge_document_var_key(pack_prefix, doc),
            metadata.clone(),
        );
        for key in knowledge_document_alias_var_keys(pack_prefix, doc) {
            vars.entry(key).or_insert_with(|| metadata.clone());
        }
    }
    vars
}

pub fn knowledge_pack_prompt_vars(
    selector: &str,
    pack: &dyn KnowledgePack,
) -> HashMap<String, String> {
    let prefix = knowledge_pack_var_prefix(selector);
    let mut vars = HashMap::new();
    vars.insert(prefix.clone(), knowledge_pack_summary(selector, pack));
    vars.extend(knowledge_document_metadata_vars(&prefix, pack));
    vars
}

pub fn knowledge_pack_var_prefix(selector: &str) -> String {
    if let Some(slug) = selector.strip_prefix("workspace:") {
        format!("lib.{}", normalize_var_segment(slug))
    } else if selector == "workspace" {
        "lib".to_string()
    } else {
        selector.replace(':', ".").replace('-', "_")
    }
}

pub fn knowledge_pack_summary(selector: &str, pack: &dyn KnowledgePack) -> String {
    let manifest = pack.manifest();
    let mut source_name = selector.splitn(2, ':');
    let source = source_name.next().unwrap_or(selector);
    let name = source_name.next().unwrap_or(manifest.pack_id());
    let ctx = KnowledgePackSummaryContext {
        source,
        name,
        root: manifest.root_uri(),
        usage: "Use the knowledge tools to search, inspect metadata, expand graph neighbors, and read documents from this pack when relevant.",
        docs: manifest
            .docs()
            .iter()
            .map(|doc| KnowledgeDocumentSummaryContext {
                path: doc.virtual_path.as_str(),
                id: doc.id.as_str(),
                kind: doc.kind.as_str(),
                title: doc.title.as_str(),
                summary: doc.summary.as_str(),
            })
            .collect(),
    };

    nenjo_xml::to_xml_pretty(&ctx, 2)
}

#[derive(Debug, Serialize)]
#[serde(rename = "knowledge_pack")]
struct KnowledgePackSummaryContext<'a> {
    #[serde(rename = "@source")]
    source: &'a str,
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
    #[serde(rename = "@path")]
    path: &'a str,
    #[serde(rename = "@id")]
    id: &'a str,
    #[serde(rename = "@kind")]
    kind: &'a str,
    title: &'a str,
    summary: &'a str,
}

pub fn knowledge_document_var_key(pack_prefix: &str, doc: &KnowledgeDocManifest) -> String {
    let relative = pack_relative_path(pack_prefix, doc)
        .unwrap_or(doc.virtual_path.as_str())
        .trim_matches('/');
    let path = relative
        .strip_suffix(".md")
        .unwrap_or(relative)
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(normalize_var_segment)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(".");
    if path.is_empty() {
        pack_prefix.to_string()
    } else {
        format!("{pack_prefix}.{path}")
    }
}

fn knowledge_document_alias_var_keys(pack_prefix: &str, doc: &KnowledgeDocManifest) -> Vec<String> {
    let mut keys = Vec::new();
    let Some(relative) = pack_relative_path(pack_prefix, doc) else {
        return keys;
    };
    let Some((parent, _leaf)) = relative
        .strip_suffix(".md")
        .unwrap_or(relative)
        .rsplit_once('/')
    else {
        return keys;
    };
    let parent = parent
        .split('/')
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

fn pack_relative_path<'a>(pack_prefix: &str, doc: &'a KnowledgeDocManifest) -> Option<&'a str> {
    match pack_prefix {
        "builtin.nenjo" => doc.virtual_path.strip_prefix("builtin://nenjo/"),
        "lib" => doc
            .virtual_path
            .strip_prefix("library://")
            .and_then(|rest| rest.split_once('/').map(|(_, path)| path)),
        _ if pack_prefix.starts_with("lib.") => {
            let slug = pack_prefix.trim_start_matches("lib.");
            let prefix = format!("library://{slug}/");
            doc.virtual_path.strip_prefix(&prefix)
        }
        _ => None,
    }
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
    #[serde(rename = "@path")]
    path: &'a str,
    #[serde(rename = "@title")]
    title: &'a str,
    #[serde(rename = "@kind")]
    kind: KnowledgeDocKind,
    #[serde(rename = "@authority")]
    authority: KnowledgeDocAuthority,
    #[serde(rename = "@status")]
    status: KnowledgeDocStatus,
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tags: Vec<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    aliases: Vec<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    keywords: Vec<&'a str>,
}

fn doc_metadata(doc: &KnowledgeDocManifest) -> String {
    let path = prompt_doc_path(doc);
    let ctx = KnowledgeDocMetadataContext {
        path: &path,
        title: &doc.title,
        summary: &doc.summary,
        description: doc.description.as_deref(),
        kind: doc.kind,
        authority: doc.authority,
        status: doc.status,
        tags: doc.tags.iter().map(String::as_str).collect(),
        aliases: doc.aliases.iter().map(String::as_str).collect(),
        keywords: doc.keywords.iter().map(String::as_str).collect(),
    };
    nenjo_xml::to_xml_pretty(&ctx, 2)
}

fn prompt_doc_path(doc: &KnowledgeDocManifest) -> String {
    if doc.virtual_path.starts_with("library://") {
        doc.virtual_path
            .splitn(4, '/')
            .nth(3)
            .unwrap_or(&doc.virtual_path)
            .to_string()
    } else {
        doc.virtual_path.clone()
    }
}

fn pack_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Knowledge pack selector such as builtin:nenjo, workspace:<pack_slug>, or remote:<pack_id>."
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
        "authority": {
            "type": "string",
            "description": "Optional authority filter such as canonical, reference, or advisory"
        },
        "status": {
            "type": "string",
            "description": "Optional status filter such as stable, draft, or deprecated"
        },
        "path_prefix": {
            "type": "string",
            "description": "Optional virtual or pack-relative path prefix"
        },
        "related_to": {
            "type": "string",
            "description": "Optional path of a document this result must be related to"
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
            "path": {
                "type": "string",
                "description": "Document path, id, alias, or virtual path within the selected pack"
            }
        },
        "required": ["pack", "path"],
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
            name: "list_knowledge_docs".into(),
            description: "List compact document metadata from one knowledge pack without loading document bodies.".into(),
            parameters: knowledge_filter_schema(None, &["pack"]),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "read_knowledge_doc".into(),
            description: "Read one full document body from a knowledge pack by path.".into(),
            parameters: knowledge_lookup_schema(),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "read_knowledge_doc_manifest".into(),
            description: "Read one document's metadata from a knowledge pack by path without loading the body.".into(),
            parameters: knowledge_lookup_schema(),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "search_knowledge".into(),
            description: "Search a knowledge pack and return matches with body content. Use this when you need to inspect or quote matching text.".into(),
            parameters: knowledge_filter_schema(
                Some(json!({
                    "query": {
                        "type": "string",
                        "description": "Search query, path, title, tag, summary, or body text"
                    }
                })),
                &["pack", "query"],
            ),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "search_knowledge_paths".into(),
            description: "Search a knowledge pack using metadata only and return compact results without body content.".into(),
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
            name: "list_knowledge_tree".into(),
            description: "List the document tree for a knowledge pack, optionally under a prefix.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pack": pack_schema(),
                    "prefix": {
                        "type": "string",
                        "description": "Optional virtual or pack-relative path prefix"
                    }
                },
                "required": ["pack"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_knowledge_neighbors".into(),
            description: "List graph neighbors for one document in a knowledge pack.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pack": pack_schema(),
                    "path": {
                        "type": "string",
                        "description": "Document path, id, alias, or virtual path within the selected pack"
                    },
                    "edge_type": {
                        "type": "string",
                        "description": "Optional relationship type filter such as references or depends_on"
                    }
                },
                "required": ["pack", "path"],
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
            "list_knowledge_docs" => {
                let args: KnowledgeListArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let filter = knowledge_filter(args.filter)?;
                let docs = pack
                    .list_docs(filter)
                    .into_iter()
                    .map(|doc| knowledge_manifest_result(&args.pack, doc))
                    .collect::<Vec<_>>();
                serde_json::to_value(docs)?
            }
            "read_knowledge_doc" => {
                let args: KnowledgeReadArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let doc = pack.read_doc(&args.path).ok_or_else(|| {
                    anyhow!(
                        "knowledge document '{}' not found in pack '{}'",
                        args.path,
                        args.pack
                    )
                })?;
                serde_json::to_value(KnowledgeDocReadResult {
                    manifest: knowledge_manifest_result(&args.pack, &doc.manifest),
                    content: doc.content,
                })?
            }
            "read_knowledge_doc_manifest" => {
                let args: KnowledgeReadArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let doc = pack.read_manifest(&args.path).ok_or_else(|| {
                    anyhow!(
                        "knowledge document '{}' not found in pack '{}'",
                        args.path,
                        args.pack
                    )
                })?;
                serde_json::to_value(knowledge_manifest_result(&args.pack, doc))?
            }
            "search_knowledge" => {
                let args: KnowledgeSearchArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let filter = knowledge_filter(args.filter)?;
                let hits = pack
                    .search_docs(&args.query, filter)
                    .into_iter()
                    .map(|hit| knowledge_search_result(&args.pack, hit))
                    .collect::<Vec<_>>();
                serde_json::to_value(hits)?
            }
            "search_knowledge_paths" => {
                let args: KnowledgeSearchArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let filter = knowledge_filter(args.filter)?;
                let hits = pack
                    .search_paths(&args.query, filter)
                    .into_iter()
                    .map(|hit| knowledge_search_result(&args.pack, hit))
                    .collect::<Vec<_>>();
                serde_json::to_value(hits)?
            }
            "list_knowledge_tree" => {
                let args: KnowledgeTreeArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                serde_json::to_value(pack.list_tree(args.prefix.as_deref()))?
            }
            "list_knowledge_neighbors" => {
                let args: KnowledgeNeighborArgs = serde_json::from_value(args)?;
                let pack = self.registry.resolve_pack(&args.pack).await?;
                let edge_type = parse_knowledge_enum(args.edge_type)?;
                serde_json::to_value(pack.neighbors(&args.path, edge_type))?
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
    use super::knowledge_pack_var_prefix;

    #[test]
    fn workspace_knowledge_uses_lib_template_namespace() {
        assert_eq!(
            knowledge_pack_var_prefix("workspace:Product Docs"),
            "lib.product_docs"
        );
        assert_eq!(knowledge_pack_var_prefix("workspace"), "lib");
    }
}
