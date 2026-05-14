use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::params::{
    AbilitiesGetParams, AbilityCreateParams, AbilityDeleteParams, AbilityPromptGetParams,
    AbilityPromptUpdateParams, AbilityUpdateParams, AgentCreateParams, AgentDeleteParams,
    AgentPromptGetParams, AgentPromptUpdateParams, AgentUpdateParams, AgentsGetParams,
    ContextBlockContentGetParams, ContextBlockContentUpdateParams, ContextBlockCreateParams,
    ContextBlockDeleteParams, ContextBlockUpdateParams, ContextBlocksGetParams,
    CouncilAddMemberParams, CouncilCreateParams, CouncilDeleteParams, CouncilRemoveMemberParams,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, DomainCreateParams,
    DomainDeleteParams, DomainManifestGetParams, DomainManifestUpdateParams, DomainUpdateParams,
    DomainsGetParams, ModelCreateParams, ModelDeleteParams, ModelUpdateParams, ModelsGetParams,
    ProjectCreateParams, ProjectDeleteParams, ProjectDocumentContentUpdateParams,
    ProjectDocumentCreateParams, ProjectDocumentDeleteParams, ProjectUpdateParams,
    ProjectsGetParams, RoutineCreateParams, RoutineDeleteParams, RoutineUpdateParams,
    RoutinesGetParams,
};
use super::results::{
    AbilitiesListResult, AbilityGetResult, AbilityMutationResult, AbilityPromptGetResult,
    AbilityPromptMutationResult, AgentGetResult, AgentMutationResult, AgentPromptGetResult,
    AgentPromptMutationResult, AgentsListResult, ContextBlockContentGetResult,
    ContextBlockContentMutationResult, ContextBlockGetResult, ContextBlockMutationResult,
    ContextBlocksListResult, CouncilGetResult, CouncilMutationResult, CouncilsListResult,
    DeleteResult, DomainGetResult, DomainManifestGetResult, DomainManifestMutationResult,
    DomainMutationResult, DomainsListResult, ModelGetResult, ModelMutationResult, ModelsListResult,
    ProjectDocumentContentMutationResult, ProjectDocumentMutationResult, ProjectGetResult,
    ProjectMutationResult, ProjectsListResult, RoutineGetResult, RoutineMutationResult,
    RoutinesListResult,
};

#[async_trait]
/// Backend operations for generic knowledge pack resources.
pub trait KnowledgeManifestBackend: Send + Sync {
    /// List locally available knowledge packs.
    async fn list_knowledge_packs(&self) -> Result<Value>;
    /// List compact document metadata from one pack.
    async fn list_knowledge_docs(&self, params: Value) -> Result<Value>;
    /// Read one document manifest from one pack.
    async fn read_knowledge_doc_manifest(&self, params: Value) -> Result<Value>;
    /// Read one full document from one pack.
    async fn read_knowledge_doc(&self, params: Value) -> Result<Value>;
    /// Search one pack with document bodies.
    async fn search_knowledge(&self, params: Value) -> Result<Value>;
    /// Search one pack using metadata only.
    async fn search_knowledge_paths(&self, params: Value) -> Result<Value>;
    /// List one pack's document tree.
    async fn list_knowledge_tree(&self, params: Value) -> Result<Value>;
    /// List graph neighbors for one document in one pack.
    async fn list_knowledge_neighbors(&self, params: Value) -> Result<Value>;
}

