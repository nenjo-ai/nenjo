use anyhow::Result;
use nenjo::client::{DocumentSyncMeta, NenjoClient};
use nenjo::manifest::{context_block_slug, domain_slug};
use nenjo::{Manifest, Slug};
use nenjo_events::{EncryptedPayload, ResourceAction, ResourceType};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::delete::apply_delete;
use super::fetch::apply_upsert;
use super::inline::{apply_decrypted_manifest_upsert, apply_inline_upsert};
use super::payload::{InlineDocumentMeta, parse_decrypted_manifest_payload};
use super::services::{ManifestStore, McpRuntime};

#[derive(Debug, Clone)]
pub(super) struct ManifestChange {
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

pub(super) async fn apply_manifest_change<StoreRt, McpRt>(
    client: &NenjoClient,
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
        resource,
        action,
        project,
        payload,
        encrypted_payload,
    } = change;

    let resource_id = resolve_resource_id(current, resource_type, &resource, payload.as_ref());

    info!(
        %resource_type,
        %resource,
        project = ?project,
        resource_id = ?resource_id,
        ?action,
        inline = payload.is_some(),
        encrypted = encrypted_payload.is_some(),
        "Manifest resource changed"
    );

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
        if let Some(resource_id) = resource_id {
            if let Some(ref data) = payload
                && let Some(decrypted) = parse_decrypted_manifest_payload(data)
            {
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
            } else if let Some(ref data) = payload {
                applied_inline =
                    apply_inline_upsert(&mut manifest, resource_type, resource_id, data);
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
                let Some(resource_id) = resource_id else {
                    warn!(
                        %resource_type,
                        %resource,
                        "Manifest change did not include a resolvable resource id, falling back to full refresh"
                    );
                    manifest = store.full_refresh(client).await?;
                    if let Some(mcp) = mcp {
                        mcp.reconcile_mcp(&manifest.mcp_servers).await;
                    }
                    source = ManifestApplySource::FullRefresh;
                    return finish_manifest_change(
                        store,
                        manifest,
                        resource_type,
                        &resource,
                        source,
                    )
                    .await;
                };
                if let Err(e) =
                    apply_upsert(&mut manifest, client, resource_type, resource_id).await
                {
                    warn!(
                        error = %e,
                        %resource_type,
                        %resource,
                        %resource_id,
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

async fn finish_manifest_change<StoreRt>(
    store: &StoreRt,
    manifest: Manifest,
    resource_type: ResourceType,
    resource: &Slug,
    source: ManifestApplySource,
) -> Result<ManifestChangeResult>
where
    StoreRt: ManifestStore,
{
    if let Err(e) = store.persist_resource(&manifest, resource_type).await {
        warn!(error = %e, rt = %resource_type, "Failed to persist resource cache");
    }
    debug!(?source, %resource_type, %resource, "Manifest change applied");
    Ok(ManifestChangeResult { manifest })
}

struct DocumentSideEffectContext<'a, StoreRt>
where
    StoreRt: ManifestStore,
{
    client: &'a NenjoClient,
    store: &'a StoreRt,
    resource: &'a Slug,
    action: ResourceAction,
    payload: Option<&'a serde_json::Value>,
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
        applied_inline,
    } = ctx;

    let metadata_value = payload.and_then(|payload| {
        if let Some(decrypted) = parse_decrypted_manifest_payload(payload) {
            decrypted.inline_payload.cloned()
        } else {
            Some(payload.clone())
        }
    });

    let metadata = metadata_value
        .map(serde_json::from_value::<InlineDocumentMeta>)
        .transpose()
        .map_err(|error| {
            warn!(%resource, error = %error, "Failed to deserialize inline document metadata");
            error
        })
        .ok()
        .flatten()
        .map(|meta| DocumentSyncMeta {
            id: Some(meta.id),
            pack_id: meta.pack_id,
            pack_slug: meta.pack_slug.unwrap_or_else(|| "default".to_string()),
            slug: meta.slug.unwrap_or_else(|| meta.filename.clone()),
            filename: meta.filename,
            path: meta.path,
            title: meta.title,
            kind: meta.kind,
            summary: meta.summary,
            tags: meta.tags,
            content_type: "application/octet-stream".to_string(),
            updated_at: meta.updated_at.to_rfc3339(),
        });

    let Some(metadata) = metadata
        .as_ref()
        .filter(|meta| !meta.pack_slug.trim().is_empty())
    else {
        warn!(%resource, "Document change without knowledge pack slug, skipping sync");
        return;
    };
    let pack = metadata.pack_slug.as_str();

    if action == ResourceAction::Deleted {
        if let Err(error) = store.remove_document(resource, Some(metadata)).await {
            warn!(%pack, %resource, error = %error, "Failed to update local knowledge manifest");
        }
        return;
    }

    let result = if applied_inline {
        store
            .sync_document_metadata(client, resource, Some(metadata))
            .await
    } else {
        store.sync_document(client, resource, Some(metadata)).await
    };
    if let Err(e) = result {
        warn!(%pack, %resource, error = %e, "Document sync failed");
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
        .or_else(|| Uuid::parse_str(resource.as_str()).ok())
        .or_else(|| resource_id_from_manifest(manifest, resource_type, resource))
}

fn resource_id_from_payload(payload: &serde_json::Value) -> Option<Uuid> {
    if let Some(decrypted) = parse_decrypted_manifest_payload(payload) {
        return decrypted
            .inline_payload
            .and_then(resource_id_from_payload)
            .or(Some(decrypted.object_id));
    }
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
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|item| item.id),
        ResourceType::Model => manifest
            .models
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|item| item.id),
        ResourceType::Routine => manifest
            .routines
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|item| item.id),
        ResourceType::Project => manifest
            .projects
            .iter()
            .find(|item| item.slug == *resource)
            .map(|item| item.id),
        ResourceType::Council => manifest
            .councils
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|item| item.id),
        ResourceType::Ability => manifest
            .abilities
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|item| item.id),
        ResourceType::ContextBlock => manifest
            .context_blocks
            .iter()
            .find(|item| context_block_slug(&item.path, &item.name) == *resource)
            .map(|item| item.id),
        ResourceType::McpServer => manifest
            .mcp_servers
            .iter()
            .find(|item| Slug::derive(&item.name) == *resource)
            .map(|item| item.id),
        ResourceType::Domain => manifest
            .domains
            .iter()
            .find(|item| domain_slug(&item.path, &item.name) == *resource)
            .map(|item| item.id),
        ResourceType::Document | ResourceType::KnowledgePack => None,
    }
}
