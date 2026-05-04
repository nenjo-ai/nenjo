//! Manifest change handler — incremental resource updates.

use anyhow::Result;
use nenjo::agents::prompts::PromptConfig;
use nenjo_platform::{
    AbilityDocument, AbilityPromptDocument, AgentDocument, ContextBlockContentDocument,
    ContextBlockDocument, CouncilDocument, DomainDocument, DomainPromptDocument, ManifestKind,
    ProjectDocument,
};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo_events::{EncryptedPayload, ResourceAction, ResourceType};

use crate::crypto::WorkerAuthProvider;
use crate::crypto::decrypt_text_with_provider;
use crate::harness::CommandContext;
use crate::harness::loader::FileSystemManifestLoader;

static CACHE_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
struct InlineDocumentMeta {
    id: Uuid,
    project_id: Uuid,
    filename: String,
    path: Option<String>,
    size_bytes: i64,
    updated_at: chrono::DateTime<chrono::Utc>,
}

/// Handle a manifest.changed event.
///
/// Fetches only the changed resource and applies an incremental update to
/// the manifest. Falls back to a full refresh if the fetch fails.
pub async fn handle_manifest_changed(
    ctx: &CommandContext,
    resource_type: ResourceType,
    resource_id: Uuid,
    action: ResourceAction,
    project_id: Option<Uuid>,
    payload: Option<serde_json::Value>,
    encrypted_payload: Option<EncryptedPayload>,
) -> Result<()> {
    info!(
        %resource_type,
        %resource_id,
        ?action,
        inline = payload.is_some(),
        encrypted = encrypted_payload.is_some(),
        "Manifest resource changed"
    );

    let applied_inline = if action == ResourceAction::Deleted {
        apply_delete(ctx, resource_type, resource_id);
        false
    } else {
        // Try encrypted inline payload first, then plaintext, then API fetch.
        let applied_inline = if let Some(ref data) = encrypted_payload {
            apply_inline_encrypted_upsert(ctx, resource_type, resource_id, payload.as_ref(), data)
                .await
        } else if let Some(ref data) = payload {
            apply_inline_upsert(ctx, resource_type, resource_id, data)
        } else {
            false
        };

        if !applied_inline && let Err(e) = apply_upsert(ctx, resource_type, resource_id).await {
            warn!(
                error = %e,
                %resource_type,
                %resource_id,
                "Incremental fetch failed, falling back to full refresh"
            );
            full_refresh(ctx).await?;
            return Ok(());
        }
        applied_inline
    };

    // Side-effects for specific resource types
    match resource_type {
        ResourceType::McpServer => {
            let manifest = ctx.provider().manifest().clone();
            ctx.external_mcp.reconcile(&manifest.mcp_servers).await;
        }
        ResourceType::Document => {
            if let Some(pid) = project_id {
                let manifest = ctx.provider().manifest().clone();
                let slug = project_workspace_slug(&manifest, pid);
                let project_dir = ctx.config.workspace_dir.join(&slug);
                if action == ResourceAction::Deleted {
                    match crate::harness::doc_sync::remove_manifest_entry(&project_dir, resource_id)
                    {
                        Ok(Some(filename)) => {
                            if let Err(error) =
                                crate::harness::doc_sync::remove_project_knowledge_entry(
                                    &project_dir,
                                    resource_id,
                                )
                            {
                                warn!(%pid, %resource_id, error = %error, "Failed to update local project knowledge manifest");
                            }
                            if let Err(error) = crate::harness::doc_sync::delete_document_file(
                                &project_dir,
                                &filename,
                            ) {
                                warn!(%pid, %resource_id, error = %error, "Failed to delete document file");
                            }
                        }
                        Ok(None) => {
                            debug!(%pid, %resource_id, "Deleted document was not present in local manifest");
                        }
                        Err(error) => {
                            warn!(%pid, %resource_id, error = %error, "Failed to update local document manifest");
                        }
                    }
                } else {
                    let metadata = payload
                        .as_ref()
                        .cloned()
                        .map(serde_json::from_value::<InlineDocumentMeta>)
                        .transpose()
                        .map_err(|error| {
                            warn!(%pid, %resource_id, error = %error, "Failed to deserialize inline document metadata");
                            error
                        })
                        .ok()
                        .flatten();
                    let metadata =
                        metadata.map(|meta| crate::harness::api_client::DocumentSyncMeta {
                            id: meta.id,
                            filename: meta.filename,
                            path: meta.path,
                            title: None,
                            kind: None,
                            authority: None,
                            summary: None,
                            status: None,
                            tags: Vec::new(),
                            content_type: "application/octet-stream".to_string(),
                            size_bytes: meta.size_bytes,
                            updated_at: meta.updated_at.to_rfc3339(),
                        });
                    let result = if applied_inline {
                        crate::harness::doc_sync::sync_document_metadata(
                            &ctx.api,
                            &project_dir,
                            pid,
                            resource_id,
                            metadata.as_ref(),
                        )
                        .await
                    } else {
                        crate::harness::doc_sync::sync_document(
                            &ctx.api,
                            &project_dir,
                            pid,
                            resource_id,
                            &ctx.config.state_dir,
                            metadata.as_ref(),
                        )
                        .await
                    };
                    if let Err(e) = result {
                        warn!(%pid, %resource_id, error = %e, "Document sync failed");
                    }
                }
            } else {
                warn!("Document change without project_id, skipping sync");
            }
        }
        ResourceType::Project => {}
        _ => {}
    }

    // Persist the changed resource to the filesystem cache
    persist_cache(ctx, resource_type);

    if should_refresh_domain_sessions(resource_type) {
        refresh_active_domain_sessions(ctx).await;
    }

    Ok(())
}

