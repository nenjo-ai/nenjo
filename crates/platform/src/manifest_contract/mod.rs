//! Canonical wire types for manifest resources (REST, events, worker sync).

pub mod ability;
pub mod agent;
pub mod context_block;
pub mod council;
pub mod domain;
pub mod knowledge;
pub mod model;
pub mod project;
pub mod routine;
pub mod wire;

pub use ability::{AbilityPromptRecord, AbilityRecord};
pub use agent::{AgentPromptRecord, AgentRecord};
pub use context_block::{ContextBlockContentRecord, ContextBlockRecord};
pub use council::{CouncilMemberRecord, CouncilRecord};
pub use domain::{DomainPromptRecord, DomainRecord};
pub use knowledge::{
    KnowledgeDocumentEdgeRecord, KnowledgeDocumentRecord, KnowledgePackRecord,
    ParsedKnowledgeDocument, library_pack_selector, parse_doc_edge_type, parse_doc_kind,
    parse_document_payload, parse_library_pack_selector, parse_library_pack_slug,
    to_knowledge_manifest, wrap_document_record,
};
pub use model::ModelRecord;
pub use project::{ProjectDetailRecord, ProjectRecord};
pub use routine::{RoutineEdgeRecord, RoutineRecord, RoutineStepRecord};
pub use wire::{PlatformRecord, data_field_present, parse_resource_payload, wrap_resource_record};
