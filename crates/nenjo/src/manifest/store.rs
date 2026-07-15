use anyhow::Result;
use async_trait::async_trait;

use super::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    KnowledgePackManifest, Manifest, ManifestResource, ManifestResourceKind, McpServerManifest,
    ModelManifest, ProjectManifest, RoutineManifest,
};
use crate::Slug;

/// Read manifest resources from any backing store.
#[async_trait]
pub trait ManifestReader: Send + Sync {
    /// Load the full manifest snapshot.
    async fn load_manifest(&self) -> Result<Manifest>;

    /// List all cached agents.
    async fn list_agents(&self) -> Result<Vec<AgentManifest>>;
    /// Look up one agent by slug.
    async fn get_agent(&self, slug: &Slug) -> Result<Option<AgentManifest>>;

    /// List all cached models.
    async fn list_models(&self) -> Result<Vec<ModelManifest>>;
    /// Look up one model by slug.
    async fn get_model(&self, slug: &Slug) -> Result<Option<ModelManifest>>;

    /// List all cached routines.
    async fn list_routines(&self) -> Result<Vec<RoutineManifest>>;
    /// Look up one routine by slug.
    async fn get_routine(&self, slug: &Slug) -> Result<Option<RoutineManifest>>;

    /// List all cached projects.
    async fn list_projects(&self) -> Result<Vec<ProjectManifest>>;
    /// Look up one project by slug.
    async fn get_project(&self, slug: &Slug) -> Result<Option<ProjectManifest>>;
    /// Look up one project by slug.
    async fn get_project_by_slug(&self, slug: &str) -> Result<Option<ProjectManifest>> {
        Ok(self
            .list_projects()
            .await?
            .into_iter()
            .find(|item| item.slug.as_str() == slug))
    }

    /// List all cached councils.
    async fn list_councils(&self) -> Result<Vec<CouncilManifest>>;
    /// Look up one council by slug.
    async fn get_council(&self, slug: &Slug) -> Result<Option<CouncilManifest>>;

    /// List all cached domains.
    async fn list_domains(&self) -> Result<Vec<DomainManifest>>;
    /// Look up one domain by slug.
    async fn get_domain(&self, slug: &Slug) -> Result<Option<DomainManifest>>;

    /// List all cached MCP servers.
    async fn list_mcp_servers(&self) -> Result<Vec<McpServerManifest>>;
    /// Look up one MCP server by slug.
    async fn get_mcp_server(&self, slug: &Slug) -> Result<Option<McpServerManifest>>;

    /// List all cached abilities.
    async fn list_abilities(&self) -> Result<Vec<AbilityManifest>>;
    /// Look up one ability by slug.
    async fn get_ability(&self, slug: &Slug) -> Result<Option<AbilityManifest>>;

    /// List all cached context blocks.
    async fn list_context_blocks(&self) -> Result<Vec<ContextBlockManifest>>;
    /// Look up one context block by slug.
    async fn get_context_block(&self, slug: &Slug) -> Result<Option<ContextBlockManifest>>;

    /// List all cached knowledge packs.
    async fn list_knowledge_packs(&self) -> Result<Vec<KnowledgePackManifest>> {
        Ok(self.load_manifest().await?.knowledge_packs)
    }

    /// Look up one knowledge pack by slug.
    async fn get_knowledge_pack(&self, slug: &Slug) -> Result<Option<KnowledgePackManifest>> {
        Ok(self
            .list_knowledge_packs()
            .await?
            .into_iter()
            .find(|item| item.slug == *slug))
    }
}

/// Write manifest resources to any backing store.
#[async_trait]
pub trait ManifestWriter: Send + Sync {
    /// Replace the full manifest snapshot.
    async fn replace_manifest(&self, manifest: &Manifest) -> Result<()>;

    /// Insert or update a single manifest resource and return the canonical stored value.
    async fn upsert_resource(&self, resource: &ManifestResource) -> Result<ManifestResource>;

    /// Cache a resource obtained while serving a read, without treating it as
    /// a platform mutation.
    ///
    /// Most stores persist read-through entries exactly like ordinary upserts.
    /// Hosts with an authoritative remote bootstrap cache can override this to
    /// preserve cache envelopes or to avoid reloading their live provider.
    async fn cache_resource(&self, resource: &ManifestResource) -> Result<ManifestResource> {
        self.upsert_resource(resource).await
    }

    /// Delete one manifest resource by kind and slug.
    async fn delete_resource(&self, kind: ManifestResourceKind, slug: &Slug) -> Result<()>;
}
