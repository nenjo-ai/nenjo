use anyhow::Result;
use nenjo::manifest::{ManifestResource, ManifestResourceKind, manifest_by_slug};
use nenjo::{Manifest, Slug};
use nenjo_events::{EncryptedPayload, ManifestResourcePayload, ResourceAction, ResourceType};
use nenjo_platform::api_client::ApiClient;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::delete::apply_delete;
use super::fetch::apply_upsert;
use super::inline::{apply_decrypted_manifest_upsert, apply_inline_upsert};
use super::knowledge::{document_edges_source, parse_knowledge_document_payload};
use super::payload::parse_decrypted_manifest_payload;
use super::services::{ManifestCacheMutation, ManifestStore, McpRuntime};
use nenjo_platform::PlatformResourceKind;

use crate::bootstrap::WorkerManifestCache;

#[derive(Debug, Clone)]
pub(super) struct ManifestChange {
    pub resource_id: Uuid,
    pub resource_type: ResourceType,
    pub resource: Slug,
    pub action: ResourceAction,
    pub project: Option<Slug>,
    pub payload: Option<serde_json::Value>,
    pub encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Clone)]
pub(super) struct ManifestChangeResult {
    pub manifest: Manifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestApplySource {
    Inline,
    DecryptedInline,
    FetchedResource,
    FullRefresh,
    Deleted,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestPayloadState {
    None,
    PlainInline,
    EncryptedTransport,
    DecryptedInline,
}

impl ManifestPayloadState {
    fn from_parts(
        payload: Option<&serde_json::Value>,
        encrypted_payload: Option<&EncryptedPayload>,
    ) -> Self {
        if encrypted_payload.is_some() {
            return Self::EncryptedTransport;
        }
        match payload {
            Some(payload) if parse_decrypted_manifest_payload(payload).is_some() => {
                Self::DecryptedInline
            }
            Some(_) => Self::PlainInline,
            None => Self::None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PlainInline => "plain_inline",
            Self::EncryptedTransport => "encrypted_transport",
            Self::DecryptedInline => "decrypted_inline",
        }
    }
}

pub(super) async fn apply_manifest_change<StoreRt, McpRt>(
    client: &ApiClient,
    store: &StoreRt,
    cache: Option<&WorkerManifestCache>,
    mcp: Option<&McpRt>,
    current: &Manifest,
    change: ManifestChange,
) -> Result<ManifestChangeResult>
where
    StoreRt: ManifestStore,
    McpRt: McpRuntime,
{
    let ManifestChange {
        resource_type,
        resource_id: event_resource_id,
        resource,
        action,
        project,
        payload,
        encrypted_payload,
    } = change;

    let resource_id = if event_resource_id.is_nil() {
        resolve_resource_id(current, resource_type, &resource, payload.as_ref())
    } else {
        Some(event_resource_id)
    };
    let payload_state =
        ManifestPayloadState::from_parts(payload.as_ref(), encrypted_payload.as_ref());

    info!(
        %resource_type,
        %resource,
        ?action,
        payload_state = payload_state.as_str(),
        "Manifest resource changed"
    );
    debug!(
        %resource_type,
        %resource,
        project = ?project,
        resource_id = ?resource_id,
        ?action,
        inline = payload.is_some(),
        encrypted_transport = encrypted_payload.is_some(),
        decrypted_inline = matches!(payload_state, ManifestPayloadState::DecryptedInline),
        payload_state = payload_state.as_str(),
        "Manifest resource change details"
    );

    if resource_type == ResourceType::ModelAssignment {
        persist_model_cache_event(
            cache,
            resource_type,
            action,
            resource_id,
            &resource,
            payload.as_ref(),
        );
        return Ok(ManifestChangeResult {
            manifest: current.clone(),
        });
    }

    if resource_type == ResourceType::ModelCapabilityDefault {
        persist_model_cache_event(
            cache,
            resource_type,
            action,
            resource_id,
            &resource,
            payload.as_ref(),
        );
        return Ok(ManifestChangeResult {
            manifest: current.clone(),
        });
    }

    let previous_resource = if action != ResourceAction::Deleted {
        if let (Some(kind), Some(id)) = (platform_resource_kind(resource_type), resource_id) {
            match store.platform_resource_slug_for_id(kind, id).await {
                Ok(previous) => previous.filter(|previous| previous != &resource),
                Err(error) => {
                    warn!(
                        %resource_type,
                        %resource,
                        resource_id = %id,
                        error = %error,
                        "Failed to resolve prior platform resource slug"
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    if let Some(kind) = platform_resource_kind(resource_type) {
        let sidecar_result = if action == ResourceAction::Deleted {
            if let Some(id) = resource_id {
                store.remove_platform_resource_id_by_id(kind, id).await
            } else {
                store
                    .update_platform_resource_id(kind, &resource, None)
                    .await
            }
        } else {
            store
                .update_platform_resource_id(kind, &resource, resource_id)
                .await
        };
        if let Err(error) = sidecar_result {
            warn!(
                %resource_type,
                %resource,
                resource_id = ?resource_id,
                error = %error,
                "Failed to update platform resource id sidecar"
            );
        }
    } else if resource_type == ResourceType::Document {
        let pack_slug = knowledge_document_pack_slug(payload.as_ref());
        let sidecar_result = if action == ResourceAction::Deleted {
            if let Some(pack) = pack_slug.as_ref() {
                store
                    .update_knowledge_document_resource_id(pack, &resource, None)
                    .await
            } else if let Some(id) = resource_id {
                store.remove_knowledge_document_resource_id_by_id(id).await
            } else {
                Ok(())
            }
        } else if let (Some(pack), Some(id)) = (pack_slug.as_ref(), resource_id) {
            store
                .update_knowledge_document_resource_id(pack, &resource, Some(id))
                .await
        } else {
            Ok(())
        };
        if let Err(error) = sidecar_result {
            warn!(
                %resource,
                pack_slug = ?pack_slug,
                resource_id = ?resource_id,
                error = %error,
                "Failed to update knowledge document resource id sidecar"
            );
        }
    }

    let mut manifest = current.clone();
    if let Some(previous_resource) = previous_resource.as_ref() {
        apply_delete(&mut manifest, resource_type, previous_resource, resource_id);
        debug!(
            %resource_type,
            old_resource = %previous_resource,
            new_resource = %resource,
            resource_id = ?resource_id,
            "Removed stale resource slug before applying rename"
        );
    }
    let mut source = ManifestApplySource::Ignored;
    let mut applied_inline = false;
    let mut fetched_payload = None;

    if action == ResourceAction::Deleted {
        apply_delete(&mut manifest, resource_type, &resource, resource_id);
        if let Err(error) = store
            .cleanup_deleted_resource(resource_type, &resource, resource_id, payload.as_ref())
            .await
        {
            warn!(
                error = %error,
                %resource_type,
                %resource,
                resource_id = ?resource_id,
                "Failed to clean up deleted manifest resource"
            );
        }
        source = ManifestApplySource::Deleted;
    } else {
        if let Some(ref data) = payload {
            if let Some(decrypted) = parse_decrypted_manifest_payload(data) {
                let resource_id = resource_id.unwrap_or(decrypted.object_id);
                applied_inline = apply_decrypted_manifest_upsert(
                    &mut manifest,
                    store,
                    resource_type,
                    resource_id,
                    decrypted,
                )
                .await;
                if applied_inline {
                    source = ManifestApplySource::DecryptedInline;
                }
            } else {
                applied_inline = apply_inline_upsert(&mut manifest, resource_type, data);
                if applied_inline {
                    source = ManifestApplySource::Inline;
                }
            }
        }
        if !applied_inline {
            if matches!(
                resource_type,
                ResourceType::Document | ResourceType::KnowledgePack
            ) {
                source = ManifestApplySource::Ignored;
            } else {
                match apply_upsert(&mut manifest, client, resource_type, &resource).await {
                    Err(e) => {
                        warn!(
                            error = %e,
                            %resource_type,
                            %resource,
                            resource_id = ?resource_id,
                            "Incremental fetch failed, falling back to full refresh"
                        );
                        manifest = store.full_refresh(client).await?;
                        if let Some(mcp) = mcp {
                            mcp.reconcile_mcp(&manifest.mcp_servers).await;
                        }
                        source = ManifestApplySource::FullRefresh;
                    }
                    Ok(fetched_model) => {
                        fetched_payload = fetched_model
                            .map(ManifestResourcePayload::new)
                            .map(ManifestResourcePayload::into_value);
                        source = ManifestApplySource::FetchedResource;
                    }
                }
            }
        }
    }

    if action != ResourceAction::Deleted
        && let Err(error) = store.prepare_resource(&mut manifest, resource_type).await
    {
        warn!(
            error = %error,
            %resource_type,
            %resource,
            resource_id = ?resource_id,
            "Failed to prepare manifest resource"
        );
    }

    match resource_type {
        ResourceType::McpServer => {
            if let Some(mcp) = mcp {
                mcp.reconcile_mcp(&manifest.mcp_servers).await;
            }
        }
        ResourceType::Document => {
            apply_document_side_effects(DocumentSideEffectContext {
                client,
                store,
                resource: &resource,
                action,
                payload: payload.as_ref(),
                encrypted_payload: encrypted_payload.as_ref(),
                applied_inline,
            })
            .await;
        }
        ResourceType::KnowledgePack => {
            if action != ResourceAction::Deleted {
                match store.sync_knowledge_pack(client, &resource).await {
                    Ok(Some(pack)) => {
                        manifest.upsert_resource(ManifestResource::KnowledgePack(pack))
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(pack = %resource, error = %error, "Knowledge pack sync failed");
                    }
                }
            }
        }
        ResourceType::Project => {}
        ResourceType::ModelAssignment | ResourceType::ModelCapabilityDefault => {}
        _ => {}
    }

    if resource_type == ResourceType::Model {
        persist_model_cache_event(
            cache,
            resource_type,
            action,
            resource_id,
            &resource,
            cache_event_payload(payload.as_ref(), fetched_payload.as_ref()),
        );
    }

    match manifest_cache_mutation(
        &manifest,
        resource_type,
        action,
        resource_id,
        &resource,
        previous_resource,
    ) {
        Ok(Some(mutation)) => {
            if let Err(error) = store.persist_change(&mutation).await {
                warn!(%error, rt = %resource_type, "Failed to persist resource cache mutation");
            }
        }
        Ok(None) => {}
        Err(error) => {
            warn!(%error, rt = %resource_type, "Failed to build resource cache mutation");
        }
    }

    debug!(?source, %resource_type, %resource, resource_id = ?resource_id, "Manifest change applied");
    Ok(ManifestChangeResult { manifest })
}

fn cache_event_payload<'a>(
    event_payload: Option<&'a serde_json::Value>,
    fetched_payload: Option<&'a serde_json::Value>,
) -> Option<&'a serde_json::Value> {
    fetched_payload.or_else(|| {
        event_payload.map(|payload| {
            parse_decrypted_manifest_payload(payload)
                .and_then(|decrypted| decrypted.inline_payload)
                .unwrap_or(payload)
        })
    })
}

fn persist_model_cache_event(
    cache: Option<&WorkerManifestCache>,
    resource_type: ResourceType,
    action: ResourceAction,
    resource_id: Option<Uuid>,
    resource: &Slug,
    payload: Option<&serde_json::Value>,
) {
    let Some(cache) = cache else {
        return;
    };
    if let Err(error) =
        cache.persist_manifest_event(resource_type, action, resource_id, resource, payload)
    {
        warn!(
            %error,
            %resource_type,
            %resource,
            "Failed to persist model bootstrap cache event"
        );
    }
}

fn platform_resource_kind(resource_type: ResourceType) -> Option<PlatformResourceKind> {
    match resource_type {
        ResourceType::Agent => Some(PlatformResourceKind::Agent),
        ResourceType::Ability => Some(PlatformResourceKind::Ability),
        ResourceType::Command => Some(PlatformResourceKind::Command),
        ResourceType::Domain => Some(PlatformResourceKind::Domain),
        ResourceType::ContextBlock => Some(PlatformResourceKind::ContextBlock),
        ResourceType::Project => Some(PlatformResourceKind::Project),
        ResourceType::Routine => Some(PlatformResourceKind::Routine),
        ResourceType::Model => Some(PlatformResourceKind::Model),
        ResourceType::Council => Some(PlatformResourceKind::Council),
        ResourceType::McpServer => Some(PlatformResourceKind::McpServer),
        ResourceType::ModelAssignment
        | ResourceType::ModelCapabilityDefault
        | ResourceType::Document
        | ResourceType::KnowledgePack => None,
    }
}

fn manifest_cache_kind(resource_type: ResourceType) -> Option<ManifestResourceKind> {
    match resource_type {
        ResourceType::Agent => Some(ManifestResourceKind::Agent),
        ResourceType::Model => Some(ManifestResourceKind::Model),
        ResourceType::Routine => Some(ManifestResourceKind::Routine),
        ResourceType::Project => Some(ManifestResourceKind::Project),
        ResourceType::Council => Some(ManifestResourceKind::Council),
        ResourceType::Ability => Some(ManifestResourceKind::Ability),
        ResourceType::Command => Some(ManifestResourceKind::Command),
        ResourceType::ContextBlock => Some(ManifestResourceKind::ContextBlock),
        ResourceType::McpServer => Some(ManifestResourceKind::McpServer),
        ResourceType::Domain => Some(ManifestResourceKind::Domain),
        // Knowledge-pack synchronization owns both its library content and its
        // canonical manifest entry. Mixing it into the generic mutation path
        // can reinterpret a not-yet-loaded snapshot as a deletion.
        ResourceType::KnowledgePack => None,
        ResourceType::ModelAssignment
        | ResourceType::ModelCapabilityDefault
        | ResourceType::Document => None,
    }
}

fn manifest_cache_mutation(
    manifest: &Manifest,
    resource_type: ResourceType,
    action: ResourceAction,
    resource_id: Option<Uuid>,
    resource: &Slug,
    previous_slug: Option<Slug>,
) -> Result<Option<ManifestCacheMutation>> {
    let Some(kind) = manifest_cache_kind(resource_type) else {
        return Ok(None);
    };
    if action == ResourceAction::Deleted {
        return Ok(Some(ManifestCacheMutation::delete(
            kind,
            resource_id,
            resource.clone(),
            previous_slug,
        )));
    }

    let Some(snapshot) = manifest.resource_snapshot(kind, resource) else {
        anyhow::bail!(
            "{resource_type} '{resource}' was not present after a non-delete manifest event"
        );
    };
    ManifestCacheMutation::upsert(resource_id, previous_slug, snapshot).map(Some)
}

struct DocumentSideEffectContext<'a, StoreRt>
where
    StoreRt: ManifestStore,
{
    client: &'a ApiClient,
    store: &'a StoreRt,
    resource: &'a Slug,
    action: ResourceAction,
    payload: Option<&'a serde_json::Value>,
    encrypted_payload: Option<&'a EncryptedPayload>,
    applied_inline: bool,
}

async fn apply_document_side_effects<StoreRt>(ctx: DocumentSideEffectContext<'_, StoreRt>)
where
    StoreRt: ManifestStore,
{
    let DocumentSideEffectContext {
        client,
        store,
        resource,
        action,
        payload,
        encrypted_payload,
        applied_inline,
    } = ctx;

    if action == ResourceAction::Deleted {
        let metadata = payload.and_then(|payload| {
            let envelope = if let Some(decrypted) = parse_decrypted_manifest_payload(payload) {
                decrypted.inline_payload
            } else {
                Some(payload)
            }?;
            let parsed = parse_knowledge_document_payload(envelope)?;
            Some(parsed.record.clone())
        });
        if let Err(error) = store.remove_document(resource, metadata.as_ref()).await {
            warn!(%resource, error = %error, "Failed to update local knowledge manifest");
        }
        return;
    }

    let envelope = payload.and_then(|payload| {
        if let Some(decrypted) = parse_decrypted_manifest_payload(payload) {
            decrypted.inline_payload
        } else {
            Some(payload)
        }
    });

    let Some(parsed) = envelope.and_then(parse_knowledge_document_payload) else {
        let result = store.sync_document(client, resource, None).await;
        if let Err(error) = result {
            warn!(%resource, error = %error, "Document sync failed without inline payload");
        }
        return;
    };

    let metadata = parsed.record.clone();
    let edges_source = document_edges_source(&parsed, &metadata.edges);

    if metadata.pack_slug.trim().is_empty() {
        warn!(%resource, "Document change without knowledge pack slug, skipping sync");
        return;
    }
    let pack = metadata.pack_slug.as_str();

    let needs_content_fetch = encrypted_payload.is_some() && !applied_inline;
    let result = if applied_inline || !needs_content_fetch {
        store
            .sync_document_metadata(client, resource, Some(&metadata), Some(edges_source))
            .await
    } else {
        store.sync_document(client, resource, Some(&metadata)).await
    };
    if let Err(e) = result {
        warn!(%pack, %resource, error = %e, "Document sync failed");
    }
}

fn knowledge_document_pack_slug(payload: Option<&serde_json::Value>) -> Option<Slug> {
    let payload = payload?;
    if let Some(decrypted) = parse_decrypted_manifest_payload(payload) {
        return knowledge_document_pack_slug(decrypted.inline_payload);
    }
    let parsed = parse_knowledge_document_payload(payload)?;
    if parsed.record.pack_slug.trim().is_empty() {
        None
    } else {
        Some(Slug::derive(&parsed.record.pack_slug))
    }
}

fn resolve_resource_id(
    manifest: &Manifest,
    resource_type: ResourceType,
    resource: &Slug,
    payload: Option<&serde_json::Value>,
) -> Option<Uuid> {
    payload
        .and_then(resource_id_from_payload)
        .or_else(|| resource_id_from_manifest(manifest, resource_type, resource))
}

fn resource_id_from_payload(payload: &serde_json::Value) -> Option<Uuid> {
    if let Some(decrypted) = parse_decrypted_manifest_payload(payload) {
        return decrypted
            .inline_payload
            .and_then(resource_id_from_payload)
            .or(Some(decrypted.object_id));
    }
    let payload = payload.get("data").unwrap_or(payload);
    payload
        .get("id")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn resource_id_from_manifest(
    manifest: &Manifest,
    resource_type: ResourceType,
    resource: &Slug,
) -> Option<Uuid> {
    match resource_type {
        ResourceType::Agent => manifest_by_slug(&manifest.agents, resource)
            .map(|item| crate::resource_resolver::stable_resource_id("agent", &item.slug)),
        ResourceType::Model => manifest_by_slug(&manifest.models, resource)
            .map(|_| crate::resource_resolver::stable_resource_id("model", resource)),
        ResourceType::Routine => manifest_by_slug(&manifest.routines, resource)
            .map(|item| crate::resource_resolver::stable_resource_id("routine", &item.slug)),
        ResourceType::Project => manifest_by_slug(&manifest.projects, resource)
            .map(|item| crate::resource_resolver::stable_resource_id("project", &item.slug)),
        ResourceType::Council => manifest_by_slug(&manifest.councils, resource)
            .map(|_| crate::resource_resolver::stable_resource_id("council", resource)),
        ResourceType::Ability => manifest_by_slug(&manifest.abilities, resource)
            .map(|_| crate::resource_resolver::stable_resource_id("ability", resource)),
        ResourceType::Command => manifest_by_slug(&manifest.commands, resource)
            .map(|_| crate::resource_resolver::stable_resource_id("command", resource)),
        ResourceType::ContextBlock => manifest_by_slug(&manifest.context_blocks, resource)
            .map(|_| crate::resource_resolver::stable_resource_id("context_block", resource)),
        ResourceType::McpServer => manifest_by_slug(&manifest.mcp_servers, resource)
            .map(|_| crate::resource_resolver::stable_resource_id("mcp_server", resource)),
        ResourceType::Domain => manifest_by_slug(&manifest.domains, resource)
            .map(|_| crate::resource_resolver::stable_resource_id("domain", resource)),
        ResourceType::ModelAssignment
        | ResourceType::ModelCapabilityDefault
        | ResourceType::Document
        | ResourceType::KnowledgePack => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::agents::prompts::PromptConfig;

    fn agent(slug: &str) -> nenjo::manifest::AgentManifest {
        nenjo::manifest::AgentManifest {
            name: slug.to_string(),
            slug: Slug::derive(slug),
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model: None,
            domains: Vec::new(),
            platform_scopes: Vec::new(),
            mcp_servers: Vec::new(),
            script_tools: Vec::new(),
            media: Vec::new(),
            abilities: Vec::new(),
            prompt_locked: false,
            source_type: None,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn agent_cache_mutation_contains_only_the_event_target() {
        let target_id = Uuid::from_u128(1);
        let target = Slug::derive("shop-manager");
        let package_overlay = Slug::derive("bay-agent");
        let manifest = Manifest {
            agents: vec![agent(target.as_str()), agent(package_overlay.as_str())],
            ..Default::default()
        };

        let mutation = manifest_cache_mutation(
            &manifest,
            ResourceType::Agent,
            ResourceAction::Updated,
            Some(target_id),
            &target,
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(mutation.kind(), ManifestResourceKind::Agent);
        assert_eq!(mutation.resource_id(), Some(target_id));
        assert_eq!(mutation.slug(), &target);
        let Some(ManifestResource::Agent(snapshot)) = mutation.resource() else {
            panic!("expected an agent cache snapshot")
        };
        assert_eq!(snapshot.slug, target);
        assert_ne!(snapshot.slug, package_overlay);
    }

    #[test]
    fn missing_snapshot_on_non_delete_event_is_not_a_delete() {
        let error = manifest_cache_mutation(
            &Manifest::default(),
            ResourceType::Agent,
            ResourceAction::Updated,
            Some(Uuid::from_u128(1)),
            &Slug::derive("missing-agent"),
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("was not present"));
    }

    #[test]
    fn knowledge_pack_events_are_not_generic_cache_mutations() {
        let mutation = manifest_cache_mutation(
            &Manifest::default(),
            ResourceType::KnowledgePack,
            ResourceAction::Updated,
            Some(Uuid::from_u128(1)),
            &Slug::derive("product-guides"),
            None,
        )
        .unwrap();

        assert!(mutation.is_none());
    }
}
