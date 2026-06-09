//! Typed inline payloads for [`crate::Command::ManifestChanged`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema version for inline manifest resource envelopes.
pub const MANIFEST_RESOURCE_SCHEMA: &str = "manifest.resource.v1";

/// Envelope wrapping canonical inline manifest resource bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestResourcePayload<T> {
    pub schema: String,
    pub data: T,
}

impl<T> ManifestResourcePayload<T> {
    pub fn new(data: T) -> Self {
        Self {
            schema: MANIFEST_RESOURCE_SCHEMA.to_string(),
            data,
        }
    }
}

impl<T: Serialize> ManifestResourcePayload<T> {
    pub fn into_value(self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// Inline library knowledge document metadata published with `manifest.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeDocumentResource {
    pub id: Uuid,
    pub pack_id: Uuid,
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
    /// Outgoing edges for this document (same set as `GET .../items/{doc}/edges`).
    #[serde(default)]
    pub edges: Vec<KnowledgeDocumentEdge>,
}

/// Relationship between two knowledge documents within a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeDocumentEdge {
    pub id: Uuid,
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

/// Parsed inline knowledge document payload plus wire-presence metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKnowledgeDocument {
    pub resource: KnowledgeDocumentResource,
    /// `true` when the inline payload explicitly included an `edges` field.
    pub edges_present: bool,
}

impl ManifestResourcePayload<KnowledgeDocumentResource> {
    pub fn parse(value: &serde_json::Value) -> Option<Self> {
        let payload: Self = serde_json::from_value(value.clone()).ok()?;
        (payload.schema == MANIFEST_RESOURCE_SCHEMA).then_some(payload)
    }

    pub fn parse_document(value: &serde_json::Value) -> Option<ParsedKnowledgeDocument> {
        let envelope = Self::parse(value)?;
        let edges_present = edges_field_present(value).unwrap_or(false);
        Some(ParsedKnowledgeDocument {
            resource: envelope.data,
            edges_present,
        })
    }
}

fn edges_field_present(envelope: &serde_json::Value) -> Option<bool> {
    let data = envelope.get("data")?;
    data.as_object().map(|object| object.contains_key("edges"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_doc() -> KnowledgeDocumentResource {
        let now = Utc::now();
        KnowledgeDocumentResource {
            id: Uuid::from_u128(1),
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
            edges: vec![KnowledgeDocumentEdge {
                id: Uuid::from_u128(3),
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
        let payload = ManifestResourcePayload::new(sample_doc());
        let value = payload.into_value();
        let parsed = ManifestResourcePayload::<KnowledgeDocumentResource>::parse(&value)
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
                "pack_id": Uuid::from_u128(2),
                "pack_slug": "alpha",
                "slug": "notes",
                "filename": "notes.md",
                "content_type": "text/markdown",
                "created_at": Utc::now(),
                "updated_at": Utc::now()
            }
        });
        let parsed = ManifestResourcePayload::<KnowledgeDocumentResource>::parse_document(&value)
            .expect("payload should parse");
        assert!(parsed.resource.edges.is_empty());
        assert!(!parsed.edges_present);
    }

    #[test]
    fn parse_document_detects_explicit_edges_field() {
        let value = ManifestResourcePayload::new(KnowledgeDocumentResource {
            edges: Vec::new(),
            ..sample_doc()
        })
        .into_value();
        let parsed = ManifestResourcePayload::<KnowledgeDocumentResource>::parse_document(&value)
            .expect("payload should parse");
        assert!(parsed.edges_present);
        assert!(parsed.resource.edges.is_empty());
    }

    #[test]
    fn parse_rejects_wrong_schema() {
        let value = serde_json::json!({
            "schema": "manifest.resource.v0",
            "data": {}
        });
        assert!(
            ManifestResourcePayload::<KnowledgeDocumentResource>::parse(&value).is_none()
        );
    }
}