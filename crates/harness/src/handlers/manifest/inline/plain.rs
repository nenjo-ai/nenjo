use nenjo::Manifest;
use nenjo::agents::prompts::PromptConfig;
use nenjo_events::ResourceType;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::handlers::manifest::payload::{
    AbilityDocument, AbilityPromptDocument, AgentDocument, AgentPromptDocument,
    ContextBlockContentDocument, ContextBlockDocument, CouncilDocument, DomainDocument,
    DomainPromptDocument, ProjectDocument,
};

/// Apply an inline payload directly to the manifest without an API fetch.
/// Returns `true` if the payload was successfully applied, `false` if
/// deserialization failed (caller should fall back to API fetch).
pub(in crate::handlers::manifest) fn apply_inline_upsert(
    manifest: &mut Manifest,
    rt: ResourceType,
    id: Uuid,
    data: &serde_json::Value,
) -> bool {
    if rt == ResourceType::Agent {
        if data.get("prompt_config").is_some() {
            return match serde_json::from_value::<nenjo::manifest::AgentManifest>(data.clone()) {
                Ok(agent) => {
                    upsert_agent(manifest, id, agent);
                    debug!(%rt, %id, "Applied inline agent payload");
                    true
                }
                Err(_) => match serde_json::from_value::<AgentPromptDocument>(data.clone()) {
                    Ok(agent) => {
                        let agent: nenjo::manifest::AgentManifest =
                            agent_with_prompt_document(agent, None);
                        upsert_agent(manifest, id, agent);
                        debug!(%rt, %id, "Applied inline agent prompt document payload");
                        true
                    }
                    Err(e) => {
                        warn!(%rt, %id, error = %e, "Failed to deserialize inline agent prompt payload, will fetch");
                        false
                    }
                },
            };
        }

        return match serde_json::from_value::<AgentDocument>(data.clone()) {
            Ok(agent) => {
                let existing_prompt = manifest
                    .agents
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.prompt_config.clone());
                let agent = agent_with_prompt_document(
                    AgentPromptDocument {
                        agent,
                        prompt_config: existing_prompt.clone().unwrap_or_default(),
                    },
                    existing_prompt,
                );
                upsert_agent(manifest, id, agent);
                debug!(%rt, %id, "Applied inline agent document payload");
                true
            }
            Err(e) => {
                warn!(%rt, %id, error = %e, "Failed to deserialize inline agent payload, will fetch");
                false
            }
        };
    }

    macro_rules! inline_upsert {
        ($field:ident, $ty:ty) => {{
            match serde_json::from_value::<$ty>(data.clone()) {
                Ok(item) => {
                    if let Some(pos) = manifest.$field.iter().position(|r| r.id == id) {
                        manifest.$field[pos] = item;
                    } else {
                        manifest.$field.push(item);
                    }
                    debug!(%rt, %id, "Applied inline resource payload");
                    true
                }
                Err(e) => {
                    warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                    false
                }
            }
        }};
    }

    match rt {
        ResourceType::Agent => false,
        ResourceType::Model => inline_upsert!(models, nenjo::manifest::ModelManifest),
        ResourceType::Routine => inline_upsert!(routines, nenjo::manifest::RoutineManifest),
        ResourceType::Project => {
            match serde_json::from_value::<nenjo::manifest::ProjectManifest>(data.clone()) {
                Ok(item) => {
                    if let Some(pos) = manifest.projects.iter().position(|r| r.id == id) {
                        manifest.projects[pos] = item;
                    } else {
                        manifest.projects.push(item);
                    }
                    debug!(%rt, %id, "Applied inline project payload");
                    true
                }
                Err(_) => match serde_json::from_value::<ProjectDocument>(data.clone()) {
                    Ok(item) => {
                        let item: nenjo::manifest::ProjectManifest = item.into();
                        if let Some(pos) = manifest.projects.iter().position(|r| r.id == id) {
                            manifest.projects[pos] = item;
                        } else {
                            manifest.projects.push(item);
                        }
                        debug!(%rt, %id, "Applied inline project document payload");
                        true
                    }
                    Err(e) => {
                        warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                        false
                    }
                },
            }
        }
        ResourceType::Council => {
            match serde_json::from_value::<nenjo::manifest::CouncilManifest>(data.clone()) {
                Ok(item) => {
                    if let Some(pos) = manifest.councils.iter().position(|r| r.id == id) {
                        manifest.councils[pos] = item;
                    } else {
                        manifest.councils.push(item);
                    }
                    debug!(%rt, %id, "Applied inline council payload");
                    true
                }
                Err(_) => match serde_json::from_value::<CouncilDocument>(data.clone()) {
                    Ok(item) => {
                        let item: nenjo::manifest::CouncilManifest = item.into();
                        if let Some(pos) = manifest.councils.iter().position(|r| r.id == id) {
                            manifest.councils[pos] = item;
                        } else {
                            manifest.councils.push(item);
                        }
                        debug!(%rt, %id, "Applied inline council document payload");
                        true
                    }
                    Err(e) => {
                        warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                        false
                    }
                },
            }
        }
        ResourceType::Ability => {
            match serde_json::from_value::<nenjo::manifest::AbilityManifest>(data.clone()) {
                Ok(ability) => {
                    if let Some(pos) = manifest.abilities.iter().position(|r| r.id == id) {
                        manifest.abilities[pos] = ability;
                    } else {
                        manifest.abilities.push(ability);
                    }
                    true
                }
                Err(_) => match serde_json::from_value::<AbilityPromptDocument>(data.clone()) {
                    Ok(ability) => {
                        let prompt_config = ability.prompt_config;
                        let mut next_ability: nenjo::manifest::AbilityManifest =
                            ability.ability.into();
                        next_ability.prompt_config = prompt_config;
                        if let Some(pos) = manifest.abilities.iter().position(|r| r.id == id) {
                            manifest.abilities[pos] = next_ability;
                        } else {
                            manifest.abilities.push(next_ability);
                        }
                        true
                    }
                    Err(_) => match serde_json::from_value::<AbilityDocument>(data.clone()) {
                        Ok(ability) => {
                            let prompt_config = manifest
                                .abilities
                                .iter()
                                .find(|r| r.id == id)
                                .map(|r| r.prompt_config.clone())
                                .unwrap_or_default();
                            let mut ability: nenjo::manifest::AbilityManifest = ability.into();
                            ability.prompt_config = prompt_config;
                            if let Some(pos) = manifest.abilities.iter().position(|r| r.id == id) {
                                manifest.abilities[pos] = ability;
                            } else {
                                manifest.abilities.push(ability);
                            }
                            true
                        }
                        Err(e) => {
                            warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                            false
                        }
                    },
                },
            }
        }
        ResourceType::ContextBlock => {
            match serde_json::from_value::<nenjo::manifest::ContextBlockManifest>(data.clone()) {
                Ok(block) => {
                    if let Some(pos) = manifest.context_blocks.iter().position(|r| r.id == id) {
                        manifest.context_blocks[pos] = block;
                    } else {
                        manifest.context_blocks.push(block);
                    }
                    true
                }
                Err(_) => match serde_json::from_value::<ContextBlockContentDocument>(data.clone())
                {
                    Ok(block) => {
                        let block = nenjo::manifest::ContextBlockManifest {
                            id: block.context_block.summary.id,
                            name: block.context_block.summary.name,
                            path: block.context_block.summary.path,
                            display_name: block.context_block.summary.display_name,
                            description: block.context_block.summary.description,
                            template: block.template,
                        };
                        if let Some(pos) = manifest.context_blocks.iter().position(|r| r.id == id) {
                            manifest.context_blocks[pos] = block;
                        } else {
                            manifest.context_blocks.push(block);
                        }
                        true
                    }
                    Err(_) => match serde_json::from_value::<ContextBlockDocument>(data.clone()) {
                        Ok(block) => {
                            let existing_template = manifest
                                .context_blocks
                                .iter()
                                .find(|r| r.id == id)
                                .map(|r| r.template.clone())
                                .unwrap_or_default();
                            let block = nenjo::manifest::ContextBlockManifest {
                                id: block.summary.id,
                                name: block.summary.name,
                                path: block.summary.path,
                                display_name: block.summary.display_name,
                                description: block.summary.description,
                                template: existing_template,
                            };
                            if let Some(pos) =
                                manifest.context_blocks.iter().position(|r| r.id == id)
                            {
                                manifest.context_blocks[pos] = block;
                            } else {
                                manifest.context_blocks.push(block);
                            }
                            true
                        }
                        Err(e) => {
                            warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                            false
                        }
                    },
                },
            }
        }
        ResourceType::McpServer => inline_upsert!(mcp_servers, nenjo::manifest::McpServerManifest),
        ResourceType::Domain => {
            match serde_json::from_value::<nenjo::manifest::DomainManifest>(data.clone()) {
                Ok(domain) => {
                    if let Some(pos) = manifest.domains.iter().position(|r| r.id == id) {
                        manifest.domains[pos] = domain;
                    } else {
                        manifest.domains.push(domain);
                    }
                    true
                }
                Err(_) => match serde_json::from_value::<DomainPromptDocument>(data.clone()) {
                    Ok(domain) => {
                        let existing_manifest =
                            manifest.domains.iter().find(|r| r.id == id).cloned();
                        let domain = nenjo::manifest::DomainManifest {
                            id: domain.domain.summary.id,
                            name: domain.domain.summary.name,
                            path: domain.domain.summary.path,
                            display_name: domain.domain.summary.display_name,
                            description: domain.domain.summary.description,
                            command: domain.domain.command,
                            platform_scopes: existing_manifest
                                .as_ref()
                                .map(|domain| domain.platform_scopes.clone())
                                .unwrap_or_else(|| domain.domain.platform_scopes.clone()),
                            ability_ids: existing_manifest
                                .as_ref()
                                .map(|domain| domain.ability_ids.clone())
                                .unwrap_or_else(|| domain.domain.ability_ids.clone()),
                            mcp_server_ids: existing_manifest
                                .as_ref()
                                .map(|domain| domain.mcp_server_ids.clone())
                                .unwrap_or_else(|| domain.domain.mcp_server_ids.clone()),
                            prompt_config: domain.prompt_config,
                        };
                        if let Some(pos) = manifest.domains.iter().position(|r| r.id == id) {
                            manifest.domains[pos] = domain;
                        } else {
                            manifest.domains.push(domain);
                        }
                        true
                    }
                    Err(_) => match serde_json::from_value::<DomainDocument>(data.clone()) {
                        Ok(domain) => {
                            let existing_manifest =
                                manifest.domains.iter().find(|r| r.id == id).cloned();
                            let domain = nenjo::manifest::DomainManifest {
                                id: domain.summary.id,
                                name: domain.summary.name,
                                path: domain.summary.path,
                                display_name: domain.summary.display_name,
                                description: domain.summary.description,
                                command: domain.command,
                                platform_scopes: domain.platform_scopes,
                                ability_ids: domain.ability_ids,
                                mcp_server_ids: domain.mcp_server_ids,
                                prompt_config: existing_manifest
                                    .as_ref()
                                    .map(|domain| domain.prompt_config.clone())
                                    .unwrap_or_default(),
                            };
                            if let Some(pos) = manifest.domains.iter().position(|r| r.id == id) {
                                manifest.domains[pos] = domain;
                            } else {
                                manifest.domains.push(domain);
                            }
                            true
                        }
                        Err(e) => {
                            warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                            false
                        }
                    },
                },
            }
        }
        ResourceType::Document => false,
    }
}

