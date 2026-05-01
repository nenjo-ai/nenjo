use anyhow::Result;
use async_trait::async_trait;

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
    ProjectCreateParams, ProjectDeleteParams, ProjectDocumentContentGetParams,
    ProjectDocumentContentUpdateParams, ProjectDocumentCreateParams, ProjectDocumentDeleteParams,
    ProjectDocumentGetParams, ProjectDocumentsListParams, ProjectUpdateParams, ProjectsGetParams,
    RoutineCreateParams, RoutineDeleteParams, RoutineUpdateParams, RoutinesGetParams,
};
use super::results::{
    AbilitiesListResult, AbilityGetResult, AbilityMutationResult, AbilityPromptGetResult,
    AbilityPromptMutationResult, AgentGetResult, AgentMutationResult, AgentPromptGetResult,
    AgentPromptMutationResult, AgentsListResult, ContextBlockContentGetResult,
    ContextBlockContentMutationResult, ContextBlockGetResult, ContextBlockMutationResult,
    ContextBlocksListResult, CouncilGetResult, CouncilMutationResult, CouncilsListResult,
    DeleteResult, DomainGetResult, DomainManifestGetResult, DomainManifestMutationResult,
    DomainMutationResult, DomainsListResult, ModelGetResult, ModelMutationResult, ModelsListResult,
    ProjectDocumentContentGetResult, ProjectDocumentContentMutationResult,
    ProjectDocumentGetResult, ProjectDocumentMutationResult, ProjectDocumentsListResult,
    ProjectGetResult, ProjectMutationResult, ProjectsListResult, RoutineGetResult,
    RoutineMutationResult, RoutinesListResult,
};

