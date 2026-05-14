use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use super::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    Manifest, ManifestResource, ManifestResourceKind, McpServerManifest, ModelManifest,
    ProjectManifest, RoutineManifest,
};

/// Read manifest resources from any backing store.
#[async_trait]
pub trait ManifestReader: Send + Sync {
    /// Load the full manifest snapshot.
    async fn load_manifest(&self) -> Result<Manifest>;

    /// List all cached agents.
    async fn list_agents(&self) -> Result<Vec<AgentManifest>>;
    /// Look up one agent by ID.
    async fn get_agent(&self, id: Uuid) -> Result<Option<AgentManifest>>;

    /// List all cached models.
    async fn list_models(&self) -> Result<Vec<ModelManifest>>;
    /// Look up one model by ID.
    async fn get_model(&self, id: Uuid) -> Result<Option<ModelManifest>>;

    /// List all cached routines.
    async fn list_routines(&self) -> Result<Vec<RoutineManifest>>;
    /// Look up one routine by ID.
    async fn get_routine(&self, id: Uuid) -> Result<Option<RoutineManifest>>;

    /// List all cached projects.
    async fn list_projects(&self) -> Result<Vec<ProjectManifest>>;
    /// Look up one project by ID.
    async fn get_project(&self, id: Uuid) -> Result<Option<ProjectManifest>>;
    /// Look up one project by slug.
    async fn get_project_by_slug(&self, slug: &str) -> Result<Option<ProjectManifest>> {
        Ok(self
            .list_projects()
            .await?
            .into_iter()
            .find(|item| item.slug == slug))
    }

    /// List all cached councils.
    async fn list_councils(&self) -> Result<Vec<CouncilManifest>>;
    /// Look up one council by ID.
    async fn get_council(&self, id: Uuid) -> Result<Option<CouncilManifest>>;

    /// List all cached domains.
    async fn list_domains(&self) -> Result<Vec<DomainManifest>>;
    /// Look up one domain by ID.
    async fn get_domain(&self, id: Uuid) -> Result<Option<DomainManifest>>;

    /// List all cached MCP servers.
    async fn list_mcp_servers(&self) -> Result<Vec<McpServerManifest>>;
    /// Look up one MCP server by ID.
    async fn get_mcp_server(&self, id: Uuid) -> Result<Option<McpServerManifest>>;

    /// List all cached abilities.
    async fn list_abilities(&self) -> Result<Vec<AbilityManifest>>;
    /// Look up one ability by ID.
    async fn get_ability(&self, id: Uuid) -> Result<Option<AbilityManifest>>;

    /// List all cached context blocks.
    async fn list_context_blocks(&self) -> Result<Vec<ContextBlockManifest>>;
    /// Look up one context block by ID.
    async fn get_context_block(&self, id: Uuid) -> Result<Option<ContextBlockManifest>>;
}

/// Write manifest resources to any backing store.
#[async_trait]
pub trait ManifestWriter: Send + Sync {
    /// Replace the full manifest snapshot.
    async fn replace_manifest(&self, manifest: &Manifest) -> Result<()>;

    /// Insert or update a single manifest resource and return the canonical stored value.
    async fn upsert_resource(&self, resource: &ManifestResource) -> Result<ManifestResource>;

    /// Delete one manifest resource by kind and ID.
    async fn delete_resource(&self, kind: ManifestResourceKind, id: Uuid) -> Result<()>;
}
