use nenjo::agents::prompts::PromptConfig;
use nenjo::{Manifest, Slug};
use nenjo_events::ResourceType;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::handlers::manifest::payload::{
    AbilityDocument, AgentDocument, ContextBlockDocument, DecryptedManifestPayload, DomainDocument,
    InlineDocumentMeta, ManifestKind,
};
use crate::handlers::manifest::services::ManifestStore;

use super::plain::apply_inline_upsert;

fn decrypted_string_payload(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

pub(in crate::handlers::manifest) async fn apply_decrypted_manifest_upsert<StoreRt>(
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
    let Some(manifest_kind) = ManifestKind::from_encrypted_object_type(object_type) else {
        debug!(%rt, %id, object_type, "Encrypted manifest payload not handled inline");
        return false;
    };
    let handled_inline = manifest_kind.matches_resource_type(rt) && rt != ResourceType::Routine;
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

    match manifest_kind {
        ManifestKind::ProjectSettings => {
            apply_decrypted_project_settings(manifest, rt, id, decrypted)
        }
        ManifestKind::Agent => {
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
        ManifestKind::Ability => {
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
        ManifestKind::Domain => {
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
                            abilities: existing_manifest
                                .as_ref()
                                .map(|domain| domain.abilities.clone())
                                .unwrap_or_else(|| domain.abilities.clone()),
                            mcp_servers: existing_manifest
                                .as_ref()
                                .map(|domain| domain.mcp_servers.clone())
                                .unwrap_or(domain.mcp_servers),
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
        ManifestKind::ContextBlock => {
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
        ManifestKind::Document => {
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

            let pack = match metadata.pack_slug.as_deref().map(Slug::parse).transpose() {
                Ok(Some(pack)) => pack,
                Ok(None) => {
                    warn!(%rt, %id, "Encrypted document payload received without pack slug metadata");
                    return false;
                }
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Encrypted document payload contained invalid pack slug metadata");
                    return false;
                }
            };

            if let Err(error) = store.write_document_content(&pack, &relative_path, &content) {
                warn!(%rt, %id, error = %error, "Failed to write inline decrypted document");
                return false;
            }

            true
        }
        ManifestKind::Task
        | ManifestKind::Project
        | ManifestKind::Routine
        | ManifestKind::Model
        | ManifestKind::Council => false,
    }
}

fn apply_decrypted_project_settings(
    manifest: &mut Manifest,
    rt: ResourceType,
    id: Uuid,
    decrypted: DecryptedManifestPayload<'_>,
) -> bool {
    if decrypted.object_id != id {
        warn!(
            %rt,
            %id,
            object_id = %decrypted.object_id,
            "Encrypted project settings object id did not match resource id"
        );
        return false;
    }

    let Some(inline_payload) = decrypted.inline_payload else {
        warn!(%rt, %id, "Encrypted project settings received without inline project payload");
        return false;
    };

    let Some(settings) = decrypted.decrypted_payload.as_object() else {
        warn!(%rt, %id, "Decrypted project settings payload was not an object");
        return false;
    };

    let Some(project_payload) = merge_project_settings_payload(inline_payload, settings) else {
        warn!(%rt, %id, "Failed to merge decrypted project settings into inline project payload");
        return false;
    };

    apply_inline_upsert(manifest, rt, id, &project_payload)
}

fn merge_project_settings_payload(
    inline_payload: &serde_json::Value,
    decrypted_settings: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let mut project_payload = inline_payload.clone();
    let project_object = project_payload.as_object_mut()?;
    let settings_value = project_object
        .entry("settings")
        .or_insert_with(|| serde_json::json!({}));
    let settings_object = settings_value.as_object_mut()?;
    for (key, value) in decrypted_settings {
        settings_object.insert(key.clone(), value.clone());
    }
    project_object.remove("encrypted_payload");
    Some(project_payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::manifest::services::NoopManifestStore;

    #[test]
    fn decrypted_string_payload_accepts_raw_string_values() {
        assert_eq!(
            decrypted_string_payload(&serde_json::json!("ok")).as_deref(),
            Some("ok")
        );
    }

    #[tokio::test]
    async fn decrypted_manifest_agent_metadata_is_not_supported() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let decrypted_payload = serde_json::json!({
            "id": id,
            "name": "agent",
            "description": null,
            "color": null,
            "model_id": null,
            "domains": [],
            "platform_scopes": [],
            "mcp_servers": [],
            "abilities": [],
            "prompt_locked": false,
            "heartbeat": null
        });

        assert!(
            !apply_decrypted_manifest_upsert(
                &mut manifest,
                &NoopManifestStore,
                ResourceType::Agent,
                id,
                DecryptedManifestPayload {
                    object_type: "manifest.agent",
                    object_id: id,
                    inline_payload: None,
                    decrypted_payload: &decrypted_payload,
                },
            )
            .await
        );
        assert!(manifest.agents.is_empty());
    }

    #[tokio::test]
    async fn decrypted_project_settings_merge_into_inline_project_payload() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let inline_payload = serde_json::json!({
            "id": id,
            "name": "Project",
            "slug": "project",
            "description": null,
            "settings": {
                "theme": "dark"
            },
            "encrypted_payload": {
                "object_type": "project.settings"
            }
        });
        let decrypted_payload = serde_json::json!({
            "context": "Use the saved project context.",
            "notes": ["one", "two"]
        });

        assert!(
            apply_decrypted_manifest_upsert(
                &mut manifest,
                &NoopManifestStore,
                ResourceType::Project,
                id,
                DecryptedManifestPayload {
                    object_type: "project.settings",
                    object_id: id,
                    inline_payload: Some(&inline_payload),
                    decrypted_payload: &decrypted_payload,
                },
            )
            .await
        );

        assert_eq!(manifest.projects.len(), 1);
        assert_eq!(manifest.projects[0].settings["theme"], "dark");
        assert_eq!(
            manifest.projects[0].settings["context"],
            "Use the saved project context."
        );
        assert_eq!(manifest.projects[0].settings["notes"][1], "two");
    }

    #[tokio::test]
    async fn decrypted_project_settings_rejects_mismatched_object_id() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let inline_payload = serde_json::json!({
            "id": id,
            "name": "Project",
            "slug": "project",
            "description": null,
            "settings": {}
        });
        let decrypted_payload = serde_json::json!({
            "context": "wrong project"
        });

        assert!(
            !apply_decrypted_manifest_upsert(
                &mut manifest,
                &NoopManifestStore,
                ResourceType::Project,
                id,
                DecryptedManifestPayload {
                    object_type: "project.settings",
                    object_id: Uuid::new_v4(),
                    inline_payload: Some(&inline_payload),
                    decrypted_payload: &decrypted_payload,
                },
            )
            .await
        );
        assert!(manifest.projects.is_empty());
    }

    #[tokio::test]
    async fn decrypted_project_settings_rejects_non_object_payload() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let inline_payload = serde_json::json!({
            "id": id,
            "name": "Project",
            "slug": "project",
            "description": null,
            "settings": {}
        });
        let decrypted_payload = serde_json::json!("not settings");

        assert!(
            !apply_decrypted_manifest_upsert(
                &mut manifest,
                &NoopManifestStore,
                ResourceType::Project,
                id,
                DecryptedManifestPayload {
                    object_type: "project.settings",
                    object_id: id,
                    inline_payload: Some(&inline_payload),
                    decrypted_payload: &decrypted_payload,
                },
            )
            .await
        );
        assert!(manifest.projects.is_empty());
    }

    #[tokio::test]
    async fn decrypted_project_settings_without_inline_payload_falls_back() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let decrypted_payload = serde_json::json!({
            "context": "fetch instead"
        });

        assert!(
            !apply_decrypted_manifest_upsert(
                &mut manifest,
                &NoopManifestStore,
                ResourceType::Project,
                id,
                DecryptedManifestPayload {
                    object_type: "project.settings",
                    object_id: id,
                    inline_payload: None,
                    decrypted_payload: &decrypted_payload,
                },
            )
            .await
        );
        assert!(manifest.projects.is_empty());
    }
}
