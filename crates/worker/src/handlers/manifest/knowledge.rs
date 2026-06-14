use nenjo::Slug;
use nenjo_events::{
    KnowledgeDocumentEdge, KnowledgeDocumentResource, ManifestResourcePayload,
    ParsedKnowledgeDocument,
};
use nenjo_platform::api_client::{DocumentSyncEdge, DocumentSyncMeta};

pub(super) fn parse_knowledge_document_payload(
    payload: &serde_json::Value,
) -> Option<ParsedKnowledgeDocument> {
    ManifestResourcePayload::<KnowledgeDocumentResource>::parse_document(payload)
}

pub(super) fn document_sync_meta(doc: &KnowledgeDocumentResource) -> DocumentSyncMeta {
    DocumentSyncMeta {
        id: Some(doc.id),
        pack_id: Some(doc.pack_id),
        pack_slug: doc.pack_slug.clone(),
        slug: doc.slug.clone(),
        filename: doc.filename.clone(),
        path: doc.path.clone(),
        title: doc.title.clone(),
        kind: doc.kind.clone(),
        summary: doc.summary.clone(),
        tags: doc.tags.clone(),
        content_type: doc.content_type.clone(),
        updated_at: doc.updated_at.to_rfc3339(),
    }
}

pub(super) fn document_sync_edges(edges: &[KnowledgeDocumentEdge]) -> Vec<DocumentSyncEdge> {
    edges
        .iter()
        .map(|edge| DocumentSyncEdge {
            id: edge.id,
            pack_id: None,
            source_doc: Slug::derive(&edge.source_doc),
            source_item_id: Some(edge.source_item_id),
            target_doc: Slug::derive(&edge.target_doc),
            target_item_id: Some(edge.target_item_id),
            edge_type: edge.edge_type.clone(),
            note: edge.note.clone(),
            created_at: edge.created_at,
            updated_at: edge.updated_at,
        })
        .collect()
}

pub enum DocumentEdgesSource<'a> {
    Inline(&'a [DocumentSyncEdge]),
    FetchFromApi,
}

pub(super) fn document_edges_source<'a>(
    parsed: &'a ParsedKnowledgeDocument,
    edges: &'a [DocumentSyncEdge],
) -> DocumentEdgesSource<'a> {
    if parsed.edges_present {
        DocumentEdgesSource::Inline(edges)
    } else {
        DocumentEdgesSource::FetchFromApi
    }
}