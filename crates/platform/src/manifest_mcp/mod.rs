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
/// Tool specs for slash command operations.
pub mod commands;
/// Tool specs for context block operations.
pub mod context_blocks;
/// Dispatch helpers for the manifest MCP contract.
pub mod contract;
/// Tool specs for council operations.
pub mod councils;
/// Tool specs for domain operations.
pub mod domains;
/// Tool specs for library knowledge document mutations.
pub mod library;
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
mod serde_helpers;
/// Helpers for assembling the full manifest tool set.
pub mod tools;
mod types;

pub use backend::{
    AbilityManifestBackend, AgentManifestBackend, CommandManifestBackend,
    ContextBlockManifestBackend, CouncilManifestBackend, DomainManifestBackend,
    LibraryManifestBackend, ManifestMcpBackend, ModelManifestBackend, ProjectManifestBackend,
    RoutineManifestBackend,
};
pub use contract::ManifestMcpContract;
pub use params::{
    AbilitiesGetParams, AbilityConfigureParams, AgentConfigureParams, AgentsGetParams,
    CommandConfigureParams, CommandsGetParams, ContextBlockConfigureParams, ContextBlocksGetParams,
    CouncilAddMemberParams, CouncilCreateParams, CouncilDeleteParams, CouncilRemoveMemberParams,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, DomainConfigureParams,
    DomainsGetParams, KnowledgeDocCreateParams, KnowledgeDocDeleteParams, KnowledgeDocUpdateParams,
    KnowledgePackCreateParams, KnowledgePackUpdateParams, ModelCreateParams, ModelDeleteParams,
    ModelUpdateParams, ModelsGetParams, ProjectCreateParams, ProjectDeleteParams,
    ProjectUpdateParams, ProjectsGetParams, RoutineConfigureParams, RoutineDeleteParams,
    RoutinesGetParams,
};
pub use results::{
    AbilitiesListResult, AbilityConfigureResult, AbilityGetResult, AgentConfigureResult,
    AgentGetResult, AgentsListResult, CommandConfigureResult, CommandGetResult, CommandsListResult,
    ContextBlockConfigureResult, ContextBlockGetResult, ContextBlocksListResult, CouncilGetResult,
    CouncilMutationResult, CouncilsListResult, DeleteResult, DomainConfigureResult,
    DomainGetResult, DomainsListResult, KnowledgeDocMutationResult, KnowledgePackMutationResult,
    ModelGetResult, ModelMutationResult, ModelsListResult, ProjectGetResult, ProjectMutationResult,
    ProjectsListResult, RoutineConfigureResult, RoutineGetResult, RoutinesListResult,
};
pub use types::{
    AbilityConfigureAssignments, AbilityConfigureDocument, AbilityConfigureMetadata,
    AbilityDocument, AbilitySummary, AgentConfigureAssignments, AgentConfigureDocument,
    AgentConfigureMetadata, AgentDocument, AgentHeartbeatConfigureDocument, AgentSummary,
    CommandConfigureDocument, CommandConfigureMetadata, CommandSummary,
    ContextBlockConfigureDocument, ContextBlockConfigureMetadata, ContextBlockDocument,
    ContextBlockSummary, CouncilCreateDocument, CouncilCreateMemberDocument, CouncilDocument,
    CouncilMemberDocument, CouncilMemberUpdateDocument, CouncilSummary, CouncilUpdateDocument,
    DomainConfigureAssignments, DomainConfigureDocument, DomainConfigureMetadata, DomainDocument,
    DomainSummary, KnowledgeDocContentDocument, KnowledgeDocCreateDocument,
    KnowledgeDocRelatedDocument, KnowledgeDocSummary, KnowledgeDocUpdateDocument,
    KnowledgePackCreateDocument, KnowledgePackDocument, KnowledgePackUpdateDocument,
    ModelCreateDocument, ModelDocument, ModelSummary, ModelUpdateDocument, ProjectCreateDocument,
    ProjectDocument, ProjectSummary, ProjectUpdateDocument, RoutineConfigureDocument,
    RoutineConfigureMetadata, RoutineCronTaskConfigureDocument, RoutineDocument,
    RoutineEdgeDocument, RoutineEdgeInput, RoutineGraphInput, RoutineStepConfigInput,
    RoutineStepDocument, RoutineStepInput, RoutineSummary,
};
