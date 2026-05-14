use anyhow::Result;
use nenjo::Manifest;
use nenjo::client::NenjoClient;
use nenjo_events::ResourceType;
use tracing::debug;
use uuid::Uuid;

/// Fetch a single resource from the API and upsert it into the manifest.
pub(super) async fn apply_upsert(
    manifest: &mut Manifest,
    client: &NenjoClient,
    rt: ResourceType,
    id: Uuid,
) -> Result<()> {
    macro_rules! upsert {
        ($field:ident, $fetch:ident) => {{
            match client.$fetch(id).await? {
                Some(item) => {
                    if let Some(pos) = manifest.$field.iter().position(|r| r.id == id) {
                        manifest.$field[pos] = item;
                        debug!(%rt, %id, "Updated existing resource");
                    } else {
                        manifest.$field.push(item);
                        debug!(%rt, %id, "Added new resource");
                    }
                }
                None => {
                    manifest.$field.retain(|r| r.id != id);
                    debug!(%rt, %id, "Resource returned 404, removing");
                }
            }
        }};
    }

    match rt {
        ResourceType::Agent => match client.fetch_agent(id).await? {
            Some(mut item) => {
                if let Some(prompt_response) = client.fetch_agent_prompt_config(id).await? {
                    if let Some(prompt_config) = prompt_response.prompt_config {
                        item.prompt_config = prompt_config;
                    } else if let Some(existing) =
                        manifest.agents.iter().find(|agent| agent.id == id)
                    {
                        item.prompt_config = existing.prompt_config.clone();
                    }
                } else if let Some(existing) = manifest.agents.iter().find(|agent| agent.id == id) {
                    item.prompt_config = existing.prompt_config.clone();
                }
                if let Some(pos) = manifest.agents.iter().position(|r| r.id == id) {
                    manifest.agents[pos] = item;
                    debug!(%rt, %id, "Updated existing resource");
                } else {
                    manifest.agents.push(item);
                    debug!(%rt, %id, "Added new resource");
                }
            }
            None => {
                manifest.agents.retain(|r| r.id != id);
                debug!(%rt, %id, "Resource returned 404, removing");
            }
        },
        ResourceType::Model => upsert!(models, fetch_model),
        ResourceType::Routine => upsert!(routines, fetch_routine),
        ResourceType::Project => upsert!(projects, fetch_project),
        ResourceType::Council => upsert!(councils, fetch_council),
        ResourceType::Ability => upsert!(abilities, fetch_ability),
        ResourceType::ContextBlock => match client.fetch_context_block_summary(id).await? {
            Some(summary) => {
                let existing_template = manifest
                    .context_blocks
                    .iter()
                    .find(|block| block.id == id)
                    .map(|block| block.template.clone())
                    .unwrap_or_default();
                let content = client.fetch_context_block_content(id).await?;
                let template = match content {
                    Some(content) => content.template.unwrap_or(existing_template),
                    None => existing_template,
                };

                let block = nenjo::manifest::ContextBlockManifest {
                    id: summary.id,
                    name: summary.name,
                    path: summary.path,
                    display_name: summary.display_name,
                    description: summary.description,
                    template,
                };

                if let Some(pos) = manifest.context_blocks.iter().position(|r| r.id == id) {
                    manifest.context_blocks[pos] = block;
                    debug!(%rt, %id, "Updated existing resource");
                } else {
                    manifest.context_blocks.push(block);
                    debug!(%rt, %id, "Added new resource");
                }
            }
            None => {
                manifest.context_blocks.retain(|r| r.id != id);
                debug!(%rt, %id, "Resource returned 404, removing");
            }
        },
        ResourceType::McpServer => upsert!(mcp_servers, fetch_mcp_server),
        ResourceType::Domain => upsert!(domains, fetch_domain),
        ResourceType::Document => return Ok(()),
    }

    Ok(())
}