fn agent_with_prompt_document(
    agent: AgentPromptDocument,
    fallback_prompt: Option<PromptConfig>,
) -> nenjo::manifest::AgentManifest {
    let mut agent_manifest: nenjo::manifest::AgentManifest = agent.agent.into();
    agent_manifest.prompt_config = fallback_prompt.unwrap_or(agent.prompt_config);
    agent_manifest
}

fn upsert_agent(manifest: &mut Manifest, id: Uuid, agent: nenjo::manifest::AgentManifest) {
    if let Some(pos) = manifest.agents.iter().position(|r| r.id == id) {
        manifest.agents[pos] = agent;
    } else {
        manifest.agents.push(agent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_manifest(id: Uuid, developer_prompt: &str) -> nenjo::manifest::AgentManifest {
        nenjo::manifest::AgentManifest {
            id,
            name: "agent".into(),
            description: None,
            prompt_config: PromptConfig {
                developer_prompt: developer_prompt.into(),
                ..Default::default()
            },
            color: None,
            model_id: None,
            domain_ids: Vec::new(),
            platform_scopes: Vec::new(),
            mcp_server_ids: Vec::new(),
            ability_ids: Vec::new(),
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
        let payload = serde_json::to_value(agent_manifest(id, "new")).unwrap();

        assert!(apply_inline_upsert(
            &mut manifest,
            ResourceType::Agent,
            id,
            &payload
        ));

        assert_eq!(manifest.agents[0].prompt_config.developer_prompt, "new");
    }

    #[test]
    fn inline_agent_document_updates_uncached_agent() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let payload = serde_json::json!({
            "id": id,
            "name": "agent",
            "description": null,
            "color": null,
            "model_id": null,
            "domains": [],
            "platform_scopes": [],
            "mcp_server_ids": [],
            "abilities": [],
            "prompt_locked": false,
            "heartbeat": null
        });

        assert!(apply_inline_upsert(
            &mut manifest,
            ResourceType::Agent,
            id,
            &payload
        ));

        assert_eq!(manifest.agents.len(), 1);
        assert_eq!(manifest.agents[0].id, id);
        assert_eq!(manifest.agents[0].prompt_config.developer_prompt, "");
    }

    #[test]
    fn inline_agent_document_preserves_cached_prompt_config() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest {
            agents: vec![agent_manifest(id, "cached")],
            ..Default::default()
        };
        let payload = serde_json::json!({
            "id": id,
            "name": "renamed",
            "description": null,
            "color": null,
            "model_id": null,
            "domains": [],
            "platform_scopes": [],
            "mcp_server_ids": [],
            "abilities": [],
            "prompt_locked": false,
            "heartbeat": null
        });

        assert!(apply_inline_upsert(
            &mut manifest,
            ResourceType::Agent,
            id,
            &payload
        ));

        assert_eq!(manifest.agents[0].name, "renamed");
        assert_eq!(manifest.agents[0].prompt_config.developer_prompt, "cached");
    }
}
