//! Request parameter types for manifest MCP tools.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use nenjo::types::{AbilityPromptConfig, DomainPromptConfig};

use super::types::{
    AbilityCreateDocument, AbilityUpdateDocument, AgentCreateDocument, AgentUpdateDocument,
    ContextBlockCreateDocument, ContextBlockUpdateDocument, CouncilCreateDocument,
    CouncilCreateMemberDocument, CouncilMemberUpdateDocument, CouncilUpdateDocument,
    DomainCreateDocument, DomainUpdateDocument, ModelCreateDocument, ModelUpdateDocument,
    ProjectCreateDocument, ProjectDocumentCreateDocument, ProjectUpdateDocument,
    RoutineCreateDocument, RoutineUpdateDocument,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_agent`.
pub struct AgentsGetParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_agent_prompt`.
pub struct AgentPromptGetParams {
    /// Target agent ID.
    pub id: Uuid,
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
    /// Target agent ID.
    pub id: Uuid,
    #[serde(flatten)]
    pub data: AgentUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_agent_prompt`.
pub struct AgentPromptUpdateParams {
    /// Target agent ID.
    pub id: Uuid,
    /// Partial prompt configuration patch for this agent.
    #[serde(default)]
    pub prompt_config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_agent`.
pub struct AgentDeleteParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_ability`.
pub struct AbilitiesGetParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_ability_prompt`.
pub struct AbilityPromptGetParams {
    pub id: Uuid,
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
    pub id: Uuid,
    #[serde(flatten)]
    pub data: AbilityUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_ability_prompt`.
pub struct AbilityPromptUpdateParams {
    pub id: Uuid,
    pub prompt_config: AbilityPromptConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_ability`.
pub struct AbilityDeleteParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_domain`.
pub struct DomainsGetParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_domain_prompt`.
pub struct DomainPromptGetParams {
    pub id: Uuid,
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
    pub id: Uuid,
    #[serde(flatten)]
    pub data: DomainUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_domain_prompt`.
pub struct DomainPromptUpdateParams {
    pub id: Uuid,
    pub prompt_config: DomainPromptConfig,
}

/// Alias used by the current contract for domain prompt updates.
pub type DomainManifestUpdateParams = DomainPromptUpdateParams;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_domain`.
pub struct DomainDeleteParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_project`.
pub struct ProjectsGetParams {
    pub id: Uuid,
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
    pub id: Uuid,
    #[serde(flatten)]
    pub data: ProjectUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_project`.
pub struct ProjectDeleteParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `list_project_documents`.
pub struct ProjectDocumentsListParams {
    pub project_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `create_project_document`.
pub struct ProjectDocumentCreateParams {
    #[serde(flatten)]
    pub data: ProjectDocumentCreateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_project_document_content`.
pub struct ProjectDocumentContentUpdateParams {
    pub project_id: Uuid,
    pub document_id: Uuid,
    #[serde(alias = "content")]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_project_document`.
pub struct ProjectDocumentDeleteParams {
    pub project_id: Uuid,
    pub document_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_routine`.
pub struct RoutinesGetParams {
    pub id: Uuid,
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
    pub id: Uuid,
    #[serde(flatten)]
    pub data: RoutineUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_routine`.
pub struct RoutineDeleteParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_model`.
pub struct ModelsGetParams {
    pub id: Uuid,
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
    pub id: Uuid,
    #[serde(flatten)]
    pub data: ModelUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_model`.
pub struct ModelDeleteParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_council`.
pub struct CouncilsGetParams {
    pub id: Uuid,
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
    pub id: Uuid,
    #[serde(flatten)]
    pub data: CouncilUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_council`.
pub struct CouncilDeleteParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `add_council_member`.
pub struct CouncilAddMemberParams {
    pub council_id: Uuid,
    #[serde(flatten)]
    pub data: CouncilCreateMemberDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_council_member`.
pub struct CouncilUpdateMemberParams {
    pub council_id: Uuid,
    pub agent_id: Uuid,
    #[serde(flatten)]
    pub data: CouncilMemberUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `remove_council_member`.
pub struct CouncilRemoveMemberParams {
    pub council_id: Uuid,
    pub agent_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_context_block`.
pub struct ContextBlocksGetParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `get_context_block_content`.
pub struct ContextBlockContentGetParams {
    pub id: Uuid,
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
    pub id: Uuid,
    #[serde(flatten)]
    pub data: ContextBlockUpdateDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `update_context_block_content`.
pub struct ContextBlockContentUpdateParams {
    pub id: Uuid,
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Parameters for `delete_context_block`.
pub struct ContextBlockDeleteParams {
    pub id: Uuid,
}
