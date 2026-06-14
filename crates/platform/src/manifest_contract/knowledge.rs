//! Canonical wire types for library knowledge documents.
//!
//! Pipeline: DB row → [`KnowledgeDocumentRecord`] → local library manifest →
//! [`nenjo_knowledge::KnowledgeDocManifest`] (agent runtime).

use super::wire::{PlatformRecord, data_field_present, wrap_resource_record};
use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo_events::ManifestResourcePayload;
use nenjo_knowledge::{
    KnowledgeDocEdge, KnowledgeDocEdgeType, KnowledgeDocKind, KnowledgeDocManifest,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Directed relationship between two knowledge documents within a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeDocumentEdgeRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub source_item_id: Uuid,
    pub source_doc: String,
    pub target_item_id: Uuid,
    pub target_doc: String,
    pub edge_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Metadata for a library knowledge document on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeDocumentRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub pack_id: Uuid,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pack_slug: String,
    pub slug: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub content_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub edges: Vec<KnowledgeDocumentEdgeRecord>,
}

/// Parsed inline knowledge document payload plus wire-presence metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKnowledgeDocument {
    pub record: KnowledgeDocumentRecord,
    /// `true` when the inline payload explicitly included an `edges` field.
    pub edges_present: bool,
}

impl PlatformRecord for KnowledgeDocumentRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}

impl KnowledgeDocumentRecord {
    pub fn updated_at_rfc3339(&self) -> String {
        self.updated_at.to_rfc3339()
    }

    pub fn with_pack_slug(mut self, pack_slug: impl Into<String>) -> Self {
        if self.pack_slug.is_empty() {
            self.pack_slug = pack_slug.into();
        }
        self
    }

    pub fn outgoing_edges(&self) -> impl Iterator<Item = &KnowledgeDocumentEdgeRecord> {
        self.edges
            .iter()
            .filter(|edge| edge.source_doc == self.slug)
    }

    pub fn library_doc_relative_path(&self) -> String {
        let mut path = self.path.clone().unwrap_or_default();
        path = path.trim_matches('/').to_string();
        if path.is_empty() {
            self.filename.clone()
        } else {
            format!("{path}/{}", self.filename)
        }
    }

    pub fn library_selector(&self, pack_slug: &str) -> String {
        format!("library://{pack_slug}/{}", self.library_doc_relative_path())
    }
}

/// Parse a manifest inline envelope into a knowledge document record.
pub fn parse_document_payload(value: &serde_json::Value) -> Option<ParsedKnowledgeDocument> {
    let envelope = ManifestResourcePayload::<KnowledgeDocumentRecord>::parse(value)?;
    let edges_present = data_field_present(value, "edges").unwrap_or(false);
    Some(ParsedKnowledgeDocument {
        record: envelope.data,
        edges_present,
    })
}

/// Wrap a knowledge document record for manifest event emission.
pub fn wrap_document_record(record: KnowledgeDocumentRecord) -> serde_json::Value {
    wrap_resource_record(record)
}

pub fn parse_doc_edge_type(value: &str) -> KnowledgeDocEdgeType {
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

pub fn parse_doc_kind(value: Option<&str>) -> KnowledgeDocKind {
    KnowledgeDocKind::new(value.unwrap_or("reference"))
}

pub fn to_agent_manifest(
    pack_slug: &str,
    record: &KnowledgeDocumentRecord,
    resolve_target: impl Fn(Slug) -> Option<String>,
) -> KnowledgeDocManifest {
    let relative_path = record.library_doc_relative_path();
    KnowledgeDocManifest {
        id: record.slug.clone(),
        selector: record.library_selector(pack_slug),
        source_path: format!("docs/{relative_path}"),
        title: record
            .title
            .clone()
            .unwrap_or_else(|| record.filename.clone()),
        summary: record
            .summary
            .clone()
            .unwrap_or_else(|| format!("Knowledge document {relative_path}")),
        kind: parse_doc_kind(record.kind.as_deref()),
        tags: record.tags.clone(),
        related: record
            .outgoing_edges()
            .filter_map(|edge| {
                resolve_target(Slug::derive(&edge.target_doc)).map(|target| KnowledgeDocEdge {
                    edge_type: parse_doc_edge_type(&edge.edge_type),
                    target,
                    description: edge.note.clone(),
                })
            })
            .collect(),
        updated_at: record.updated_at_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo_events::MANIFEST_RESOURCE_SCHEMA;

    fn sample_record() -> KnowledgeDocumentRecord {
        let now = Utc::now();
        KnowledgeDocumentRecord {
            id: Uuid::from_u128(1),
            org_id: Uuid::from_u128(9),
            pack_id: Uuid::from_u128(2),
            pack_slug: "alpha".into(),
            slug: "notes".into(),
            filename: "notes.md".into(),
            path: Some("guides".into()),
            title: Some("Notes".into()),
            kind: Some("guide".into()),
            summary: Some("summary".into()),
            tags: vec!["e2e".into()],
            content_type: "text/markdown".into(),
            created_at: now,
            updated_at: now,
            edges: vec![KnowledgeDocumentEdgeRecord {
                id: Uuid::from_u128(3),
                org_id: Uuid::from_u128(9),
                source_item_id: Uuid::from_u128(1),
                source_doc: "notes".into(),
                target_item_id: Uuid::from_u128(4),
                target_doc: "related".into(),
                edge_type: "related_to".into(),
                note: Some("linked".into()),
                created_at: now,
                updated_at: now,
            }],
        }
    }

    #[test]
    fn manifest_resource_payload_round_trips() {
        let value = wrap_document_record(sample_record());
        let parsed = ManifestResourcePayload::<KnowledgeDocumentRecord>::parse(&value)
            .expect("payload should parse");
        assert_eq!(parsed.data.slug, "notes");
        assert_eq!(parsed.data.edges.len(), 1);
        assert_eq!(parsed.data.edges[0].target_doc, "related");
    }

    #[test]
    fn missing_edges_deserializes_as_empty() {
        let value = serde_json::json!({
            "schema": MANIFEST_RESOURCE_SCHEMA,
            "data": {
                "id": Uuid::from_u128(1),
                "org_id": Uuid::from_u128(9),
                "pack_id": Uuid::from_u128(2),
                "pack_slug": "alpha",
                "slug": "notes",
                "filename": "notes.md",
                "content_type": "text/markdown",
                "created_at": Utc::now(),
                "updated_at": Utc::now()
            }
        });
        let parsed = parse_document_payload(&value).expect("payload should parse");
        assert!(parsed.record.edges.is_empty());
        assert!(!parsed.edges_present);
    }

    #[test]
    fn parse_document_detects_explicit_edges_field() {
        let value = wrap_document_record(KnowledgeDocumentRecord {
            edges: Vec::new(),
            ..sample_record()
        });
        let parsed = parse_document_payload(&value).expect("payload should parse");
        assert!(parsed.edges_present);
        assert!(parsed.record.edges.is_empty());
    }
}
