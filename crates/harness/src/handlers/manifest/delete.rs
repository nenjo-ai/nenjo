use nenjo::Manifest;
use nenjo_events::ResourceType;
use tracing::info;
use uuid::Uuid;

/// Remove a deleted resource from the in-memory manifest.
pub(super) fn apply_delete(manifest: &mut Manifest, rt: ResourceType, id: Uuid) {
    match rt {
        ResourceType::Agent => manifest.agents.retain(|r| r.id != id),
        ResourceType::Model => manifest.models.retain(|r| r.id != id),
        ResourceType::Routine => manifest.routines.retain(|r| r.id != id),
        ResourceType::Project => manifest.projects.retain(|r| r.id != id),
        ResourceType::Council => manifest.councils.retain(|r| r.id != id),
        ResourceType::Ability => manifest.abilities.retain(|r| r.id != id),
        ResourceType::ContextBlock => manifest.context_blocks.retain(|r| r.id != id),
        ResourceType::McpServer => manifest.mcp_servers.retain(|r| r.id != id),
        ResourceType::Domain => manifest.domains.retain(|r| r.id != id),
        ResourceType::Document => return,
    }

    info!(%rt, %id, "Removed deleted resource from manifest");
}
