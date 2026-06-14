use nenjo::Manifest;
use nenjo::Slug;
use nenjo::manifest::{HasManifestSlug, context_block_slug};
use nenjo_events::ResourceType;
use nenjo_platform::manifest_contract::{
    AbilityPromptRecord, AgentRecord, ContextBlockContentRecord, CouncilRecord, DomainPromptRecord,
    ModelRecord, ProjectRecord, RoutineRecord,
};
use tracing::{debug, warn};

use crate::handlers::manifest::payload::{envelope_data_field, parse_inline_record};

/// Apply an inline payload directly to the manifest without an API fetch.
/// Returns `true` if the payload was successfully applied, `false` if
/// deserialization failed (caller should fall back to API fetch).
pub(in crate::handlers::manifest) fn apply_inline_upsert(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    match rt {
        ResourceType::Agent => apply_agent_inline(manifest, rt, data),
        ResourceType::Model => apply_model_inline(manifest, rt, data),
        ResourceType::Routine => apply_routine_inline(manifest, rt, data),
        ResourceType::Project => apply_project_inline(manifest, rt, data),
        ResourceType::Council => apply_council_inline(manifest, rt, data),
        ResourceType::Ability => apply_ability_inline(manifest, rt, data),
        ResourceType::ContextBlock => apply_context_block_inline(manifest, rt, data),
        ResourceType::McpServer => apply_mcp_server_inline(manifest, rt, data),
        ResourceType::Domain => apply_domain_inline(manifest, rt, data),
        ResourceType::Document | ResourceType::KnowledgePack => false,
    }
}

