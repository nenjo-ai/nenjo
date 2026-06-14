//! Request parameter types for manifest MCP tools.

use serde::{Deserialize, Serialize};

use super::types::{
    AbilityConfigureDocument, AgentConfigureDocument, ContextBlockConfigureDocument,
    CouncilCreateDocument, CouncilCreateMemberDocument, CouncilMemberUpdateDocument,
    CouncilUpdateDocument, DomainConfigureDocument, KnowledgeDocCreateDocument,
    KnowledgeDocUpdateDocument, KnowledgePackCreateDocument, KnowledgePackUpdateDocument,
    ModelCreateDocument, ModelUpdateDocument, ProjectCreateDocument, ProjectUpdateDocument,
    RoutineConfigureDocument,
};
use nenjo::Slug;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_agent`.
pub struct AgentsGetParams {
    pub agent: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `configure_agent`.
pub struct AgentConfigureParams {
    #[serde(flatten)]
    pub data: AgentConfigureDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_ability`.
pub struct AbilitiesGetParams {
    pub ability: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `configure_ability`.
pub struct AbilityConfigureParams {
    #[serde(flatten)]
    pub data: AbilityConfigureDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_domain`.
pub struct DomainsGetParams {
    pub domain: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `configure_domain`.
pub struct DomainConfigureParams {
    #[serde(flatten)]
    pub data: DomainConfigureDocument,
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
/// Parameters for `configure_routine`.
pub struct RoutineConfigureParams {
    #[serde(flatten)]
    pub data: RoutineConfigureDocument,
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
/// Parameters for `configure_context_block`.
pub struct ContextBlockConfigureParams {
    #[serde(flatten)]
    pub data: ContextBlockConfigureDocument,
}
