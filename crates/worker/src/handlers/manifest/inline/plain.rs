use nenjo::Manifest;
use nenjo::agents::prompts::PromptConfig;
use nenjo_events::ResourceType;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::handlers::manifest::payload::{
    AbilityDocument, AbilityPromptDocument, AgentDocument, AgentPromptDocument,
    ContextBlockContentDocument, ContextBlockDocument, CouncilDocument, DomainDocument,
    DomainPromptDocument, ManifestResourcePayload, ProjectDocument,
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
    let owned_data;
    let mut canonical = false;
    let data = match serde_json::from_value::<ManifestResourcePayload>(data.clone()) {
        Ok(envelope) if envelope.schema == "manifest.resource.v1" => {
            owned_data = envelope.data;
            canonical = true;
            &owned_data
        }
        Ok(envelope) => {
            warn!(
                %rt,
                %id,
                schema = %envelope.schema,
                "Unsupported inline manifest payload schema, will fetch"
            );
            return false;
        }
        Err(_) => data,
    };

    if canonical {
        macro_rules! canonical_upsert {
            ($field:ident, $ty:ty) => {{
                match serde_json::from_value::<$ty>(data.clone()) {
                    Ok(item) => {
                        let id = item.id;
                        if let Some(pos) = manifest.$field.iter().position(|r| r.id == id) {
                            manifest.$field[pos] = item;
                        } else {
                            manifest.$field.push(item);
                        }
                        debug!(%rt, %id, "Applied canonical inline resource payload");
                        true
                    }
                    Err(error) => {
                        warn!(%rt, error = %error, "Failed to deserialize canonical inline payload, will fetch");
                        false
                    }
                }
            }};
        }

        return match rt {
            ResourceType::Agent => canonical_upsert!(agents, nenjo::manifest::AgentManifest),
            ResourceType::Model => canonical_upsert!(models, nenjo::manifest::ModelManifest),
            ResourceType::Routine => canonical_upsert!(routines, nenjo::manifest::RoutineManifest),
            ResourceType::Project => canonical_upsert!(projects, nenjo::manifest::ProjectManifest),
            ResourceType::Council => canonical_upsert!(councils, nenjo::manifest::CouncilManifest),
            ResourceType::Ability => {
                canonical_upsert!(abilities, nenjo::manifest::AbilityManifest)
            }
            ResourceType::ContextBlock => {
                canonical_upsert!(context_blocks, nenjo::manifest::ContextBlockManifest)
            }
            ResourceType::McpServer => {
                canonical_upsert!(mcp_servers, nenjo::manifest::McpServerManifest)
            }
            ResourceType::Domain => canonical_upsert!(domains, nenjo::manifest::DomainManifest),
            ResourceType::Document | ResourceType::KnowledgePack => false,
        };
    }

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
                            agent_with_prompt_document(id, agent, None);
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
                    id,
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
                Ok(mut item) => {
                    item.id = id;
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
                Ok(mut item) => {
                    item.id = id;
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
                        let item = project_from_document(id, item);
                        if let Some(pos) = manifest.projects.iter().position(|r| r.id == id) {
                            manifest.projects[pos] = item;
                        } else {
                            manifest.projects.push(item);
                        }
                        debug!(%rt, %id, "Applied inline project resource payload");
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
                Ok(mut item) => {
                    item.id = id;
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
                        let item = council_from_document(id, item);
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
                Ok(mut ability) => {
                    ability.id = id;
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
                            ability_from_document(id, ability.ability, prompt_config.clone());
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
                            let ability = ability_from_document(id, ability, prompt_config);
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
                Ok(mut block) => {
                    block.id = id;
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
                            id,
                            name: block.context_block.summary.name,
                            path: block.context_block.summary.path,
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
                                id,
                                name: block.summary.name,
                                path: block.summary.path,
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
                Ok(mut domain) => {
                    domain.id = id;
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
                            id,
                            name: domain.domain.summary.name,
                            path: domain.domain.summary.path,
                            description: domain.domain.summary.description,
                            command: domain.domain.command,
                            platform_scopes: existing_manifest
                                .as_ref()
                                .map(|domain| domain.platform_scopes.clone())
                                .unwrap_or_else(|| domain.domain.platform_scopes.clone()),
                            abilities: existing_manifest
                                .as_ref()
                                .map(|domain| domain.abilities.clone())
                                .unwrap_or_else(|| domain.domain.abilities.clone()),
                            mcp_servers: existing_manifest
                                .as_ref()
                                .map(|domain| domain.mcp_servers.clone())
                                .unwrap_or(domain.domain.mcp_servers),
                            script_tools: existing_manifest
                                .as_ref()
                                .map(|domain| domain.script_tools.clone())
                                .unwrap_or_default(),
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
                                id,
                                name: domain.summary.name,
                                path: domain.summary.path,
                                description: domain.summary.description,
                                command: domain.command,
                                platform_scopes: domain.platform_scopes,
                                abilities: domain.abilities,
                                mcp_servers: domain.mcp_servers,
                                script_tools: existing_manifest
                                    .as_ref()
                                    .map(|domain| domain.script_tools.clone())
                                    .unwrap_or_default(),
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
        ResourceType::Document | ResourceType::KnowledgePack => false,
    }
}

fn agent_with_prompt_document(
    id: Uuid,
    agent: AgentPromptDocument,
    fallback_prompt: Option<PromptConfig>,
) -> nenjo::manifest::AgentManifest {
    agent_from_document(
        id,
        agent.agent,
        fallback_prompt.unwrap_or(agent.prompt_config),
    )
}

pub(super) fn agent_from_document(
    id: Uuid,
    agent: AgentDocument,
    prompt_config: PromptConfig,
) -> nenjo::manifest::AgentManifest {
    nenjo::manifest::AgentManifest {
        id,
        name: agent.summary.name,
        slug: agent.summary.slug,
        description: agent.summary.description,
        prompt_config,
        color: agent.summary.color,
        model: agent.summary.model,
        domains: agent.domains,
        platform_scopes: agent.platform_scopes,
        mcp_servers: agent.mcp_servers,
        script_tools: agent.script_tools,
        abilities: agent.abilities,
        prompt_locked: agent.prompt_locked,
        heartbeat: agent.heartbeat,
    }
}

pub(super) fn ability_from_document(
    id: Uuid,
    ability: AbilityDocument,
    prompt_config: nenjo::types::AbilityPromptConfig,
) -> nenjo::manifest::AbilityManifest {
    nenjo::manifest::AbilityManifest {
        id,
        name: ability.summary.name,
        path: if ability.summary.path.is_empty() {
            None
        } else {
            Some(ability.summary.path)
        },
        description: ability.summary.description,
        activation_condition: ability.activation_condition,
        prompt_config,
        platform_scopes: ability.platform_scopes,
        mcp_servers: ability.mcp_servers,
        script_tools: ability.script_tools,
        source_type: "native".to_string(),
        read_only: false,
        metadata: serde_json::json!({}),
    }
}

fn project_from_document(id: Uuid, project: ProjectDocument) -> nenjo::manifest::ProjectManifest {
    nenjo::manifest::ProjectManifest {
        id,
        name: project.summary.name,
        slug: project.summary.slug,
        description: project.summary.description,
        settings: project.settings,
    }
}

fn council_from_document(id: Uuid, council: CouncilDocument) -> nenjo::manifest::CouncilManifest {
    nenjo::manifest::CouncilManifest {
        id,
        name: council.summary.name,
        delegation_strategy: council.summary.delegation_strategy,
        leader_agent: council.summary.leader_agent,
        members: council
            .members
            .into_iter()
            .map(|member| nenjo::manifest::CouncilMemberManifest {
                agent: member.agent,
                priority: member.priority,
            })
            .collect(),
    }
}

fn upsert_agent(manifest: &mut Manifest, id: Uuid, mut agent: nenjo::manifest::AgentManifest) {
    agent.id = id;
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
            slug: None,
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
            "model": null,
            "domains": [],
            "platform_scopes": [],
            "mcp_servers": [],
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
            "model": null,
            "domains": [],
            "platform_scopes": [],
            "mcp_servers": [],
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