fn should_refresh_domain_sessions(resource_type: ResourceType) -> bool {
    matches!(
        resource_type,
        ResourceType::Agent
            | ResourceType::Ability
            | ResourceType::Domain
            | ResourceType::McpServer
    )
}

fn project_workspace_slug(manifest: &nenjo::manifest::Manifest, project_id: Uuid) -> String {
    manifest
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| project.slug.clone())
        .unwrap_or_else(|| project_id.to_string())
}

async fn refresh_active_domain_sessions(ctx: &CommandContext) {
    let active_sessions: Vec<_> = ctx
        .domains
        .iter()
        .map(|entry| {
            (
                *entry.key(),
                entry.agent_id,
                entry.project_id,
                entry.domain_command.clone(),
                entry.turn_number,
            )
        })
        .collect();

    for (session_id, agent_id, project_id, domain_command, turn_number) in active_sessions {
        match crate::harness::Harness::rebuild_domain_session(
            &ctx.provider,
            session_id,
            agent_id,
            project_id,
            &domain_command,
            turn_number,
        )
        .await
        {
            Ok(session) => {
                ctx.domains.insert(session_id, session);
                info!(%session_id, %agent_id, domain = %domain_command, "Refreshed active domain session after manifest change");
            }
            Err(error) => {
                warn!(
                    %session_id,
                    %agent_id,
                    domain = %domain_command,
                    error = %error,
                    "Failed to refresh active domain session after manifest change"
                );
            }
        }
    }
}

