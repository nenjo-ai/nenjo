use nenjo::manifest::{context_block_slug, domain_slug};
use nenjo::{Manifest, Slug};
use nenjo_events::ResourceType;
use tracing::{debug, info};
use uuid::Uuid;

/// Remove a deleted resource from the in-memory manifest.
pub(super) fn apply_delete(
    manifest: &mut Manifest,
    rt: ResourceType,
    resource: &Slug,
    resource_id: Option<Uuid>,
) {
    match rt {
        ResourceType::Agent => manifest.agents.retain(|r| r.slug != *resource),
        ResourceType::Model => manifest
            .models
            .retain(|r| Slug::derive(&r.name) != *resource),
        ResourceType::Routine => manifest.routines.retain(|r| r.slug != *resource),
        ResourceType::Project => manifest.projects.retain(|r| r.slug != *resource),
        ResourceType::Council => manifest
            .councils
            .retain(|r| Slug::derive(&r.name) != *resource),
        ResourceType::Ability => manifest
            .abilities
            .retain(|r| Slug::derive(&r.name) != *resource),
        ResourceType::ContextBlock => manifest
            .context_blocks
            .retain(|r| context_block_slug(&r.path, &r.name) != *resource),
        ResourceType::McpServer => manifest
            .mcp_servers
            .retain(|r| Slug::derive(&r.name) != *resource),
        ResourceType::Domain => manifest
            .domains
            .retain(|r| domain_slug(&r.path, &r.name) != *resource),
        ResourceType::Document => return,
        ResourceType::KnowledgePack => manifest.knowledge_packs.retain(|r| r.slug != *resource),
    }

    info!(%rt, %resource, "Removed deleted resource from manifest");
    debug!(%rt, %resource, resource_id = ?resource_id, "Deleted manifest resource details");
}
