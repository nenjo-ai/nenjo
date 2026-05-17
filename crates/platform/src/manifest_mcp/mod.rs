//! Manifest MCP contract surface.
//!
//! This module contains the tool registry, request/response payloads, and backend trait used by
//! the manifest MCP server implementation. The public re-exports here are intended to be the
//! stable entry point for consumers wiring the contract into a runtime.

/// Tool specs for ability operations.
pub mod abilities;
/// Tool specs for agent operations.
pub mod agents;
/// Backend trait implemented by manifest MCP executors.
pub mod backend;
/// Tool specs for context block operations.
pub mod context_blocks;
/// Dispatch helpers for the manifest MCP contract.
pub mod contract;
/// Tool specs for council operations.
pub mod councils;
/// Tool specs for domain operations.
pub mod domains;
/// Tool specs for model operations.
pub mod models;
/// Request parameter types for manifest MCP tools.
pub mod params;
/// Tool specs for project operations.
pub mod projects;
/// Result payload types for manifest MCP tools.
pub mod results;
/// Tool specs for routine operations.
pub mod routines;
/// Helpers for assembling the full manifest tool set.
pub mod tools;
mod types;

pub use backend::{
    AbilityManifestBackend, AgentManifestBackend, ContextBlockManifestBackend,
    CouncilManifestBackend, DomainManifestBackend, KnowledgeManifestBackend, ManifestMcpBackend,
    ModelManifestBackend, ProjectManifestBackend, RoutineManifestBackend,
};
pub use contract::ManifestMcpContract;
pub use params::{
    AbilitiesGetParams, AbilityCreateParams, AbilityDeleteParams, AbilityPromptGetParams,
    AbilityPromptUpdateParams, AbilityUpdateParams, AgentCreateParams, AgentDeleteParams,
    AgentPromptGetParams, AgentPromptUpdateParams, AgentUpdateParams, AgentsGetParams,
    ContextBlockContentGetParams, ContextBlockContentUpdateParams, ContextBlockCreateParams,
    ContextBlockDeleteParams, ContextBlockUpdateParams, ContextBlocksGetParams,
    CouncilAddMemberParams, CouncilCreateParams, CouncilDeleteParams, CouncilRemoveMemberParams,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, DomainCreateParams,
    DomainDeleteParams, DomainManifestGetParams, DomainManifestUpdateParams, DomainPromptGetParams,
    DomainPromptUpdateParams, DomainUpdateParams, DomainsGetParams,
    KnowledgeItemContentUpdateParams, KnowledgeItemCreateParams, KnowledgeItemDeleteParams,
    ModelCreateParams, ModelDeleteParams, ModelUpdateParams, ModelsGetParams, ProjectCreateParams,
    ProjectDeleteParams, ProjectUpdateParams, ProjectsGetParams, RoutineCreateParams,
    RoutineDeleteParams, RoutineUpdateParams, RoutinesGetParams,
};
pub use results::{
    AbilitiesListResult, AbilityGetResult, AbilityMutationResult, AbilityPromptGetResult,
    AbilityPromptMutationResult, AgentGetResult, AgentMutationResult, AgentPromptGetResult,
    AgentPromptMutationResult, AgentsListResult, ContextBlockContentGetResult,
    ContextBlockContentMutationResult, ContextBlockGetResult, ContextBlockMutationResult,
    ContextBlocksListResult, CouncilGetResult, CouncilMutationResult, CouncilsListResult,
    DeleteResult, DomainGetResult, DomainManifestGetResult, DomainManifestMutationResult,
    DomainMutationResult, DomainPromptGetResult, DomainPromptMutationResult, DomainsListResult,
    KnowledgeItemContentMutationResult, KnowledgeItemMutationResult, ModelGetResult,
    ModelMutationResult, ModelsListResult, ProjectGetResult, ProjectMutationResult,
    ProjectsListResult, RoutineGetResult, RoutineMutationResult, RoutinesListResult,
};
pub use types::{
    AbilityCreateDocument, AbilityDocument, AbilityPromptDocument, AbilitySummary,
    AbilityUpdateDocument, AgentCreateDocument, AgentDocument, AgentPromptDocument, AgentSummary,
    AgentUpdateDocument, ContextBlockContentDocument, ContextBlockCreateDocument,
    ContextBlockDocument, ContextBlockSummary, ContextBlockUpdateDocument, CouncilCreateDocument,
    CouncilCreateMemberDocument, CouncilDocument, CouncilMemberDocument,
    CouncilMemberUpdateDocument, CouncilSummary, CouncilUpdateDocument, DomainCreateDocument,
    DomainDocument, DomainManifestDocument, DomainPromptDocument, DomainSummary,
    DomainUpdateDocument, KnowledgeItemContentDocument, KnowledgeItemCreateDocument,
    KnowledgeItemSummary, ModelCreateDocument, ModelDocument, ModelSummary, ModelUpdateDocument,
    ProjectCreateDocument, ProjectDocument, ProjectSummary, ProjectUpdateDocument,
    RoutineCreateDocument, RoutineDocument, RoutineEdgeInput, RoutineGraphInput, RoutineStepInput,
    RoutineSummary, RoutineUpdateDocument,
};
