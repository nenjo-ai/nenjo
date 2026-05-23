use nenjo::manifest::{context_block_slug, domain_slug};
use nenjo::{Manifest, Slug};
use nenjo_events::ResourceType;
use tracing::info;
use uuid::Uuid;

/// Remove a deleted resource from the in-memory manifest.
pub(super) fn apply_delete(
    manifest: &mut Manifest,
    rt: ResourceType,
    resource: &Slug,
    resource_id: Option<Uuid>,
) {
    match rt {
        ResourceType::Agent => manifest
            .agents
            .retain(|r| Some(r.id) != resource_id && Slug::derive(&r.name) != *resource),
        ResourceType::Model => manifest
            .models
            .retain(|r| Some(r.id) != resource_id && Slug::derive(&r.name) != *resource),
        ResourceType::Routine => manifest
            .routines
            .retain(|r| Some(r.id) != resource_id && Slug::derive(&r.name) != *resource),
        ResourceType::Project => manifest
            .projects
            .retain(|r| Some(r.id) != resource_id && r.slug != *resource),
        ResourceType::Council => manifest
            .councils
            .retain(|r| Some(r.id) != resource_id && Slug::derive(&r.name) != *resource),
        ResourceType::Ability => manifest
            .abilities
            .retain(|r| Some(r.id) != resource_id && Slug::derive(&r.name) != *resource),
        ResourceType::ContextBlock => manifest.context_blocks.retain(|r| {
            Some(r.id) != resource_id && context_block_slug(&r.path, &r.name) != *resource
        }),
        ResourceType::McpServer => manifest
            .mcp_servers
            .retain(|r| Some(r.id) != resource_id && Slug::derive(&r.name) != *resource),
        ResourceType::Domain => manifest
            .domains
            .retain(|r| Some(r.id) != resource_id && domain_slug(&r.path, &r.name) != *resource),
        ResourceType::Document => return,
        ResourceType::KnowledgePack => return,
    }

    info!(%rt, %resource, resource_id = ?resource_id, "Removed deleted resource from manifest");
}
