//! Canonical wire types for manifest resources (REST, events, worker sync).

pub mod context_block;
pub mod knowledge;
pub mod wire;

pub use context_block::{ContextBlockContentRecord, ContextBlockRecord};
pub use knowledge::{
    KnowledgeDocumentEdgeRecord, KnowledgeDocumentRecord, ParsedKnowledgeDocument,
    parse_doc_edge_type, parse_doc_kind, parse_document_payload, to_agent_manifest,
    wrap_document_record,
};
pub use wire::{data_field_present, parse_resource_payload, wrap_resource_record, PlatformRecord};