async fn apply_inline_encrypted_upsert(
    ctx: &CommandContext,
    rt: ResourceType,
    id: Uuid,
    inline_payload: Option<&serde_json::Value>,
    encrypted_payload: &EncryptedPayload,
) -> bool {
    let object_type = encrypted_payload.object_type.as_str();
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

    let auth_provider = match WorkerAuthProvider::load_or_create(
        ctx.config.state_dir.join("crypto"),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            warn!(%rt, %id, error = %error, "Failed to load worker auth provider for manifest decrypt");
            return false;
        }
    };

    let plaintext = match decrypt_text_with_provider(&auth_provider, encrypted_payload).await {
        Ok(plaintext) => plaintext,
        Err(error) => {
            warn!(%rt, %id, error = %error, "Failed to decrypt inline manifest payload");
            return false;
        }
    };

    match object_type {
        "manifest.agent" => {
            let value = match serde_json::from_str::<serde_json::Value>(&plaintext) {
                Ok(value) => value,
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Failed to parse decrypted manifest payload JSON");
                    return false;
                }
            };

            apply_inline_upsert(ctx, rt, id, &value)
        }
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

            let mut manifest = ctx.provider().manifest().clone();
            let next_agent = if let Some(agent_payload) = inline_payload {
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

            ctx.swap_provider(ctx.provider().with_manifest(manifest));
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

            let mut manifest = ctx.provider().manifest().clone();
            let next_ability = if let Some(ability_payload) = inline_payload {
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

            ctx.swap_provider(ctx.provider().with_manifest(manifest));
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

            let mut manifest = ctx.provider().manifest().clone();
            let next_domain = if let Some(domain_payload) = inline_payload {
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

            ctx.swap_provider(ctx.provider().with_manifest(manifest));
            true
        }
        object_type
            if object_type
                == ManifestKind::ContextBlock
                    .encrypted_object_type()
                    .expect("context block content object type") =>
        {
            let template = match serde_json::from_str::<String>(&plaintext) {
                Ok(value) => value,
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Failed to parse decrypted context block content JSON");
                    return false;
                }
            };

            let mut manifest = ctx.provider().manifest().clone();
            let next_block = if let Some(block_payload) = inline_payload {
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

            ctx.swap_provider(ctx.provider().with_manifest(manifest));
            true
        }
        object_type
            if object_type
                == ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type") =>
        {
            let metadata = match inline_payload
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

            let value = match serde_json::from_str::<serde_json::Value>(&plaintext) {
                Ok(value) => value,
                Err(error) => {
                    warn!(%rt, %id, error = %error, "Failed to parse decrypted document JSON");
                    return false;
                }
            };
            let content = match value.as_str() {
                Some(content) => content,
                None => {
                    warn!(%rt, %id, "Decrypted document payload was not a string");
                    return false;
                }
            };

            let manifest = ctx.provider().manifest().clone();
            let slug = project_workspace_slug(&manifest, metadata.project_id);
            let project_dir = ctx.config.workspace_dir.join(slug);

            if let Err(error) = crate::harness::doc_sync::write_document_content(
                &project_dir,
                &match metadata.path.as_deref().map(|path| path.trim_matches('/')) {
                    Some(path) if !path.is_empty() => format!("{path}/{}", metadata.filename),
                    _ => metadata.filename.clone(),
                },
                content,
            ) {
                warn!(%rt, %id, error = %error, "Failed to write inline decrypted document");
                return false;
            }

            true
        }
        _ => false,
    }
}

async fn decrypt_prompt_payload(
    ctx: &CommandContext,
    encrypted_payload: &EncryptedPayload,
) -> Option<PromptConfig> {
    let plaintext = decrypt_string_payload(
        ctx,
        encrypted_payload,
        ManifestKind::Agent
            .encrypted_object_type()
            .expect("agent prompt object type"),
        "prompt",
    )
    .await?;

    match serde_json::from_str::<PromptConfig>(&plaintext) {
        Ok(prompt_config) => Some(prompt_config),
        Err(error) => {
            warn!(error = %error, "Failed to parse decrypted prompt config JSON");
            None
        }
    }
}

async fn decrypt_string_payload(
    ctx: &CommandContext,
    encrypted_payload: &EncryptedPayload,
    expected_object_type: &str,
    label: &str,
) -> Option<String> {
    if encrypted_payload.object_type != expected_object_type {
        warn!(
            object_type = %encrypted_payload.object_type,
            expected_object_type,
            "Encrypted payload object type mismatch during fallback fetch"
        );
        return None;
    }

    let auth_provider =
        match WorkerAuthProvider::load_or_create(ctx.config.state_dir.join("crypto")) {
            Ok(provider) => provider,
            Err(error) => {
                warn!(error = %error, "Failed to load worker auth provider for payload decrypt");
                return None;
            }
        };

    match decrypt_text_with_provider(&auth_provider, encrypted_payload).await {
        Ok(plaintext) => Some(plaintext),
        Err(error) => {
            warn!(error = %error, label = label, "Failed to decrypt encrypted payload");
            None
        }
    }
}