fn apply_agent_inline(manifest: &mut Manifest, rt: ResourceType, data: &serde_json::Value) -> bool {
    let Some(record) = parse_inline_record::<AgentRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    let slug = Slug::derive(&record.slug);
    let item = if record.prompt_config.is_some() {
        record.to_manifest(record.resolved_prompt_config())
    } else {
        let existing_prompt = manifest
            .agents
            .iter()
            .find(|agent| agent.slug == slug)
            .map(|agent| agent.prompt_config.clone())
            .unwrap_or_default();
        record.to_manifest(existing_prompt)
    };

    upsert_by_slug(&mut manifest.agents, item);
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_model_inline(manifest: &mut Manifest, rt: ResourceType, data: &serde_json::Value) -> bool {
    let Some(record) = parse_inline_record::<ModelRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    upsert_by_slug(&mut manifest.models, record.to_manifest());
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_routine_inline(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    let Some(record) = parse_inline_record::<RoutineRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    upsert_by_slug(&mut manifest.routines, record.to_manifest());
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_project_inline(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    let Some(record) = parse_inline_record::<ProjectRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    let slug = Slug::derive(&record.slug);
    let settings = envelope_data_field(data, "settings")
        .cloned()
        .unwrap_or_else(|| {
            manifest
                .projects
                .iter()
                .find(|project| project.slug == slug)
                .map(|project| project.settings.clone())
                .unwrap_or_else(|| serde_json::json!({}))
        });

    upsert_by_slug(&mut manifest.projects, record.to_manifest(settings));
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_council_inline(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    let Some(record) = parse_inline_record::<CouncilRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    upsert_by_slug(&mut manifest.councils, record.to_manifest());
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_ability_inline(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    let Some(record) = parse_inline_record::<AbilityPromptRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    let slug = Slug::derive(&record.ability.slug);
    let item = if record.prompt_config.is_some() {
        record.to_manifest()
    } else {
        let existing_prompt = manifest
            .abilities
            .iter()
            .find(|ability| ability.manifest_slug() == slug)
            .map(|ability| ability.prompt_config.clone())
            .unwrap_or_default();
        record.ability.to_manifest(existing_prompt)
    };

    upsert_by_slug(&mut manifest.abilities, item);
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_context_block_inline(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    let Some(record) = parse_inline_record::<ContextBlockContentRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    let slug = context_block_slug(&record.block.path, &record.block.name);
    let template = record.template.clone().unwrap_or_else(|| {
        manifest
            .context_blocks
            .iter()
            .find(|block| block.manifest_slug() == slug)
            .map(|block| block.template.clone())
            .unwrap_or_default()
    });

    upsert_by_slug(
        &mut manifest.context_blocks,
        record.block.to_manifest(template),
    );
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_mcp_server_inline(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    let Some(item) = parse_inline_record::<nenjo::manifest::McpServerManifest>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    upsert_by_slug(&mut manifest.mcp_servers, item);
    debug!(%rt, "Applied inline resource payload");
    true
}

fn apply_domain_inline(
    manifest: &mut Manifest,
    rt: ResourceType,
    data: &serde_json::Value,
) -> bool {
    let Some(record) = parse_inline_record::<DomainPromptRecord>(data) else {
        warn!(%rt, "Failed to deserialize inline payload, will fetch");
        return false;
    };

    let slug = nenjo::manifest::domain_slug(&record.domain.path, &record.domain.name);
    let item = if record.prompt_config.is_some() {
        record.to_manifest()
    } else {
        let existing_prompt = manifest
            .domains
            .iter()
            .find(|domain| domain.manifest_slug() == slug)
            .map(|domain| domain.prompt_config.clone())
            .unwrap_or_default();
        record.domain.to_manifest(existing_prompt)
    };

    upsert_by_slug(&mut manifest.domains, item);
    debug!(%rt, "Applied inline resource payload");
    true
}

pub(super) fn upsert_by_slug<T>(items: &mut Vec<T>, item: T)
where
    T: HasManifestSlug,
{
    let slug = item.manifest_slug();
    if let Some(pos) = items
        .iter()
        .position(|existing| existing.manifest_slug() == slug)
    {
        items[pos] = item;
    } else {
        items.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::agents::prompts::PromptConfig;
    use uuid::Uuid;

    const TS: &str = "2026-05-10T00:00:00Z";

    fn agent_record_payload(id: Uuid, developer_prompt: &str) -> serde_json::Value {
        let mut payload = agent_metadata_payload(id, "agent");
        payload["data"]["prompt_config"] = serde_json::to_value(PromptConfig {
            developer_prompt: developer_prompt.into(),
            ..Default::default()
        })
        .expect("prompt config should serialize");
        payload
    }

    fn agent_metadata_payload(id: Uuid, name: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": "manifest.resource.v1",
            "data": {
                "id": id,
                "org_id": Uuid::new_v4(),
                "slug": "test-agent",
                "name": name,
                "description": null,
                "color": null,
                "model": null,
                "domains": [],
                "platform_scopes": [],
                "mcp_servers": [],
                "script_tools": [],
                "abilities": [],
                "prompt_locked": false,
                "heartbeat": null,
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_at": TS,
                "updated_at": TS
            }
        })
    }

    fn agent_manifest(_id: Uuid, developer_prompt: &str) -> nenjo::manifest::AgentManifest {
        nenjo::manifest::AgentManifest {
            name: "agent".into(),
            slug: Slug::derive("test-agent"),
            description: None,
            prompt_config: PromptConfig {
                developer_prompt: developer_prompt.into(),
                ..Default::default()
            },
            color: None,
            model: None,
            domains: Vec::new(),
            platform_scopes: Vec::new(),
            mcp_servers: Vec::new(),
            script_tools: Vec::new(),
            abilities: Vec::new(),
            prompt_locked: false,
            heartbeat: None,
        }
    }

    #[test]
    fn inline_agent_manifest_applies_prompt_config() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest {
            agents: vec![agent_manifest(id, "old")],
            ..Default::default()
        };
        let payload = agent_record_payload(id, "new");

        assert!(apply_inline_upsert(
            &mut manifest,
            ResourceType::Agent,
            &payload
        ));

        assert_eq!(manifest.agents[0].prompt_config.developer_prompt, "new");
    }

    #[test]
    fn inline_agent_document_updates_uncached_agent() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let payload = agent_metadata_payload(id, "agent");

        assert!(apply_inline_upsert(
            &mut manifest,
            ResourceType::Agent,
            &payload
        ));

        assert_eq!(manifest.agents.len(), 1);
        assert_eq!(manifest.agents[0].slug, Slug::derive("test-agent"));
        assert_eq!(manifest.agents[0].prompt_config.developer_prompt, "");
    }

    #[test]
    fn inline_agent_document_preserves_cached_prompt_config() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest {
            agents: vec![agent_manifest(id, "cached")],
            ..Default::default()
        };
        let payload = agent_metadata_payload(id, "renamed");

        assert!(apply_inline_upsert(
            &mut manifest,
            ResourceType::Agent,
            &payload
        ));

        assert_eq!(manifest.agents[0].name, "renamed");
        assert_eq!(manifest.agents[0].prompt_config.developer_prompt, "cached");
    }
}
