//! Request parameter types for manifest MCP tools.

use serde::{Deserialize, Serialize};

use nenjo::types::{AbilityPromptConfig, DomainPromptConfig};

use super::types::{
    AbilityCreateDocument, AbilityUpdateDocument, AgentCreateDocument, AgentUpdateDocument,
    ContextBlockCreateDocument, ContextBlockUpdateDocument, CouncilCreateDocument,
    CouncilCreateMemberDocument, CouncilMemberUpdateDocument, CouncilUpdateDocument,
    DomainCreateDocument, DomainUpdateDocument, KnowledgeDocCreateDocument,
    KnowledgeDocUpdateDocument, KnowledgePackCreateDocument, KnowledgePackUpdateDocument,
    ModelCreateDocument, ModelUpdateDocument, ProjectCreateDocument, ProjectUpdateDocument,
    RoutineCreateDocument, RoutineUpdateDocument,
};
use nenjo::Slug;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_agent`.
pub struct AgentsGetParams {
    pub agent: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_agent_prompt`.
pub struct AgentPromptGetParams {
    /// Target agent slug.
    pub agent: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_agent`.
pub struct AgentCreateParams {
    #[serde(flatten)]
    pub data: AgentCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_agent`.
pub struct AgentUpdateParams {
    /// Target agent slug.
    pub agent: Slug,
    #[serde(flatten)]
    pub data: AgentUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_agent_prompt`.
pub struct AgentPromptUpdateParams {
    /// Target agent slug.
    pub agent: Slug,
    /// Partial prompt configuration patch for this agent.
    #[serde(default)]
    pub prompt_config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_agent`.
pub struct AgentDeleteParams {
    pub agent: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_ability`.
pub struct AbilitiesGetParams {
    pub ability: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_ability_prompt`.
pub struct AbilityPromptGetParams {
    pub ability: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_ability`.
pub struct AbilityCreateParams {
    #[serde(flatten)]
    pub data: AbilityCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_ability`.
pub struct AbilityUpdateParams {
    pub ability: Slug,
    #[serde(flatten)]
    pub data: AbilityUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_ability_prompt`.
pub struct AbilityPromptUpdateParams {
    pub ability: Slug,
    pub prompt_config: AbilityPromptConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_ability`.
pub struct AbilityDeleteParams {
    pub ability: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_domain`.
pub struct DomainsGetParams {
    pub domain: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_domain_prompt`.
pub struct DomainPromptGetParams {
    pub domain: Slug,
}

/// Alias used by the current contract for domain prompt retrieval.
pub type DomainManifestGetParams = DomainPromptGetParams;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_domain`.
pub struct DomainCreateParams {
    #[serde(flatten)]
    pub data: DomainCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_domain`.
pub struct DomainUpdateParams {
    pub domain: Slug,
    #[serde(flatten)]
    pub data: DomainUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_domain_prompt`.
pub struct DomainPromptUpdateParams {
    pub domain: Slug,
    pub prompt_config: DomainPromptConfig,
}

/// Alias used by the current contract for domain prompt updates.
pub type DomainManifestUpdateParams = DomainPromptUpdateParams;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_domain`.
pub struct DomainDeleteParams {
    pub domain: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_project`.
pub struct ProjectsGetParams {
    pub project: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_project`.
pub struct ProjectCreateParams {
    #[serde(flatten)]
    pub data: ProjectCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_project`.
pub struct ProjectUpdateParams {
    pub project: Slug,
    #[serde(flatten)]
    pub data: ProjectUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_project`.
pub struct ProjectDeleteParams {
    pub project: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_knowledge_pack`.
pub struct KnowledgePackCreateParams {
    #[serde(flatten)]
    pub data: KnowledgePackCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_knowledge_pack`.
pub struct KnowledgePackUpdateParams {
    #[serde(
        deserialize_with = "crate::manifest_mcp::serde_helpers::deserialize_library_pack_slug"
    )]
    pub pack: Slug,
    #[serde(flatten)]
    pub data: KnowledgePackUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_knowledge_doc`.
pub struct KnowledgeDocCreateParams {
    #[serde(flatten)]
    pub data: KnowledgeDocCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_knowledge_doc`.
pub struct KnowledgeDocUpdateParams {
    #[serde(
        deserialize_with = "crate::manifest_mcp::serde_helpers::deserialize_library_pack_slug"
    )]
    pub pack: Slug,
    pub slug: Slug,
    #[serde(flatten)]
    pub data: KnowledgeDocUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_knowledge_doc`.
pub struct KnowledgeDocDeleteParams {
    #[serde(
        deserialize_with = "crate::manifest_mcp::serde_helpers::deserialize_library_pack_slug"
    )]
    pub pack: Slug,
    pub slug: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_routine`.
pub struct RoutinesGetParams {
    pub slug: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_routine`.
pub struct RoutineCreateParams {
    #[serde(flatten)]
    pub data: RoutineCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_routine`.
pub struct RoutineUpdateParams {
    pub slug: Slug,
    #[serde(flatten)]
    pub data: RoutineUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_routine`.
pub struct RoutineDeleteParams {
    pub slug: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_model`.
pub struct ModelsGetParams {
    pub model: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_model`.
pub struct ModelCreateParams {
    #[serde(flatten)]
    pub data: ModelCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_model`.
pub struct ModelUpdateParams {
    pub model: Slug,
    #[serde(flatten)]
    pub data: ModelUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_model`.
pub struct ModelDeleteParams {
    pub model: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_council`.
pub struct CouncilsGetParams {
    pub council: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_council`.
pub struct CouncilCreateParams {
    #[serde(flatten)]
    pub data: CouncilCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_council`.
pub struct CouncilUpdateParams {
    pub council: Slug,
    #[serde(flatten)]
    pub data: CouncilUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_council`.
pub struct CouncilDeleteParams {
    pub council: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `add_council_member`.
pub struct CouncilAddMemberParams {
    pub council: Slug,
    #[serde(flatten)]
    pub data: CouncilCreateMemberDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_council_member`.
pub struct CouncilUpdateMemberParams {
    pub council: Slug,
    pub agent: Slug,
    #[serde(flatten)]
    pub data: CouncilMemberUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `remove_council_member`.
pub struct CouncilRemoveMemberParams {
    pub council: Slug,
    pub agent: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_context_block`.
pub struct ContextBlocksGetParams {
    pub context_block: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_context_block_content`.
pub struct ContextBlockContentGetParams {
    pub context_block: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_context_block`.
pub struct ContextBlockCreateParams {
    #[serde(flatten)]
    pub data: ContextBlockCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_context_block`.
pub struct ContextBlockUpdateParams {
    pub context_block: Slug,
    #[serde(flatten)]
    pub data: ContextBlockUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_context_block_content`.
pub struct ContextBlockContentUpdateParams {
    pub context_block: Slug,
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_context_block`.
pub struct ContextBlockDeleteParams {
    pub context_block: Slug,
}