/// Apply an inline payload directly to the manifest without an API fetch.
/// Returns `true` if the payload was successfully applied, `false` if
/// deserialization failed (caller should fall back to API fetch).
fn apply_inline_upsert(
    ctx: &CommandContext,
    rt: ResourceType,
    id: Uuid,
    data: &serde_json::Value,
) -> bool {
    let mut manifest = ctx.provider().manifest().clone();

    if rt == ResourceType::Agent {
        return match serde_json::from_value::<nenjo::manifest::AgentManifest>(data.clone()) {
            Ok(agent) => {
                if let Some(pos) = manifest.agents.iter().position(|r| r.id == id) {
                    let mut next = agent;
                    next.prompt_config = manifest.agents[pos].prompt_config.clone();
                    manifest.agents[pos] = next;
                    debug!(%rt, %id, "Applied inline agent payload");
                    ctx.swap_provider(ctx.provider().with_manifest(manifest));
                    true
                } else {
                    debug!(
                        %rt,
                        %id,
                        "Inline agent payload missing prompt_config for uncached agent, falling back to fetch"
                    );
                    false
                }
            }
            Err(_) => match serde_json::from_value::<AgentDocument>(data.clone()) {
                Ok(agent) => {
                    if let Some(pos) = manifest.agents.iter().position(|r| r.id == id) {
                        let mut agent: nenjo::manifest::AgentManifest = agent.into();
                        agent.prompt_config = manifest.agents[pos].prompt_config.clone();
                        manifest.agents[pos] = agent;
                        debug!(%rt, %id, "Applied inline agent document payload");
                        ctx.swap_provider(ctx.provider().with_manifest(manifest));
                        true
                    } else {
                        debug!(
                            %rt,
                            %id,
                            "Inline agent document payload missing prompt_config for uncached agent, falling back to fetch"
                        );
                        false
                    }
                }
                Err(e) => {
                    warn!(%rt, %id, error = %e, "Failed to deserialize inline agent payload, will fetch");
                    false
                }
            },
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

    let ok = match rt {
        ResourceType::Agent => return false,
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
            return match serde_json::from_value::<nenjo::manifest::AbilityManifest>(data.clone()) {
                Ok(ability) => {
                    if let Some(pos) = manifest.abilities.iter().position(|r| r.id == id) {
                        manifest.abilities[pos] = ability;
                    } else {
                        manifest.abilities.push(ability);
                    }
                    ctx.swap_provider(ctx.provider().with_manifest(manifest));
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
                        ctx.swap_provider(ctx.provider().with_manifest(manifest));
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
                            ctx.swap_provider(ctx.provider().with_manifest(manifest));
                            true
                        }
                        Err(e) => {
                            warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                            false
                        }
                    },
                },
            };
        }
        ResourceType::ContextBlock => {
            return match serde_json::from_value::<nenjo::manifest::ContextBlockManifest>(
                data.clone(),
            ) {
                Ok(block) => {
                    if let Some(pos) = manifest.context_blocks.iter().position(|r| r.id == id) {
                        manifest.context_blocks[pos] = block;
                    } else {
                        manifest.context_blocks.push(block);
                    }
                    ctx.swap_provider(ctx.provider().with_manifest(manifest));
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
                        ctx.swap_provider(ctx.provider().with_manifest(manifest));
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
                            ctx.swap_provider(ctx.provider().with_manifest(manifest));
                            true
                        }
                        Err(e) => {
                            warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                            false
                        }
                    },
                },
            };
        }
        ResourceType::McpServer => inline_upsert!(mcp_servers, nenjo::manifest::McpServerManifest),
        ResourceType::Domain => {
            return match serde_json::from_value::<nenjo::manifest::DomainManifest>(data.clone()) {
                Ok(domain) => {
                    if let Some(pos) = manifest.domains.iter().position(|r| r.id == id) {
                        manifest.domains[pos] = domain;
                    } else {
                        manifest.domains.push(domain);
                    }
                    ctx.swap_provider(ctx.provider().with_manifest(manifest));
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
                        ctx.swap_provider(ctx.provider().with_manifest(manifest));
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
                            ctx.swap_provider(ctx.provider().with_manifest(manifest));
                            true
                        }
                        Err(e) => {
                            warn!(%rt, %id, error = %e, "Failed to deserialize inline payload, will fetch");
                            false
                        }
                    },
                },
            };
        }
        ResourceType::Document => return false, // documents don't live in manifest
    };

    if ok {
        ctx.swap_provider(ctx.provider().with_manifest(manifest));
    }
    ok
}

/// Remove a deleted resource from the in-memory manifest.
fn apply_delete(ctx: &CommandContext, rt: ResourceType, id: Uuid) {
    let mut manifest = ctx.provider().manifest().clone();

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
    ctx.swap_provider(ctx.provider().with_manifest(manifest));
}

