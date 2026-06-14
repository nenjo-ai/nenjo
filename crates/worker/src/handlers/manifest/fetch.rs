use anyhow::Result;
use nenjo::Manifest;
use nenjo::Slug;
use nenjo_events::ResourceType;
use nenjo_platform::api_client::ApiClient;
use tracing::debug;

/// Fetch a single resource from the API and upsert it into the manifest.
pub(super) async fn apply_upsert(
    manifest: &mut Manifest,
    client: &ApiClient,
    rt: ResourceType,
    resource: &Slug,
) -> Result<()> {
    macro_rules! upsert {
        ($field:ident, $fetch:ident, $slug:expr) => {{
            match client.$fetch(resource).await? {
                Some(item) => {
                    let item_slug = $slug(&item);
                    if let Some(pos) = manifest.$field.iter().position(|r| $slug(r) == item_slug) {
                        manifest.$field[pos] = item;
                        debug!(%rt, %item_slug, "Updated existing resource");
                    } else {
                        manifest.$field.push(item);
                        debug!(%rt, %item_slug, "Added new resource");
                    }
                }
                None => {
                    manifest.$field.retain(|r| $slug(r) != *resource);
                    debug!(%rt, %resource, "Resource returned 404, removing");
                }
            }
        }};
    }

    match rt {
        ResourceType::Agent => match client.fetch_agent(resource).await? {
            Some(record) => {
                let slug = Slug::derive(&record.slug);
                let mut item = record.to_manifest(nenjo::manifest::PromptConfig::default());
                if let Some(prompt_response) = client.fetch_agent_prompt_config(resource).await? {
                    item = prompt_response.to_manifest();
                } else if let Some(existing) =
                    manifest.agents.iter().find(|agent| agent.slug == slug)
                {
                    item.prompt_config = existing.prompt_config.clone();
                }
                if let Some(pos) = manifest.agents.iter().position(|r| r.slug == item.slug) {
                    let slug = item.slug.clone();
                    manifest.agents[pos] = item;
                    debug!(%rt, %slug, "Updated existing resource");
                } else {
                    let slug = item.slug.clone();
                    manifest.agents.push(item);
                    debug!(%rt, %slug, "Added new resource");
                }
            }
            None => {
                manifest.agents.retain(|r| r.slug != *resource);
                debug!(%rt, %resource, "Resource returned 404, removing");
            }
        },
        ResourceType::Model => {
            upsert!(models, fetch_model, |r: &nenjo::manifest::ModelManifest| {
                nenjo::manifest::model_manifest_slug(&r.model_provider, &r.model)
            })
        }
        ResourceType::Routine => match client.fetch_routine(resource).await? {
            Some(record) => {
                let item = record.to_manifest();
                let item_slug = item.slug.clone();
                if let Some(pos) = manifest.routines.iter().position(|r| r.slug == item_slug) {
                    manifest.routines[pos] = item;
                    debug!(%rt, %item_slug, "Updated existing resource");
                } else {
                    manifest.routines.push(item);
                    debug!(%rt, %item_slug, "Added new resource");
                }
            }
            None => {
                manifest.routines.retain(|r| r.slug != *resource);
                debug!(%rt, %resource, "Resource returned 404, removing");
            }
        },
        ResourceType::Project => match client.fetch_project(resource).await? {
            Some(detail) => {
                let slug = Slug::derive(&detail.project.slug);
                let item = detail.project.to_manifest(detail.settings);
                if let Some(pos) = manifest.projects.iter().position(|r| r.slug == item.slug) {
                    manifest.projects[pos] = item;
                    debug!(%rt, %slug, "Updated existing resource");
                } else {
                    manifest.projects.push(item);
                    debug!(%rt, %slug, "Added new resource");
                }
            }
            None => {
                manifest.projects.retain(|r| r.slug != *resource);
                debug!(%rt, %resource, "Resource returned 404, removing");
            }
        },
        ResourceType::Council => match client.fetch_council(resource).await? {
            Some(record) => {
                let item = record.to_manifest();
                let item_slug = Slug::derive(&record.slug);
                if let Some(pos) = manifest
                    .councils
                    .iter()
                    .position(|r| Slug::derive(&r.name) == item_slug)
                {
                    manifest.councils[pos] = item;
                    debug!(%rt, %item_slug, "Updated existing resource");
                } else {
                    manifest.councils.push(item);
                    debug!(%rt, %item_slug, "Added new resource");
                }
            }
            None => {
                manifest
                    .councils
                    .retain(|r| Slug::derive(&r.name) != *resource);
                debug!(%rt, %resource, "Resource returned 404, removing");
            }
        },
        ResourceType::Ability => upsert!(
            abilities,
            fetch_ability,
            |r: &nenjo::manifest::AbilityManifest| { Slug::derive(&r.name) }
        ),
        ResourceType::ContextBlock => match client.fetch_context_block_summary(resource).await? {
            Some(summary) => {
                let block_slug = Slug::derive(&summary.slug);
                let existing_template = manifest
                    .context_blocks
                    .iter()
                    .find(|block| {
                        nenjo::manifest::context_block_slug(&block.path, &block.name) == block_slug
                    })
                    .map(|block| block.template.clone())
                    .unwrap_or_default();
                let content = client.fetch_context_block_content(resource).await?;
                let template = match content {
                    Some(content) => content.template.unwrap_or(existing_template),
                    None => existing_template,
                };

                let block = summary.to_manifest(template);

                if let Some(pos) = manifest.context_blocks.iter().position(|r| {
                    nenjo::manifest::context_block_slug(&r.path, &r.name) == block_slug
                }) {
                    manifest.context_blocks[pos] = block;
                    debug!(%rt, %block_slug, "Updated existing resource");
                } else {
                    manifest.context_blocks.push(block);
                    debug!(%rt, %block_slug, "Added new resource");
                }
            }
            None => {
                manifest
                    .context_blocks
                    .retain(|r| nenjo::manifest::context_block_slug(&r.path, &r.name) != *resource);
                debug!(%rt, %resource, "Resource returned 404, removing");
            }
        },
        ResourceType::McpServer => upsert!(
            mcp_servers,
            fetch_mcp_server,
            |r: &nenjo::manifest::McpServerManifest| { Slug::derive(&r.name) }
        ),
        ResourceType::Domain => upsert!(
            domains,
            fetch_domain,
            |r: &nenjo::manifest::DomainManifest| {
                nenjo::manifest::domain_slug(&r.path, &r.name)
            }
        ),
        ResourceType::Document => return Ok(()),
        ResourceType::KnowledgePack => return Ok(()),
    }

    Ok(())
}
