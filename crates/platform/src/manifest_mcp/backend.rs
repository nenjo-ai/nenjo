use anyhow::Result;
use async_trait::async_trait;

use super::params::{
    AbilitiesGetParams, AbilityConfigureParams, AgentConfigureParams, AgentsGetParams,
    ContextBlockConfigureParams, ContextBlocksGetParams, CouncilAddMemberParams,
    CouncilCreateParams, CouncilDeleteParams, CouncilRemoveMemberParams, CouncilUpdateMemberParams,
    CouncilUpdateParams, CouncilsGetParams, DomainConfigureParams, DomainsGetParams,
    KnowledgeDocCreateParams, KnowledgeDocDeleteParams, KnowledgeDocUpdateParams,
    KnowledgePackCreateParams, KnowledgePackUpdateParams, ModelCreateParams, ModelDeleteParams,
    ModelUpdateParams, ModelsGetParams, ProjectCreateParams, ProjectDeleteParams,
    ProjectUpdateParams, ProjectsGetParams, RoutineConfigureParams, RoutineDeleteParams,
    RoutinesGetParams,
};
use super::results::{
    AbilitiesListResult, AbilityConfigureResult, AbilityGetResult, AgentConfigureResult,
    AgentGetResult, AgentsListResult, ContextBlockConfigureResult, ContextBlockGetResult,
    ContextBlocksListResult, CouncilGetResult, CouncilMutationResult, CouncilsListResult,
    DeleteResult, DomainConfigureResult, DomainGetResult, DomainsListResult,
    KnowledgeDocMutationResult, KnowledgePackMutationResult, ModelGetResult, ModelMutationResult,
    ModelsListResult, ProjectGetResult, ProjectMutationResult, ProjectsListResult,
    RoutineConfigureResult, RoutineGetResult, RoutinesListResult,
};

#[async_trait]
/// Backend operations for agent manifest resources.
pub trait AgentManifestBackend: Send + Sync {
    /// List visible agents.
    async fn list_agents(&self) -> Result<AgentsListResult>;
    /// Fetch one agent by slug, including prompt configuration.
    async fn get_agent(&self, params: AgentsGetParams) -> Result<AgentGetResult>;
    /// Create or update an agent in one backend-owned sequence.
    async fn configure_agent(&self, params: AgentConfigureParams) -> Result<AgentConfigureResult>;
}

#[async_trait]
/// Backend operations for ability manifest resources.
pub trait AbilityManifestBackend: Send + Sync {
    /// List visible abilities.
    async fn list_abilities(&self) -> Result<AbilitiesListResult>;
    /// Fetch one ability by ID.
    async fn get_ability(&self, params: AbilitiesGetParams) -> Result<AbilityGetResult>;
    /// Create or update an ability in one backend-owned sequence.
    async fn configure_ability(
        &self,
        params: AbilityConfigureParams,
    ) -> Result<AbilityConfigureResult>;
}

#[async_trait]
/// Backend operations for domain manifest resources.
pub trait DomainManifestBackend: Send + Sync {
    /// List visible domains.
    async fn list_domains(&self) -> Result<DomainsListResult>;
    /// Fetch one domain by ID.
    async fn get_domain(&self, params: DomainsGetParams) -> Result<DomainGetResult>;
    /// Create or update a domain in one backend-owned sequence.
    async fn configure_domain(
        &self,
        params: DomainConfigureParams,
    ) -> Result<DomainConfigureResult>;
}

#[async_trait]
/// Backend operations for project manifest resources.
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
}

#[async_trait]
/// Backend operations for org-level library knowledge document mutations.
pub trait LibraryManifestBackend: Send + Sync {
    /// Create a user-managed Library knowledge pack.
    async fn create_knowledge_pack(
        &self,
        params: KnowledgePackCreateParams,
    ) -> Result<KnowledgePackMutationResult>;
    /// Update a user-managed Library knowledge pack.
    async fn update_knowledge_pack(
        &self,
        params: KnowledgePackUpdateParams,
    ) -> Result<KnowledgePackMutationResult>;
    /// Create a library knowledge document.
    async fn create_knowledge_doc(
        &self,
        params: KnowledgeDocCreateParams,
    ) -> Result<KnowledgeDocMutationResult>;
    /// Update a library knowledge document's content, metadata, and edges.
    async fn update_knowledge_doc(
        &self,
        params: KnowledgeDocUpdateParams,
    ) -> Result<KnowledgeDocMutationResult>;
    /// Delete a library knowledge document.
    async fn delete_knowledge_doc(&self, params: KnowledgeDocDeleteParams) -> Result<DeleteResult>;
}

#[async_trait]
/// Backend operations for routine manifest resources.
pub trait RoutineManifestBackend: Send + Sync {
    /// List visible routines.
    async fn list_routines(&self) -> Result<RoutinesListResult>;
    /// Fetch one routine by ID.
    async fn get_routine(&self, params: RoutinesGetParams) -> Result<RoutineGetResult>;
    /// Create or update a routine in one backend-owned sequence.
    async fn configure_routine(
        &self,
        params: RoutineConfigureParams,
    ) -> Result<RoutineConfigureResult>;
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
    /// Create or update a context block in one backend-owned sequence.
    async fn configure_context_block(
        &self,
        params: ContextBlockConfigureParams,
    ) -> Result<ContextBlockConfigureResult>;
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
    + ProjectManifestBackend
    + LibraryManifestBackend
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
        + ProjectManifestBackend
        + LibraryManifestBackend
        + RoutineManifestBackend
        + ModelManifestBackend
        + CouncilManifestBackend
        + ContextBlockManifestBackend
        + Send
        + Sync
{
}
