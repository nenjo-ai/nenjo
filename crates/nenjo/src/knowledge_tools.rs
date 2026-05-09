//! Generic knowledge tool contracts and response shaping.
//!
//! Runtime-specific crates provide pack discovery and resolution. The SDK owns
//! the stable tool schemas and result payloads so builtin, project, and remote
//! packs present the same API to agents.

use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::knowledge::{
    KnowledgeDocAuthority, KnowledgeDocFilter, KnowledgeDocKind, KnowledgeDocManifest,
    KnowledgeDocSearchHit, KnowledgeDocStatus, KnowledgePack, KnowledgePackManifest,
};
use crate::{ToolCategory, ToolSpec};

#[async_trait]
pub trait KnowledgeRegistry: Send + Sync {
    async fn list_packs(&self) -> Result<Vec<KnowledgePackSummary>>;
    async fn resolve_pack(&self, selector: &str) -> Result<Box<dyn KnowledgePack>>;
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
        "project" => doc
            .virtual_path
            .strip_prefix("project://")
            .and_then(|rest| rest.split_once('/').map(|(_, path)| path)),
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
    if doc.virtual_path.starts_with("project://") {
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
        "description": "Knowledge pack selector such as builtin:nenjo, project for the active project, project:<project_slug>, or remote:<pack_id>."
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
