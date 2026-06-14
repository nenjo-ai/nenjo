use nenjo_platform::manifest_contract::{
    KnowledgeDocumentEdgeRecord, ParsedKnowledgeDocument, parse_document_payload,
};

pub(super) fn parse_knowledge_document_payload(
    payload: &serde_json::Value,
) -> Option<ParsedKnowledgeDocument> {
    parse_document_payload(payload)
}

pub enum DocumentEdgesSource<'a> {
    Inline(&'a [KnowledgeDocumentEdgeRecord]),
    FetchFromApi,
}

pub(super) fn document_edges_source<'a>(
    parsed: &'a ParsedKnowledgeDocument,
    edges: &'a [KnowledgeDocumentEdgeRecord],
) -> DocumentEdgesSource<'a> {
    if parsed.edges_present {
        DocumentEdgesSource::Inline(edges)
    } else {
        DocumentEdgesSource::FetchFromApi
    }
}