/// Fetch a single resource from the API and upsert it into the manifest.
async fn apply_upsert(ctx: &CommandContext, rt: ResourceType, id: Uuid) -> Result<()> {
    let mut manifest = ctx.provider().manifest().clone();

    macro_rules! upsert {
        ($field:ident, $fetch:ident) => {{
            match ctx.api.$fetch(id).await? {
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
        ResourceType::Agent => match ctx.api.fetch_agent(id).await? {
            Some(mut item) => {
                if let Some(prompt_response) = ctx.api.fetch_agent_prompt_config(id).await? {
                    if let Some(encrypted_payload) = prompt_response.encrypted_payload.as_ref() {
                        if let Some(prompt_config) =
                            decrypt_prompt_payload(ctx, encrypted_payload).await
                        {
                            item.prompt_config = prompt_config;
                        } else if let Some(existing) =
                            manifest.agents.iter().find(|agent| agent.id == id)
                        {
                            item.prompt_config = existing.prompt_config.clone();
                        }
                    } else if let Some(prompt_config) = prompt_response.prompt_config {
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
        ResourceType::ContextBlock => match ctx.api.fetch_context_block_summary(id).await? {
            Some(summary) => {
                let existing_template = manifest
                    .context_blocks
                    .iter()
                    .find(|block| block.id == id)
                    .map(|block| block.template.clone())
                    .unwrap_or_default();
                let content = ctx.api.fetch_context_block_content(id).await?;
                let template = match content {
                    Some(content) => {
                        if let Some(encrypted_payload) = content.encrypted_payload.as_ref() {
                            decrypt_string_payload(
                                ctx,
                                encrypted_payload,
                                ManifestKind::ContextBlock
                                    .encrypted_object_type()
                                    .expect("context block content object type"),
                                "context block content",
                            )
                            .await
                            .unwrap_or(existing_template)
                        } else {
                            content.template.unwrap_or(existing_template)
                        }
                    }
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

    ctx.swap_provider(ctx.provider().with_manifest(manifest));
    Ok(())
}

/// Full re-fetch of all manifest data (fallback).
async fn full_refresh(ctx: &CommandContext) -> Result<()> {
    crate::harness::manifest::sync(
        &ctx.api,
        &ctx.config.manifests_dir,
        &ctx.config.workspace_dir,
        &ctx.config.state_dir,
    )
    .await?;

    let loader = FileSystemManifestLoader::new(&ctx.config.manifests_dir);
    let manifest = nenjo::ManifestLoader::load(&loader).await?;

    ctx.external_mcp.reconcile(&manifest.mcp_servers).await;

    ctx.swap_provider(ctx.provider().with_manifest(manifest));

    info!("Full manifest refresh complete");
    Ok(())
}

/// Persist the current manifest to the filesystem cache for this resource type.
fn persist_cache(ctx: &CommandContext, rt: ResourceType) {
    let manifest = ctx.provider().manifest().clone();
    let manifests_dir = &ctx.config.manifests_dir;

    let result = match rt {
        ResourceType::Model => atomic_write(manifests_dir, "models.json", &manifest.models),
        ResourceType::Agent => atomic_write(manifests_dir, "agents.json", &manifest.agents),
        ResourceType::Routine => atomic_write(manifests_dir, "routines.json", &manifest.routines),
        ResourceType::Project => atomic_write(manifests_dir, "projects.json", &manifest.projects),
        ResourceType::Council => atomic_write(manifests_dir, "councils.json", &manifest.councils),
        ResourceType::Ability => crate::harness::manifest::sync_tree(
            &manifests_dir.join("abilities"),
            &manifest.abilities,
        ),
        ResourceType::ContextBlock => crate::harness::manifest::sync_tree(
            &manifests_dir.join("context_blocks"),
            &manifest.context_blocks,
        ),
        ResourceType::McpServer => {
            atomic_write(manifests_dir, "mcp_servers.json", &manifest.mcp_servers)
        }
        ResourceType::Domain => {
            crate::harness::manifest::sync_tree(&manifests_dir.join("domains"), &manifest.domains)
        }
        ResourceType::Document => return,
    };

    if let Err(e) = result {
        warn!(error = %e, %rt, "Failed to persist resource cache");
    }
}

fn atomic_write<T: serde::Serialize>(
    dir: &std::path::Path,
    filename: &str,
    value: &T,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(filename);
    let tmp = unique_tmp_path(&target, filename);
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

fn unique_tmp_path(target: &std::path::Path, filename: &str) -> std::path::PathBuf {
    let nonce = CACHE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    target.with_file_name(format!(".{filename}.{pid}.{nonce}.tmp"))
}
