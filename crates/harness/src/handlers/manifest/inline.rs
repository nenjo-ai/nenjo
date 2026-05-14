use nenjo::Manifest;
use nenjo::agents::prompts::PromptConfig;
use nenjo_events::ResourceType;
use tracing::{debug, warn};
use uuid::Uuid;

use super::payload::{
    AbilityDocument, AbilityPromptDocument, AgentDocument, AgentPromptDocument,
    ContextBlockContentDocument, ContextBlockDocument, CouncilDocument, DecryptedManifestPayload,
    DomainDocument, DomainPromptDocument, InlineDocumentMeta, ManifestKind, ProjectDocument,
};
use super::services::ManifestStore;

fn decrypted_string_payload(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

pub(super) async fn apply_decrypted_manifest_upsert<StoreRt>(
    manifest: &mut Manifest,
    store: &StoreRt,
    rt: ResourceType,
    id: Uuid,
    decrypted: DecryptedManifestPayload<'_>,
) -> bool
where
    StoreRt: ManifestStore,
{
    let object_type = decrypted.object_type;
    let handled_inline = match rt {
        ResourceType::Agent => {
            object_type == "manifest.agent"
                || object_type
                    == ManifestKind::Agent
                        .encrypted_object_type()
                        .expect("agent prompt object type")
        }
        ResourceType::Ability => {
            object_type
                == ManifestKind::Ability
                    .encrypted_object_type()
                    .expect("ability prompt object type")
        }
        ResourceType::Domain => {
            object_type
                == ManifestKind::Domain
                    .encrypted_object_type()
                    .expect("domain prompt object type")
        }
        ResourceType::ContextBlock => {
            object_type
                == ManifestKind::ContextBlock
                    .encrypted_object_type()
                    .expect("context block content object type")
        }
        ResourceType::Document => {
            object_type
                == ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type")
        }
        _ => false,
    };
    if !handled_inline {
        debug!(%rt, %id, object_type, "Encrypted manifest payload not handled inline");
        return false;
    }

    let plaintext = match decrypted.decrypted_payload {
        serde_json::Value::String(value) => value.clone(),
        value => match serde_json::to_string(value) {
            Ok(value) => value,
            Err(error) => {
                warn!(%rt, %id, error = %error, "Failed to serialize decrypted manifest payload");
                return false;
            }
        },
    };

    match object_type {
        "manifest.agent" => apply_inline_upsert(manifest, rt, id, decrypted.decrypted_payload),
        object_type
            if object_type
                == ManifestKind::Agent
                    .encrypted_object_type()
                    .expect("agent prompt object type") =>
        {
            let prompt_config = match serde_json::from_str::<PromptConfig>(&plaintext) {
                Ok(value) => value,
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Failed to parse decrypted prompt config JSON");
                    return false;
                }
            };

            let next_agent = if let Some(agent_payload) = decrypted.inline_payload {
                match serde_json::from_value::<nenjo::manifest::AgentManifest>(
                    agent_payload.clone(),
                ) {
                    Ok(mut agent) => {
                        agent.prompt_config = prompt_config;
                        agent
                    }
                    Err(_) => {
                        match serde_json::from_value::<AgentDocument>(agent_payload.clone()) {
                            Ok(agent) => {
                                let mut agent: nenjo::manifest::AgentManifest = agent.into();
                                agent.prompt_config = prompt_config;
                                agent
                            }
                            Err(error) => {
                                warn!(%rt, %id, error = %error, "Failed to deserialize inline agent payload for prompt merge");
                                return false;
                            }
                        }
                    }
                }
            } else if let Some(existing) = manifest.agents.iter().find(|agent| agent.id == id) {
                let mut agent = existing.clone();
                agent.prompt_config = prompt_config;
                agent
            } else {
                warn!(%rt, %id, "Encrypted prompt payload received without inline or cached agent state");
                return false;
            };

            if let Some(pos) = manifest.agents.iter().position(|agent| agent.id == id) {
                manifest.agents[pos] = next_agent;
            } else {
                manifest.agents.push(next_agent);
            }

            true
        }
        object_type
            if object_type
                == ManifestKind::Ability
                    .encrypted_object_type()
                    .expect("ability prompt object type") =>
        {
            let prompt_config = match serde_json::from_str::<nenjo::types::AbilityPromptConfig>(
                &plaintext,
            ) {
                Ok(value) => value,
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Failed to parse decrypted ability prompt JSON");
                    return false;
                }
            };

            let next_ability = if let Some(ability_payload) = decrypted.inline_payload {
                match serde_json::from_value::<AbilityDocument>(ability_payload.clone()) {
                    Ok(ability) => {
                        let mut ability: nenjo::manifest::AbilityManifest = ability.into();
                        ability.prompt_config = prompt_config.clone();
                        ability
                    }
                    Err(error) => {
                        warn!(%rt, %id, error = %error, "Failed to deserialize inline ability payload for prompt merge");
                        return false;
                    }
                }
            } else if let Some(existing) =
                manifest.abilities.iter().find(|ability| ability.id == id)
            {
                let mut ability = existing.clone();
                ability.prompt_config = prompt_config.clone();
                ability
            } else {
                warn!(%rt, %id, "Encrypted ability prompt received without inline or cached ability state");
                return false;
            };

            if let Some(pos) = manifest
                .abilities
                .iter()
                .position(|ability| ability.id == id)
            {
                manifest.abilities[pos] = next_ability;
            } else {
                manifest.abilities.push(next_ability);
            }

            true
        }
        object_type
            if object_type
                == ManifestKind::Domain
                    .encrypted_object_type()
                    .expect("domain prompt object type") =>
        {
            let prompt_config = match serde_json::from_str::<nenjo::types::DomainPromptConfig>(
                &plaintext,
            ) {
                Ok(value) => value,
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Failed to parse decrypted domain prompt JSON");
                    return false;
                }
            };

            let next_domain = if let Some(domain_payload) = decrypted.inline_payload {
                match serde_json::from_value::<DomainDocument>(domain_payload.clone()) {
                    Ok(domain) => {
                        let existing_manifest = manifest
                            .domains
                            .iter()
                            .find(|domain_entry| domain_entry.id == id)
                            .cloned();
                        nenjo::manifest::DomainManifest {
                            id: domain.summary.id,
                            name: domain.summary.name,
                            path: domain.summary.path,
                            display_name: domain.summary.display_name,
                            description: domain.summary.description,
                            command: domain.command,
                            platform_scopes: existing_manifest
                                .as_ref()
                                .map(|domain| domain.platform_scopes.clone())
                                .unwrap_or_else(|| domain.platform_scopes.clone()),
                            ability_ids: existing_manifest
                                .as_ref()
                                .map(|domain| domain.ability_ids.clone())
                                .unwrap_or_else(|| domain.ability_ids.clone()),
                            mcp_server_ids: existing_manifest
                                .as_ref()
                                .map(|domain| domain.mcp_server_ids.clone())
                                .unwrap_or_else(|| domain.mcp_server_ids.clone()),
                            prompt_config: prompt_config.clone(),
                        }
                    }
                    Err(error) => {
                        warn!(%rt, %id, error = %error, "Failed to deserialize inline domain payload for prompt merge");
                        return false;
                    }
                }
            } else if let Some(existing) = manifest.domains.iter().find(|domain| domain.id == id) {
                let mut domain = existing.clone();
                domain.prompt_config = prompt_config.clone();
                domain
            } else {
                warn!(%rt, %id, "Encrypted domain prompt received without inline or cached domain state");
                return false;
            };

            if let Some(pos) = manifest.domains.iter().position(|domain| domain.id == id) {
                manifest.domains[pos] = next_domain;
            } else {
                manifest.domains.push(next_domain);
            }

            true
        }
        object_type
            if object_type
                == ManifestKind::ContextBlock
                    .encrypted_object_type()
                    .expect("context block content object type") =>
        {
            let template = match decrypted_string_payload(decrypted.decrypted_payload) {
                Some(value) => value,
                None => {
                    warn!(%rt, %id, "Failed to parse decrypted context block content");
                    return false;
                }
            };

            let next_block = if let Some(block_payload) = decrypted.inline_payload {
                match serde_json::from_value::<ContextBlockDocument>(block_payload.clone()) {
                    Ok(block) => nenjo::manifest::ContextBlockManifest {
                        id: block.summary.id,
                        name: block.summary.name,
                        path: block.summary.path,
                        display_name: block.summary.display_name,
                        description: block.summary.description,
                        template,
                    },
                    Err(error) => {
                        warn!(%rt, %id, error = %error, "Failed to deserialize inline context block payload for content merge");
                        return false;
                    }
                }
            } else if let Some(existing) =
                manifest.context_blocks.iter().find(|block| block.id == id)
            {
                let mut block = existing.clone();
                block.template = template;
                block
            } else {
                warn!(%rt, %id, "Encrypted context block content received without inline or cached context block state");
                return false;
            };

            if let Some(pos) = manifest
                .context_blocks
                .iter()
                .position(|block| block.id == id)
            {
                manifest.context_blocks[pos] = next_block;
            } else {
                manifest.context_blocks.push(next_block);
            }

            true
        }
        object_type
            if object_type
                == ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type") =>
        {
            let metadata = match decrypted
                .inline_payload
                .cloned()
                .map(serde_json::from_value::<InlineDocumentMeta>)
            {
                Some(Ok(metadata)) => metadata,
                Some(Err(error)) => {
                    warn!(%rt, %id, error = %error, "Failed to deserialize inline document metadata payload");
                    return false;
                }
                None => {
                    warn!(%rt, %id, "Encrypted document payload received without inline metadata");
                    return false;
                }
            };

            let content = match decrypted_string_payload(decrypted.decrypted_payload) {
                Some(content) => content,
                None => {
                    warn!(%rt, %id, "Failed to parse decrypted document content");
                    return false;
                }
            };

            let relative_path = match metadata.path.as_deref().map(|path| path.trim_matches('/')) {
                Some(path) if !path.is_empty() => format!("{path}/{}", metadata.filename),
                _ => metadata.filename.clone(),
            };

            if let Err(error) = store.write_document_content(
                manifest,
                metadata
                    .pack_id
                    .or(metadata.project_id)
                    .unwrap_or_else(Uuid::nil),
                &relative_path,
                &content,
            ) {
                warn!(%rt, %id, error = %error, "Failed to write inline decrypted document");
                return false;
            }

            true
        }
        _ => false,
    }
}

/// Apply an inline payload directly to the manifest without an API fetch.
/// Returns `true` if the payload was successfully applied, `false` if
/// deserialization failed (caller should fall back to API fetch).
pub(super) fn apply_inline_upsert(
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

    #[test]
    fn decrypted_string_payload_accepts_raw_string_values() {
        assert_eq!(
            decrypted_string_payload(&serde_json::json!("ok")).as_deref(),
            Some("ok")
        );
    }

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
