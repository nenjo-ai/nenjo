//! Result payload types returned by manifest MCP tools.

use serde::{Deserialize, Serialize};

use crate::manifest_contract::KnowledgeDocumentEdgeRecord;
use nenjo::manifest::CommandManifest;

use super::types::{
    AbilityDocument, AbilitySummary, AgentDocument, AgentSummary, CommandSummary,
    ContextBlockDocument, ContextBlockSummary, CouncilDocument, CouncilSummary, DomainDocument,
    DomainSummary, KnowledgeDocSummary, KnowledgePackDocument, ModelDocument, ModelSummary,
    ProjectDocument, ProjectSummary, RoutineDocument, RoutineSummary,
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
/// Result for `configure_agent`.
pub struct AgentConfigureResult {
    /// Manifest-facing agent document after all requested changes.
    pub agent: AgentDocument,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
/// Result for `configure_ability`.
pub struct AbilityConfigureResult {
    pub ability: AbilityDocument,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `list_commands`.
pub struct CommandsListResult {
    pub commands: Vec<CommandSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `get_command`.
pub struct CommandGetResult {
    pub command: CommandManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result for `configure_command`.
pub struct CommandConfigureResult {
    pub command: CommandManifest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
/// Result for `configure_domain`.
pub struct DomainConfigureResult {
    pub domain: DomainDocument,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

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
/// Result for `configure_routine`.
pub struct RoutineConfigureResult {
    pub routine: RoutineDocument,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
/// Result for `configure_context_block`.
pub struct ContextBlockConfigureResult {
    pub context_block: ContextBlockDocument,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
