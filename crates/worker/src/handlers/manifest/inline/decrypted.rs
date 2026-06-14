use std::borrow::Cow;

use nenjo::agents::prompts::PromptConfig;
use nenjo::{Manifest, Slug};
use nenjo_events::ResourceType;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::handlers::manifest::payload::{
    DecryptedManifestPayload, is_canonical_inline_envelope, parse_inline_record,
};
use crate::handlers::manifest::services::ManifestStore;
use nenjo_platform::SensitiveContentKind;
use nenjo_platform::manifest_contract::{
    AbilityRecord, AgentRecord, ContextBlockRecord, DomainRecord,
};

use super::plain::{apply_inline_upsert, upsert_by_slug};

fn decrypted_string_payload(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn canonical_inline_payload_data(value: &serde_json::Value) -> (Cow<'_, serde_json::Value>, bool) {
    if is_canonical_inline_envelope(value) {
        (Cow::Borrowed(value), true)
    } else {
        (Cow::Borrowed(value), false)
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
    let Some(content_kind) = SensitiveContentKind::from_encrypted_object_type(object_type) else {
        debug!(%rt, %id, object_type, "Encrypted manifest payload not handled inline");
        return false;
    };
    let handled_inline = content_kind.matches_resource_type(rt) && rt != ResourceType::Routine;
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

    match content_kind {
        SensitiveContentKind::ProjectSettings => {
            apply_decrypted_project_settings(manifest, rt, id, decrypted)
        }
        SensitiveContentKind::AgentPrompt => {
            let prompt_config = match serde_json::from_str::<PromptConfig>(&plaintext) {
                Ok(value) => value,
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Failed to parse decrypted prompt config JSON");
                    return false;
                }
            };

            let next_agent = if let Some(agent_payload) = decrypted.inline_payload {
                let (agent_payload, canonical) = canonical_inline_payload_data(agent_payload);
                match parse_inline_record::<AgentRecord>(agent_payload.as_ref()) {
                    Some(record) => record.to_manifest(prompt_config),
                    None if canonical => {
                        warn!(%rt, %id, "Failed to deserialize canonical inline agent payload for prompt merge");
                        return false;
                    }
                    None => {
                        warn!(%rt, %id, "Failed to deserialize inline agent payload for prompt merge");
                        return false;
                    }
                }
            } else {
                warn!(%rt, %id, "Encrypted prompt payload received without inline or cached agent state");
                return false;
            };

            upsert_by_slug(&mut manifest.agents, next_agent);

            true
        }
        SensitiveContentKind::AbilityPrompt => {
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
                let (ability_payload, canonical) = canonical_inline_payload_data(ability_payload);
                match parse_inline_record::<AbilityRecord>(ability_payload.as_ref()) {
                    Some(record) => record.to_manifest(prompt_config),
                    None if canonical => {
                        warn!(%rt, %id, "Failed to deserialize canonical inline ability payload for prompt merge");
                        return false;
                    }
                    None => {
                        warn!(%rt, %id, "Failed to deserialize inline ability payload for prompt merge");
                        return false;
                    }
                }
            } else {
                warn!(%rt, %id, "Encrypted ability prompt received without inline or cached ability state");
                return false;
            };

            upsert_by_slug(&mut manifest.abilities, next_ability);

            true
        }
        SensitiveContentKind::DomainPrompt => {
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
                let (domain_payload, canonical) = canonical_inline_payload_data(domain_payload);
                match parse_inline_record::<DomainRecord>(domain_payload.as_ref()) {
                    Some(record) => record.to_manifest(prompt_config),
                    None if canonical => {
                        warn!(%rt, %id, "Failed to deserialize canonical inline domain payload for prompt merge");
                        return false;
                    }
                    None => {
                        warn!(%rt, %id, "Failed to deserialize inline domain payload for prompt merge");
                        return false;
                    }
                }
            } else {
                warn!(%rt, %id, "Encrypted domain prompt received without inline or cached domain state");
                return false;
            };

            upsert_by_slug(&mut manifest.domains, next_domain);

            true
        }
        SensitiveContentKind::ContextBlockContent => {
            let template = match decrypted_string_payload(decrypted.decrypted_payload) {
                Some(value) => value,
                None => {
                    warn!(%rt, %id, "Failed to parse decrypted context block content");
                    return false;
                }
            };

            let next_block = if let Some(block_payload) = decrypted.inline_payload {
                let (block_payload, canonical) = canonical_inline_payload_data(block_payload);
                match parse_inline_record::<ContextBlockRecord>(block_payload.as_ref()) {
                    Some(record) => record.to_manifest(template),
                    None if canonical => {
                        warn!(%rt, %id, "Failed to deserialize canonical inline context block payload for content merge");
                        return false;
                    }
                    None => {
                        warn!(%rt, %id, "Failed to deserialize inline context block payload for content merge");
                        return false;
                    }
                }
            } else {
                warn!(%rt, %id, "Encrypted context block content received without inline or cached context block state");
                return false;
            };

            upsert_by_slug(&mut manifest.context_blocks, next_block);

            true
        }
        SensitiveContentKind::DocumentContent => {
            let metadata = match decrypted.inline_payload.and_then(
                nenjo_events::ManifestResourcePayload::<
                    nenjo_platform::knowledge_contract::KnowledgeDocumentRecord,
                >::parse,
            ) {
                Some(payload) => payload.data,
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

            let pack = match Slug::parse(&metadata.pack_slug) {
                Ok(pack) => pack,
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
        SensitiveContentKind::TaskContent
        | SensitiveContentKind::HeartbeatInstructions
        | SensitiveContentKind::RoutineCronTask => false,
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

    apply_inline_upsert(manifest, rt, &project_payload)
}

fn merge_project_settings_payload(
    inline_payload: &serde_json::Value,
    decrypted_settings: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    if is_canonical_inline_envelope(inline_payload) {
        let mut project_payload = inline_payload.clone();
        let data = project_payload.get_mut("data")?.as_object_mut()?;
        let settings_value = data
            .entry("settings")
            .or_insert_with(|| serde_json::json!({}));
        let settings_object = settings_value.as_object_mut()?;
        for (key, value) in decrypted_settings {
            settings_object.insert(key.clone(), value.clone());
        }
        data.remove("encrypted_payload");
        return Some(project_payload);
    }

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

    const TS: &str = "2026-05-10T00:00:00Z";

    fn ability_inline_payload(id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "schema": "manifest.resource.v1",
            "data": {
                "id": id,
                "org_id": Uuid::new_v4(),
                "slug": "ability",
                "name": "ability",
                "path": "testing/e2e",
                "description": "ability description",
                "activation_condition": "when needed",
                "platform_scopes": [],
                "mcp_servers": [],
                "script_tools": [],
                "source_type": "native",
                "read_only": false,
                "metadata": {},
                "created_at": TS,
                "updated_at": TS
            }
        })
    }

    fn project_inline_payload(id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "schema": "manifest.resource.v1",
            "data": {
                "id": id,
                "org_id": Uuid::new_v4(),
                "slug": "project",
                "name": "Project",
                "description": null,
                "created_at": TS,
                "updated_at": TS,
                "settings": {
                    "theme": "dark"
                },
                "encrypted_payload": {
                    "object_type": "project.settings"
                }
            }
        })
    }

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
    async fn decrypted_ability_prompt_uses_manifest_event_id() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let inline_payload = ability_inline_payload(id);
        let decrypted_payload = serde_json::json!({
            "developer_prompt": "Use the decrypted prompt."
        });

        assert!(
            apply_decrypted_manifest_upsert(
                &mut manifest,
                &NoopManifestStore,
                ResourceType::Ability,
                id,
                DecryptedManifestPayload {
                    object_type: "manifest.ability.prompt",
                    object_id: id,
                    inline_payload: Some(&inline_payload),
                    decrypted_payload: &decrypted_payload,
                },
            )
            .await
        );

        assert_eq!(manifest.abilities.len(), 1);
        assert_eq!(manifest.abilities[0].name, "ability");
        assert_eq!(
            manifest.abilities[0].prompt_config.developer_prompt,
            "Use the decrypted prompt."
        );
    }

    #[tokio::test]
    async fn decrypted_project_settings_merge_into_inline_project_payload() {
        let id = Uuid::new_v4();
        let mut manifest = Manifest::default();
        let inline_payload = project_inline_payload(id);
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
        let inline_payload = project_inline_payload(id);
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
        let inline_payload = project_inline_payload(id);
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
