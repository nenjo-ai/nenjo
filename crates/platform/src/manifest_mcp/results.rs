//! Result payload types returned by manifest MCP tools.

use serde::{Deserialize, Serialize};

use nenjo::agents::prompts::PromptConfig;
use nenjo::types::{AbilityPromptConfig, DomainPromptConfig};

use crate::manifest_contract::KnowledgeDocumentEdgeRecord;

use super::types::{
    AbilityDocument, AbilityPromptDocument, AbilitySummary, AgentDocument, AgentPromptDocument,
    AgentSummary, ContextBlockContentDocument, ContextBlockDocument, ContextBlockSummary,
    CouncilDocument, CouncilSummary, DomainDocument, DomainPromptDocument, DomainSummary,
    KnowledgeDocSummary, KnowledgePackDocument, ModelDocument, ModelSummary, ProjectDocument,
    ProjectSummary, RoutineDocument, RoutineSummary,
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
/// Result for creating or updating a Library knowledge pack.
pub struct KnowledgePackMutationResult {
    pub knowledge_pack: KnowledgePackDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for creating or updating a library knowledge document.
///
/// `edges` contains canonical UUID-backed outbound edge records after an edge
/// replacement. It is empty when the mutation left edges unchanged or created a
/// document without related edges.
pub struct KnowledgeDocMutationResult {
    pub knowledge_doc: KnowledgeDocSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<KnowledgeDocumentEdgeRecord>,
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
