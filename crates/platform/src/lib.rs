//! Platform-facing manifest contracts, transport clients, and local execution backends.
//!
//! This crate sits between the core `nenjo` manifest model and the runtime surfaces that expose
//! manifest operations over HTTP or MCP. Most consumers will use:
//!
//! - [`PlatformManifestClient`] to talk to platform HTTP endpoints.
//! - [`ApiClient`] to talk to worker-facing platform HTTP endpoints.
//! - [`PlatformManifestBackend`] to bridge a local manifest store with the platform API.
//! - [`ManifestMcpContract`] to expose the manifest tool contract to an MCP server or test
//!   harness.

/// Typed HTTP client for worker-facing platform API endpoints.
pub mod api_client;
/// Platform-backed manifest backend implementations and payload encoding hooks.
pub mod backend;
/// Thin HTTP client for the platform manifest API.
pub mod client;
pub mod library_knowledge;
/// Local in-process manifest MCP backend implementations.
pub mod local;
/// Canonical wire record types for manifest resources.
pub mod manifest_contract;
/// Shared manifest resource and encrypted-content classification.
pub mod manifest_kinds;
/// Manifest MCP contract types, params, results, and dispatch helpers.
pub mod manifest_mcp;
/// Access-policy helpers for filtering manifest resources by platform scopes.
pub mod policy;
mod prompt_merge;
/// Platform-private resource id sidecar used for encrypted write metadata.
pub mod resource_ids;
/// REST tool specs shared by worker-side REST-backed tooling.
pub mod rest;
/// Platform scope parsing and implication rules.
pub mod scope;
/// Tool implementations for platform manifest and REST operations.
pub mod tools;
/// Shared transport types used by the platform bootstrap and write APIs.
pub mod types;

pub use api_client::{ApiClient, ApiClientError, NoopPayloadCodec, PayloadCodec};
pub use backend::{NoopSensitivePayloadEncoder, PlatformManifestBackend, SensitivePayloadEncoder};
pub use client::PlatformManifestClient;

pub use local::LocalManifestMcpBackend;
pub use manifest_contract::{
    AgentRecord, ContextBlockContentRecord, ContextBlockRecord, KnowledgeDocumentEdgeRecord,
    KnowledgeDocumentRecord, ParsedKnowledgeDocument, PlatformRecord, parse_doc_edge_type,
    parse_doc_kind, parse_document_payload, parse_resource_payload, to_knowledge_manifest,
    wrap_document_record, wrap_resource_record,
};
pub use manifest_kinds::{ContentScope, ManifestKind, SensitiveContentKind};
pub use manifest_mcp::{
    AbilitiesGetParams, AbilitiesListResult, AbilityConfigureAssignments, AbilityConfigureDocument,
    AbilityConfigureMetadata, AbilityConfigureParams, AbilityConfigureResult, AbilityDocument,
    AbilityGetResult, AbilityManifestBackend, AbilitySummary, AgentConfigureAssignments,
    AgentConfigureDocument, AgentConfigureMetadata, AgentConfigureParams, AgentConfigureResult,
    AgentDocument, AgentGetResult, AgentManifestBackend, AgentSummary, AgentsGetParams,
    AgentsListResult, CommandConfigureDocument, CommandConfigureMetadata, CommandConfigureParams,
    CommandConfigureResult, CommandManifestBackend, ContextBlockConfigureDocument,
    ContextBlockConfigureMetadata, ContextBlockConfigureParams, ContextBlockConfigureResult,
    ContextBlockDocument, ContextBlockGetResult, ContextBlockManifestBackend, ContextBlockSummary,
    ContextBlocksGetParams, ContextBlocksListResult, CouncilAddMemberParams, CouncilCreateDocument,
    CouncilCreateMemberDocument, CouncilCreateParams, CouncilDeleteParams, CouncilDocument,
    CouncilGetResult, CouncilManifestBackend, CouncilMemberDocument, CouncilMemberUpdateDocument,
    CouncilMutationResult, CouncilRemoveMemberParams, CouncilSummary, CouncilUpdateDocument,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, CouncilsListResult,
    DeleteResult, DomainConfigureAssignments, DomainConfigureDocument, DomainConfigureMetadata,
    DomainConfigureParams, DomainConfigureResult, DomainDocument, DomainGetResult,
    DomainManifestBackend, DomainSummary, DomainsGetParams, DomainsListResult,
    KnowledgeDocContentDocument, KnowledgeDocCreateDocument, KnowledgeDocCreateParams,
    KnowledgeDocDeleteParams, KnowledgeDocMutationResult, KnowledgeDocRelatedDocument,
    KnowledgeDocSummary, KnowledgeDocUpdateDocument, KnowledgeDocUpdateParams,
    KnowledgePackCreateDocument, KnowledgePackCreateParams, KnowledgePackDocument,
    KnowledgePackMutationResult, KnowledgePackUpdateDocument, KnowledgePackUpdateParams,
    LibraryManifestBackend, ManifestMcpBackend, ManifestMcpContract, ModelCreateDocument,
    ModelCreateParams, ModelDeleteParams, ModelDocument, ModelGetResult, ModelManifestBackend,
    ModelMutationResult, ModelSummary, ModelUpdateDocument, ModelUpdateParams, ModelsGetParams,
    ModelsListResult, ProjectCreateDocument, ProjectCreateParams, ProjectDeleteParams,
    ProjectDocument, ProjectGetResult, ProjectManifestBackend, ProjectMutationResult,
    ProjectSummary, ProjectUpdateDocument, ProjectUpdateParams, ProjectsGetParams,
    ProjectsListResult, RoutineConfigureDocument, RoutineConfigureMetadata, RoutineConfigureParams,
    RoutineConfigureResult, RoutineDeleteParams, RoutineDocument, RoutineEdgeInput,
    RoutineGetResult, RoutineGraphInput, RoutineManifestBackend, RoutineStepInput, RoutineSummary,
    RoutinesGetParams, RoutinesListResult,
};
pub use policy::ManifestAccessPolicy;
pub use resource_ids::{PlatformResourceIdSnapshot, PlatformResourceIdStore, PlatformResourceKind};
pub use scope::{PlatformScope, ScopeAction, ScopeResource};
pub use types::{BootstrapManifestResponse, PlatformManifestItem, PlatformManifestWriteRequest};