#[async_trait]
/// Backend operations for agent manifest resources.
pub trait AgentManifestBackend: Send + Sync {
    /// List visible agents.
    async fn list_agents(&self) -> Result<AgentsListResult>;
    /// Fetch one agent by ID.
    async fn get_agent(&self, params: AgentsGetParams) -> Result<AgentGetResult>;
    /// Fetch one agent's prompt document.
    async fn get_agent_prompt(&self, params: AgentPromptGetParams) -> Result<AgentPromptGetResult>;
    /// Create a new agent.
    async fn create_agent(&self, params: AgentCreateParams) -> Result<AgentMutationResult>;
    /// Update agent metadata.
    async fn update_agent(&self, params: AgentUpdateParams) -> Result<AgentMutationResult>;
    /// Update an agent prompt document.
    async fn update_agent_prompt(
        &self,
        params: AgentPromptUpdateParams,
    ) -> Result<AgentPromptMutationResult>;
    /// Delete an agent.
    async fn delete_agent(&self, params: AgentDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for ability manifest resources.
pub trait AbilityManifestBackend: Send + Sync {
    /// List visible abilities.
    async fn list_abilities(&self) -> Result<AbilitiesListResult>;
    /// Fetch one ability by ID.
    async fn get_ability(&self, params: AbilitiesGetParams) -> Result<AbilityGetResult>;
    /// Fetch one ability's prompt document.
    async fn get_ability_prompt(
        &self,
        params: AbilityPromptGetParams,
    ) -> Result<AbilityPromptGetResult>;
    /// Create a new ability.
    async fn create_ability(&self, params: AbilityCreateParams) -> Result<AbilityMutationResult>;
    /// Update an existing ability.
    async fn update_ability(&self, params: AbilityUpdateParams) -> Result<AbilityMutationResult>;
    /// Update an ability prompt document.
    async fn update_ability_prompt(
        &self,
        params: AbilityPromptUpdateParams,
    ) -> Result<AbilityPromptMutationResult>;
    /// Delete an ability.
    async fn delete_ability(&self, params: AbilityDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for domain manifest resources.
pub trait DomainManifestBackend: Send + Sync {
    /// List visible domains.
    async fn list_domains(&self) -> Result<DomainsListResult>;
    /// Fetch one domain by ID.
    async fn get_domain(&self, params: DomainsGetParams) -> Result<DomainGetResult>;
    /// Fetch one domain prompt/manifest document.
    async fn get_domain_prompt(
        &self,
        params: DomainManifestGetParams,
    ) -> Result<DomainManifestGetResult>;
    /// Create a new domain.
    async fn create_domain(&self, params: DomainCreateParams) -> Result<DomainMutationResult>;
    /// Update an existing domain.
    async fn update_domain(&self, params: DomainUpdateParams) -> Result<DomainMutationResult>;
    /// Update a domain prompt/manifest document.
    async fn update_domain_prompt(
        &self,
        params: DomainManifestUpdateParams,
    ) -> Result<DomainManifestMutationResult>;
    /// Delete a domain.
    async fn delete_domain(&self, params: DomainDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for project manifest resources and library knowledge items.
pub trait ProjectManifestBackend: Send + Sync {
    /// List visible projects.
    async fn list_projects(&self) -> Result<ProjectsListResult>;
    /// Fetch one project by ID.
    async fn get_project(&self, params: ProjectsGetParams) -> Result<ProjectGetResult>;
    /// Create a new project.
    async fn create_project(&self, params: ProjectCreateParams) -> Result<ProjectMutationResult>;
    /// Update an existing project.
    async fn update_project(&self, params: ProjectUpdateParams) -> Result<ProjectMutationResult>;
    /// Delete a project.
    async fn delete_project(&self, params: ProjectDeleteParams) -> Result<DeleteResult>;
    /// Create a library knowledge item.
    async fn create_project_document(
        &self,
        params: ProjectDocumentCreateParams,
    ) -> Result<ProjectDocumentMutationResult>;
    /// Update a library knowledge item's content.
    async fn update_project_document_content(
        &self,
        params: ProjectDocumentContentUpdateParams,
    ) -> Result<ProjectDocumentContentMutationResult>;
    /// Delete a library knowledge item.
    async fn delete_project_document(
        &self,
        params: ProjectDocumentDeleteParams,
    ) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for routine manifest resources.
pub trait RoutineManifestBackend: Send + Sync {
    /// List visible routines.
    async fn list_routines(&self) -> Result<RoutinesListResult>;
    /// Fetch one routine by ID.
    async fn get_routine(&self, params: RoutinesGetParams) -> Result<RoutineGetResult>;
    /// Create a new routine.
    async fn create_routine(&self, params: RoutineCreateParams) -> Result<RoutineMutationResult>;
    /// Update an existing routine.
    async fn update_routine(&self, params: RoutineUpdateParams) -> Result<RoutineMutationResult>;
    /// Delete a routine.
    async fn delete_routine(&self, params: RoutineDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for model manifest resources.
pub trait ModelManifestBackend: Send + Sync {
    /// List visible models.
    async fn list_models(&self) -> Result<ModelsListResult>;
    /// Fetch one model by ID.
    async fn get_model(&self, params: ModelsGetParams) -> Result<ModelGetResult>;
    /// Create a new model.
    async fn create_model(&self, params: ModelCreateParams) -> Result<ModelMutationResult>;
    /// Update an existing model.
    async fn update_model(&self, params: ModelUpdateParams) -> Result<ModelMutationResult>;
    /// Delete a model.
    async fn delete_model(&self, params: ModelDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for council manifest resources.
pub trait CouncilManifestBackend: Send + Sync {
    /// List visible councils.
    async fn list_councils(&self) -> Result<CouncilsListResult>;
    /// Fetch one council by ID.
    async fn get_council(&self, params: CouncilsGetParams) -> Result<CouncilGetResult>;
    /// Create a new council.
    async fn create_council(&self, params: CouncilCreateParams) -> Result<CouncilMutationResult>;
    /// Update an existing council.
    async fn update_council(&self, params: CouncilUpdateParams) -> Result<CouncilMutationResult>;
    /// Add a member to a council.
    async fn add_council_member(
        &self,
        params: CouncilAddMemberParams,
    ) -> Result<CouncilMutationResult>;
    /// Update one council member.
    async fn update_council_member(
        &self,
        params: CouncilUpdateMemberParams,
    ) -> Result<CouncilMutationResult>;
    /// Remove a member from a council.
    async fn remove_council_member(
        &self,
        params: CouncilRemoveMemberParams,
    ) -> Result<CouncilMutationResult>;
    /// Delete a council.
    async fn delete_council(&self, params: CouncilDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for context block manifest resources.
pub trait ContextBlockManifestBackend: Send + Sync {
    /// List visible context blocks.
    async fn list_context_blocks(&self) -> Result<ContextBlocksListResult>;
    /// Fetch one context block by ID.
    async fn get_context_block(
        &self,
        params: ContextBlocksGetParams,
    ) -> Result<ContextBlockGetResult>;
    /// Fetch one context block's template content.
    async fn get_context_block_content(
        &self,
        params: ContextBlockContentGetParams,
    ) -> Result<ContextBlockContentGetResult>;
    /// Create a new context block.
    async fn create_context_block(
        &self,
        params: ContextBlockCreateParams,
    ) -> Result<ContextBlockMutationResult>;
    /// Update a context block metadata record.
    async fn update_context_block(
        &self,
        params: ContextBlockUpdateParams,
    ) -> Result<ContextBlockMutationResult>;
    /// Update a context block template.
    async fn update_context_block_content(
        &self,
        params: ContextBlockContentUpdateParams,
    ) -> Result<ContextBlockContentMutationResult>;
    /// Delete a context block.
    async fn delete_context_block(&self, params: ContextBlockDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend interface consumed by [`ManifestMcpContract`](super::ManifestMcpContract).
///
/// Implementations can serve manifest operations from local state, a remote service, or a
/// write-through cache that does both.
pub trait ManifestMcpBackend:
    AgentManifestBackend
    + AbilityManifestBackend
    + DomainManifestBackend
    + KnowledgeManifestBackend
    + ProjectManifestBackend
    + RoutineManifestBackend
    + ModelManifestBackend
    + CouncilManifestBackend
    + ContextBlockManifestBackend
    + Send
    + Sync
{
}

impl<T> ManifestMcpBackend for T where
    T: AgentManifestBackend
        + AbilityManifestBackend
        + DomainManifestBackend
        + KnowledgeManifestBackend
        + ProjectManifestBackend
        + RoutineManifestBackend
        + ModelManifestBackend
        + CouncilManifestBackend
        + ContextBlockManifestBackend
        + Send
        + Sync
{
}