#[async_trait]
/// Backend interface consumed by [`ManifestMcpContract`](super::ManifestMcpContract).
///
/// Implementations can serve manifest operations from local state, a remote service, or a
/// write-through cache that does both.
pub trait ManifestMcpBackend: Send + Sync {
    /// List visible agents.
    async fn agents_list(&self) -> Result<AgentsListResult>;
    /// Fetch one agent by ID.
    async fn agents_get(&self, params: AgentsGetParams) -> Result<AgentGetResult>;
    /// Fetch one agent's prompt document.
    async fn agents_get_prompt(&self, params: AgentPromptGetParams)
    -> Result<AgentPromptGetResult>;
    /// Create a new agent.
    async fn agents_create(&self, params: AgentCreateParams) -> Result<AgentMutationResult>;
    /// Update agent metadata.
    async fn agents_update(&self, params: AgentUpdateParams) -> Result<AgentMutationResult>;
    /// Update an agent prompt document.
    async fn agents_update_prompt(
        &self,
        params: AgentPromptUpdateParams,
    ) -> Result<AgentPromptMutationResult>;
    /// Delete an agent.
    async fn agents_delete(&self, params: AgentDeleteParams) -> Result<DeleteResult>;
    /// List visible abilities.
    async fn abilities_list(&self) -> Result<AbilitiesListResult>;
    /// Fetch one ability by ID.
    async fn abilities_get(&self, params: AbilitiesGetParams) -> Result<AbilityGetResult>;
    /// Fetch one ability's prompt document.
    async fn abilities_get_prompt(
        &self,
        params: AbilityPromptGetParams,
    ) -> Result<AbilityPromptGetResult>;
    /// Create a new ability.
    async fn abilities_create(&self, params: AbilityCreateParams) -> Result<AbilityMutationResult>;
    /// Update an existing ability.
    async fn abilities_update(&self, params: AbilityUpdateParams) -> Result<AbilityMutationResult>;
    /// Update an ability prompt document.
    async fn abilities_update_prompt(
        &self,
        params: AbilityPromptUpdateParams,
    ) -> Result<AbilityPromptMutationResult>;
    /// Delete an ability.
    async fn abilities_delete(&self, params: AbilityDeleteParams) -> Result<DeleteResult>;
    /// List visible domains.
    async fn domains_list(&self) -> Result<DomainsListResult>;
    /// Fetch one domain by ID.
    async fn domains_get(&self, params: DomainsGetParams) -> Result<DomainGetResult>;
    /// Fetch one domain prompt/manifest document.
    async fn domains_get_manifest(
        &self,
        params: DomainManifestGetParams,
    ) -> Result<DomainManifestGetResult>;
    /// Create a new domain.
    async fn domains_create(&self, params: DomainCreateParams) -> Result<DomainMutationResult>;
    /// Update an existing domain.
    async fn domains_update(&self, params: DomainUpdateParams) -> Result<DomainMutationResult>;
    /// Update a domain prompt/manifest document.
    async fn domains_update_manifest(
        &self,
        params: DomainManifestUpdateParams,
    ) -> Result<DomainManifestMutationResult>;
    /// Delete a domain.
    async fn domains_delete(&self, params: DomainDeleteParams) -> Result<DeleteResult>;
    /// List visible projects.
    async fn projects_list(&self) -> Result<ProjectsListResult>;
    /// Fetch one project by ID.
    async fn projects_get(&self, params: ProjectsGetParams) -> Result<ProjectGetResult>;
    /// Create a new project.
    async fn projects_create(&self, params: ProjectCreateParams) -> Result<ProjectMutationResult>;
    /// Update an existing project.
    async fn projects_update(&self, params: ProjectUpdateParams) -> Result<ProjectMutationResult>;
    /// Delete a project.
    async fn projects_delete(&self, params: ProjectDeleteParams) -> Result<DeleteResult>;
    /// List project documents for one project.
    async fn project_documents_list(
        &self,
        params: ProjectDocumentsListParams,
    ) -> Result<ProjectDocumentsListResult>;
    /// Fetch one project document metadata record.
    async fn project_documents_get(
        &self,
        params: ProjectDocumentGetParams,
    ) -> Result<ProjectDocumentGetResult>;
    /// Fetch one project document's content payload.
    async fn project_documents_get_content(
        &self,
        params: ProjectDocumentContentGetParams,
    ) -> Result<ProjectDocumentContentGetResult>;
    /// Create a project document.
    async fn project_documents_create(
        &self,
        params: ProjectDocumentCreateParams,
    ) -> Result<ProjectDocumentMutationResult>;
    /// Update a project document's content.
    async fn project_documents_update_content(
        &self,
        params: ProjectDocumentContentUpdateParams,
    ) -> Result<ProjectDocumentContentMutationResult>;
    /// Delete a project document.
    async fn project_documents_delete(
        &self,
        params: ProjectDocumentDeleteParams,
    ) -> Result<DeleteResult>;
    /// List visible routines.
    async fn routines_list(&self) -> Result<RoutinesListResult>;
    /// Fetch one routine by ID.
    async fn routines_get(&self, params: RoutinesGetParams) -> Result<RoutineGetResult>;
    /// Create a new routine.
    async fn routines_create(&self, params: RoutineCreateParams) -> Result<RoutineMutationResult>;
    /// Update an existing routine.
    async fn routines_update(&self, params: RoutineUpdateParams) -> Result<RoutineMutationResult>;
    /// Delete a routine.
    async fn routines_delete(&self, params: RoutineDeleteParams) -> Result<DeleteResult>;
    /// List visible models.
    async fn models_list(&self) -> Result<ModelsListResult>;
    /// Fetch one model by ID.
    async fn models_get(&self, params: ModelsGetParams) -> Result<ModelGetResult>;
    /// Create a new model.
    async fn models_create(&self, params: ModelCreateParams) -> Result<ModelMutationResult>;
    /// Update an existing model.
    async fn models_update(&self, params: ModelUpdateParams) -> Result<ModelMutationResult>;
    /// Delete a model.
    async fn models_delete(&self, params: ModelDeleteParams) -> Result<DeleteResult>;
    /// List visible councils.
    async fn councils_list(&self) -> Result<CouncilsListResult>;
    /// Fetch one council by ID.
    async fn councils_get(&self, params: CouncilsGetParams) -> Result<CouncilGetResult>;
    /// Create a new council.
    async fn councils_create(&self, params: CouncilCreateParams) -> Result<CouncilMutationResult>;
    /// Update an existing council.
    async fn councils_update(&self, params: CouncilUpdateParams) -> Result<CouncilMutationResult>;
    /// Add a member to a council.
    async fn councils_add_member(
        &self,
        params: CouncilAddMemberParams,
    ) -> Result<CouncilMutationResult>;
    /// Update one council member.
    async fn councils_update_member(
        &self,
        params: CouncilUpdateMemberParams,
    ) -> Result<CouncilMutationResult>;
    /// Remove a member from a council.
    async fn councils_remove_member(
        &self,
        params: CouncilRemoveMemberParams,
    ) -> Result<CouncilMutationResult>;
    /// Delete a council.
    async fn councils_delete(&self, params: CouncilDeleteParams) -> Result<DeleteResult>;
    /// List visible context blocks.
    async fn context_blocks_list(&self) -> Result<ContextBlocksListResult>;
    /// Fetch one context block by ID.
    async fn context_blocks_get(
        &self,
        params: ContextBlocksGetParams,
    ) -> Result<ContextBlockGetResult>;
    /// Fetch one context block's template content.
    async fn context_blocks_get_content(
        &self,
        params: ContextBlockContentGetParams,
    ) -> Result<ContextBlockContentGetResult>;
    /// Create a new context block.
    async fn context_blocks_create(
        &self,
        params: ContextBlockCreateParams,
    ) -> Result<ContextBlockMutationResult>;
    /// Update a context block metadata record.
    async fn context_blocks_update(
        &self,
        params: ContextBlockUpdateParams,
    ) -> Result<ContextBlockMutationResult>;
    /// Update a context block template.
    async fn context_blocks_update_content(
        &self,
        params: ContextBlockContentUpdateParams,
    ) -> Result<ContextBlockContentMutationResult>;
    /// Delete a context block.
    async fn context_blocks_delete(&self, params: ContextBlockDeleteParams)
    -> Result<DeleteResult>;
}
