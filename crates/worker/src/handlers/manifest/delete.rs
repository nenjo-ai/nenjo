use nenjo::manifest::ManifestIdentity;
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
        ResourceType::Model => manifest.models.retain(|r| r.slug != *resource),
        ResourceType::Routine => manifest.routines.retain(|r| r.slug != *resource),
        ResourceType::Project => manifest.projects.retain(|r| r.slug != *resource),
        ResourceType::Council => manifest.councils.retain(|r| r.slug != *resource),
        ResourceType::Ability => manifest.abilities.retain(|r| r.slug != *resource),
        ResourceType::Command => manifest.commands.retain(|r| r.manifest_slug() != resource),
        ResourceType::ContextBlock => manifest.context_blocks.retain(|r| r.slug != *resource),
        ResourceType::McpServer => manifest.mcp_servers.retain(|r| r.slug != *resource),
        ResourceType::Domain => manifest.domains.retain(|r| r.slug != *resource),
        ResourceType::ModelAssignment
        | ResourceType::ModelCapabilityDefault
        | ResourceType::Document => {
            return;
        }
        ResourceType::KnowledgePack => manifest.knowledge_packs.retain(|r| r.slug != *resource),
    }

    info!(%rt, %resource, "Removed deleted resource from manifest");
    debug!(%rt, %resource, resource_id = ?resource_id, "Deleted manifest resource details");
}
