//! Result payload types returned by manifest MCP tools.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use nenjo::agents::prompts::PromptConfig;
use nenjo::types::{AbilityPromptConfig, DomainPromptConfig};

use super::types::{
    AbilityDocument, AbilityPromptDocument, AbilitySummary, AgentDocument, AgentPromptDocument,
    AgentSummary, ContextBlockContentDocument, ContextBlockDocument, ContextBlockSummary,
    CouncilDocument, CouncilSummary, DomainDocument, DomainPromptDocument, DomainSummary,
    ModelDocument, ModelSummary, ProjectDocument, ProjectDocumentContentDocument,
    ProjectDocumentSummary, ProjectSummary, RoutineDocument, RoutineSummary,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_agents`.
pub struct AgentsListResult {
    pub agents: Vec<AgentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_agent`.
pub struct AgentGetResult {
    pub agent: AgentDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_agent_prompt`.
pub struct AgentPromptGetResult {
    pub agent: AgentPromptDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `update_agent`.
pub struct AgentMutationResult {
    /// Canonical agent state after an agent metadata update.
    pub agent: AgentDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `update_agent_prompt`.
pub struct AgentPromptMutationResult {
    /// Canonical prompt configuration for the updated agent.
    pub prompt_config: PromptConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for delete operations.
pub struct DeleteResult {
    pub deleted: bool,
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_abilities`.
pub struct AbilitiesListResult {
    pub abilities: Vec<AbilitySummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_ability`.
pub struct AbilityGetResult {
    pub ability: AbilityDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `create_ability` and `update_ability`.
pub struct AbilityMutationResult {
    pub ability: AbilityDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_ability_prompt`.
pub struct AbilityPromptGetResult {
    pub ability: AbilityPromptDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `update_ability_prompt`.
pub struct AbilityPromptMutationResult {
    pub prompt_config: AbilityPromptConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_domains`.
pub struct DomainsListResult {
    pub domains: Vec<DomainSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_domain`.
pub struct DomainGetResult {
    pub domain: DomainDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `create_domain` and `update_domain`.
pub struct DomainMutationResult {
    pub domain: DomainDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_domain_prompt`.
pub struct DomainPromptGetResult {
    pub domain: DomainPromptDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `update_domain_prompt`.
pub struct DomainPromptMutationResult {
    pub prompt_config: DomainPromptConfig,
}

/// Alias used by the current contract for domain prompt retrieval.
pub type DomainManifestGetResult = DomainPromptGetResult;
/// Alias used by the current contract for domain prompt updates.
pub type DomainManifestMutationResult = DomainPromptMutationResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_projects`.
pub struct ProjectsListResult {
    pub projects: Vec<ProjectSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_project`.
pub struct ProjectGetResult {
    pub project: ProjectDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `create_project` and `update_project`.
pub struct ProjectMutationResult {
    pub project: ProjectDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_project_documents`.
pub struct ProjectDocumentsListResult {
    pub project_documents: Vec<ProjectDocumentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_project_document`.
pub struct ProjectDocumentGetResult {
    pub project_document: ProjectDocumentSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `create_project_document`.
pub struct ProjectDocumentMutationResult {
    pub project_document: ProjectDocumentSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_project_document_content`.
pub struct ProjectDocumentContentGetResult {
    pub project_document: ProjectDocumentContentDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `update_project_document_content`.
pub struct ProjectDocumentContentMutationResult {
    pub project_document: ProjectDocumentSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_routines`.
pub struct RoutinesListResult {
    pub routines: Vec<RoutineSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_routine`.
pub struct RoutineGetResult {
    pub routine: RoutineDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `create_routine` and `update_routine`.
pub struct RoutineMutationResult {
    pub routine: RoutineDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_models`.
pub struct ModelsListResult {
    pub models: Vec<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_model`.
pub struct ModelGetResult {
    pub model: ModelDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `create_model` and `update_model`.
pub struct ModelMutationResult {
    pub model: ModelDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_councils`.
pub struct CouncilsListResult {
    pub councils: Vec<CouncilSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_council`.
pub struct CouncilGetResult {
    pub council: CouncilDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for council mutations.
pub struct CouncilMutationResult {
    pub council: CouncilDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_context_blocks`.
pub struct ContextBlocksListResult {
    pub context_blocks: Vec<ContextBlockSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_context_block`.
pub struct ContextBlockGetResult {
    pub context_block: ContextBlockDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `create_context_block` and `update_context_block`.
pub struct ContextBlockMutationResult {
    pub context_block: ContextBlockDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_context_block_content`.
pub struct ContextBlockContentGetResult {
    pub context_block: ContextBlockContentDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `update_context_block_content`.
pub struct ContextBlockContentMutationResult {
    pub template: String,
}
