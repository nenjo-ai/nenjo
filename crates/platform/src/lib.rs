//! Platform-facing manifest contracts, transport clients, and local execution backends.
//!
//! This crate sits between the core `nenjo` manifest model and the runtime surfaces that expose
//! manifest operations over HTTP or MCP. Most consumers will use:
//!
//! - [`PlatformManifestClient`] to talk to platform HTTP endpoints.
//! - [`PlatformManifestBackend`] to bridge a local manifest store with the platform API.
//! - [`ManifestMcpContract`](crate::manifest_mcp::ManifestMcpContract) to expose the manifest
//!   tool contract to an MCP server or test harness.

/// Platform-backed manifest backend implementations and payload encoding hooks.
pub mod backend;
/// Thin HTTP client for the platform manifest API.
pub mod client;
/// Local in-process manifest MCP backend implementations.
pub mod local;
/// Manifest MCP contract types, params, results, and dispatch helpers.
pub mod manifest_mcp;
/// Access-policy helpers for filtering manifest resources by platform scopes.
pub mod policy;
/// REST tool specs shared by worker-side REST-backed tooling.
pub mod rest;
/// Platform scope parsing and implication rules.
pub mod scope;
/// Shared transport types used by the platform bootstrap and write APIs.
pub mod types;

pub use backend::{NoopSensitivePayloadEncoder, PlatformManifestBackend, SensitivePayloadEncoder};
pub use client::PlatformManifestClient;
pub use local::LocalManifestMcpBackend;
pub use manifest_mcp::{
    AbilitiesGetParams, AbilitiesListResult, AbilityCreateDocument, AbilityCreateParams,
    AbilityDeleteParams, AbilityDocument, AbilityGetResult, AbilityManifestBackend,
    AbilityMutationResult, AbilityPromptDocument, AbilityPromptGetParams, AbilityPromptGetResult,
    AbilityPromptMutationResult, AbilityPromptUpdateParams, AbilitySummary, AbilityUpdateDocument,
    AbilityUpdateParams, AgentDeleteParams, AgentDocument, AgentGetResult, AgentManifestBackend,
    AgentMutationResult, AgentPromptDocument, AgentPromptGetParams, AgentPromptGetResult,
    AgentPromptMutationResult, AgentPromptUpdateParams, AgentSummary, AgentUpdateDocument,
    AgentUpdateParams, AgentsGetParams, AgentsListResult, ContextBlockContentDocument,
    ContextBlockContentGetParams, ContextBlockContentGetResult, ContextBlockContentMutationResult,
    ContextBlockContentUpdateParams, ContextBlockCreateDocument, ContextBlockCreateParams,
    ContextBlockDeleteParams, ContextBlockDocument, ContextBlockGetResult,
    ContextBlockManifestBackend, ContextBlockMutationResult, ContextBlockSummary,
    ContextBlockUpdateDocument, ContextBlockUpdateParams, ContextBlocksGetParams,
    ContextBlocksListResult, CouncilAddMemberParams, CouncilCreateDocument,
    CouncilCreateMemberDocument, CouncilCreateParams, CouncilDeleteParams, CouncilDocument,
    CouncilGetResult, CouncilManifestBackend, CouncilMemberDocument, CouncilMemberUpdateDocument,
    CouncilMutationResult, CouncilRemoveMemberParams, CouncilSummary, CouncilUpdateDocument,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, CouncilsListResult,
    DeleteResult, DomainCreateDocument, DomainCreateParams, DomainDeleteParams, DomainDocument,
    DomainGetResult, DomainManifestBackend, DomainManifestDocument, DomainManifestGetParams,
    DomainManifestGetResult, DomainManifestMutationResult, DomainManifestUpdateParams,
    DomainMutationResult, DomainPromptDocument, DomainPromptGetParams, DomainPromptGetResult,
    DomainPromptMutationResult, DomainPromptUpdateParams, DomainSummary, DomainUpdateDocument,
    DomainUpdateParams, DomainsGetParams, DomainsListResult, ManifestMcpBackend,
    ManifestMcpContract, ModelCreateDocument, ModelCreateParams, ModelDeleteParams, ModelDocument,
    ModelGetResult, ModelManifestBackend, ModelMutationResult, ModelSummary, ModelUpdateDocument,
    ModelUpdateParams, ModelsGetParams, ModelsListResult, ProjectCreateDocument,
    ProjectCreateParams, ProjectDeleteParams, ProjectDocument, ProjectDocumentContentDocument,
    ProjectDocumentContentMutationResult, ProjectDocumentContentUpdateParams,
    ProjectDocumentCreateDocument, ProjectDocumentCreateParams, ProjectDocumentDeleteParams,
    ProjectDocumentMutationResult, ProjectDocumentSummary, ProjectDocumentsListParams,
    ProjectDocumentsListResult, ProjectGetResult, ProjectManifestBackend, ProjectMutationResult,
    ProjectSummary, ProjectUpdateDocument, ProjectUpdateParams, ProjectsGetParams,
    ProjectsListResult, RoutineCreateDocument, RoutineCreateParams, RoutineDeleteParams,
    RoutineDocument, RoutineEdgeInput, RoutineGetResult, RoutineGraphInput, RoutineManifestBackend,
    RoutineMutationResult, RoutineStepInput, RoutineSummary, RoutineUpdateDocument,
    RoutineUpdateParams, RoutinesGetParams, RoutinesListResult,
};
pub use policy::ManifestAccessPolicy;
pub use scope::{PlatformScope, ScopeAction, ScopeResource};
pub use types::{BootstrapManifestResponse, PlatformManifestItem, PlatformManifestWriteRequest};
