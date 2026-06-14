use anyhow::Result;
use nenjo::manifest::{context_block_slug, domain_slug};
use nenjo::{Manifest, Slug};
use nenjo_events::{EncryptedPayload, ResourceAction, ResourceType};
use nenjo_platform::api_client::ApiClient;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::delete::apply_delete;
use super::fetch::apply_upsert;
use super::inline::{apply_decrypted_manifest_upsert, apply_inline_upsert};
use super::knowledge::{document_edges_source, parse_knowledge_document_payload};
use super::payload::parse_decrypted_manifest_payload;
use super::services::{ManifestStore, McpRuntime};
use nenjo_platform::PlatformResourceKind;

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
    let mut source = ManifestApplySource::Ignored;
    let mut applied_inline = false;

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
                if let Err(e) = apply_upsert(&mut manifest, client, resource_type, &resource).await
                {
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
                } else {
                    source = ManifestApplySource::FetchedResource;
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
            if action != ResourceAction::Deleted
                && let Err(error) = store.sync_knowledge_pack(client, &resource).await
            {
                warn!(pack = %resource, error = %error, "Knowledge pack sync failed");
            }
        }
        ResourceType::Project => {}
        _ => {}
    }

    let persist_result = if action == ResourceAction::Deleted {
        store
            .remove_resource(&manifest, resource_type, &resource)
            .await
    } else {
        store.persist_resource(&manifest, resource_type).await
    };

    if let Err(e) = persist_result {
        warn!(error = %e, rt = %resource_type, "Failed to persist resource cache");
    }

    debug!(?source, %resource_type, %resource, resource_id = ?resource_id, "Manifest change applied");
    Ok(ManifestChangeResult { manifest })
}

fn platform_resource_kind(resource_type: ResourceType) -> Option<PlatformResourceKind> {
    match resource_type {
        ResourceType::Agent => Some(PlatformResourceKind::Agent),
        ResourceType::Ability => Some(PlatformResourceKind::Ability),
        ResourceType::Domain => Some(PlatformResourceKind::Domain),
        ResourceType::ContextBlock => Some(PlatformResourceKind::ContextBlock),
        ResourceType::Project => Some(PlatformResourceKind::Project),
        ResourceType::Routine => Some(PlatformResourceKind::Routine),
        ResourceType::Model => Some(PlatformResourceKind::Model),
        ResourceType::Council => Some(PlatformResourceKind::Council),
        ResourceType::McpServer => Some(PlatformResourceKind::McpServer),
        ResourceType::Document | ResourceType::KnowledgePack => None,
    }
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
        ResourceType::Agent => manifest
            .agents
            .iter()
            .find(|item| item.slug == *resource)
            .map(|item| crate::resource_resolver::stable_resource_id("agent", &item.slug)),
        ResourceType::Model => manifest
            .models
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|_| crate::resource_resolver::stable_resource_id("model", resource)),
        ResourceType::Routine => manifest
            .routines
            .iter()
            .find(|item| item.slug == *resource)
            .map(|item| crate::resource_resolver::stable_resource_id("routine", &item.slug)),
        ResourceType::Project => manifest
            .projects
            .iter()
            .find(|item| item.slug == *resource)
            .map(|item| crate::resource_resolver::stable_resource_id("project", &item.slug)),
        ResourceType::Council => manifest
            .councils
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|_| crate::resource_resolver::stable_resource_id("council", resource)),
        ResourceType::Ability => manifest
            .abilities
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|_| crate::resource_resolver::stable_resource_id("ability", resource)),
        ResourceType::ContextBlock => manifest
            .context_blocks
            .iter()
            .find(|item| context_block_slug(&item.path, &item.name) == *resource)
            .map(|_| crate::resource_resolver::stable_resource_id("context_block", resource)),
        ResourceType::McpServer => manifest
            .mcp_servers
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|_| crate::resource_resolver::stable_resource_id("mcp_server", resource)),
        ResourceType::Domain => manifest
            .domains
            .iter()
            .find(|item| domain_slug(&item.path, &item.name) == *resource)
            .map(|_| crate::resource_resolver::stable_resource_id("domain", resource)),
        ResourceType::Document | ResourceType::KnowledgePack => None,
    }
}
