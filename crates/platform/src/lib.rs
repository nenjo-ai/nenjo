//! Platform-facing manifest contracts, transport clients, and local execution backends.
//!
//! This crate sits between the core `nenjo` manifest model and the runtime surfaces that expose
//! manifest operations over HTTP or MCP. Most consumers will use:
//!
//! - [`PlatformManifestClient`] to talk to platform HTTP endpoints.
//! - [`PlatformManifestBackend`] to bridge a local manifest store with the platform API.
//! - [`ManifestMcpContract`] to expose the manifest tool contract to an MCP server or test
//!   harness.

/// Platform-backed manifest backend implementations and payload encoding hooks.
pub mod backend;
/// Thin HTTP client for the platform manifest API.
pub mod client;
mod knowledge_backend;
pub mod library_knowledge;
/// Local in-process manifest MCP backend implementations.
pub mod local;
/// Shared manifest resource and encrypted-content classification.
pub mod manifest_contract;
/// Manifest MCP contract types, params, results, and dispatch helpers.
pub mod manifest_mcp;
/// Access-policy helpers for filtering manifest resources by platform scopes.
pub mod policy;
mod prompt_merge;
/// REST tool specs shared by worker-side REST-backed tooling.
pub mod rest;
/// Platform scope parsing and implication rules.
pub mod scope;
/// Tool implementations for platform manifest and REST operations.
pub mod tools;
/// Shared transport types used by the platform bootstrap and write APIs.
pub mod types;

pub use backend::{NoopSensitivePayloadEncoder, PlatformManifestBackend, SensitivePayloadEncoder};
pub use client::PlatformManifestClient;
pub use local::LocalManifestMcpBackend;
pub use manifest_contract::{ContentScope, ManifestKind};
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
    DomainUpdateParams, DomainsGetParams, DomainsListResult, KnowledgeItemContentDocument,
    KnowledgeItemContentMutationResult, KnowledgeItemContentUpdateParams,
    KnowledgeItemCreateDocument, KnowledgeItemCreateParams, KnowledgeItemDeleteParams,
    KnowledgeItemMutationResult, KnowledgeItemSummary, KnowledgeManifestBackend,
    ManifestMcpBackend, ManifestMcpContract, ModelCreateDocument, ModelCreateParams,
    ModelDeleteParams, ModelDocument, ModelGetResult, ModelManifestBackend, ModelMutationResult,
    ModelSummary, ModelUpdateDocument, ModelUpdateParams, ModelsGetParams, ModelsListResult,
    ProjectCreateDocument, ProjectCreateParams, ProjectDeleteParams, ProjectDocument,
    ProjectGetResult, ProjectManifestBackend, ProjectMutationResult, ProjectSummary,
    ProjectUpdateDocument, ProjectUpdateParams, ProjectsGetParams, ProjectsListResult,
    RoutineCreateDocument, RoutineCreateParams, RoutineDeleteParams, RoutineDocument,
    RoutineEdgeInput, RoutineGetResult, RoutineGraphInput, RoutineManifestBackend,
    RoutineMutationResult, RoutineStepInput, RoutineSummary, RoutineUpdateDocument,
    RoutineUpdateParams, RoutinesGetParams, RoutinesListResult,
};
pub use policy::ManifestAccessPolicy;
pub use scope::{PlatformScope, ScopeAction, ScopeResource};
pub use types::{BootstrapManifestResponse, PlatformManifestItem, PlatformManifestWriteRequest};
