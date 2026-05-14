use anyhow::Result;
use nenjo::Manifest;
use nenjo::client::{DocumentSyncMeta, NenjoClient};
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
    pub resource_id: Uuid,
    pub action: ResourceAction,
    pub project_id: Option<Uuid>,
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
        resource_id,
        action,
        project_id,
        payload,
        encrypted_payload,
    } = change;

    info!(
        %resource_type,
        %resource_id,
        ?action,
        inline = payload.is_some(),
        encrypted = encrypted_payload.is_some(),
        "Manifest resource changed"
    );

    let mut manifest = current.clone();
    let mut source = ManifestApplySource::Ignored;
    let mut applied_inline = false;

    if action == ResourceAction::Deleted {
        apply_delete(&mut manifest, resource_type, resource_id);
        source = ManifestApplySource::Deleted;
    } else {
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
            applied_inline = apply_inline_upsert(&mut manifest, resource_type, resource_id, data);
            if applied_inline {
                source = ManifestApplySource::Inline;
            }
        } else if encrypted_payload.is_some() {
            warn!(
                %resource_type,
                %resource_id,
                "Manifest command still carried encrypted payload after secure-envelope decode"
            );
        }

        if !applied_inline {
            if let Err(e) = apply_upsert(&mut manifest, client, resource_type, resource_id).await {
                warn!(
                    error = %e,
                    %resource_type,
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
                manifest: &manifest,
                resource_id,
                action,
                project_id,
                payload: payload.as_ref(),
                applied_inline,
            })
            .await;
        }
        ResourceType::Project => {}
        _ => {}
    }

    let persist_result = if action == ResourceAction::Deleted {
        store
            .remove_resource(&manifest, resource_type, resource_id)
            .await
    } else {
        store.persist_resource(&manifest, resource_type).await
    };

    if let Err(e) = persist_result {
        warn!(error = %e, rt = %resource_type, "Failed to persist resource cache");
    }

    debug!(?source, %resource_type, %resource_id, "Manifest change applied");
    Ok(ManifestChangeResult { manifest })
}

struct DocumentSideEffectContext<'a, StoreRt>
where
    StoreRt: ManifestStore,
{
    client: &'a NenjoClient,
    store: &'a StoreRt,
    manifest: &'a Manifest,
    resource_id: Uuid,
    action: ResourceAction,
    project_id: Option<Uuid>,
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
        manifest,
        resource_id,
        action,
        project_id,
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
            warn!(?project_id, %resource_id, error = %error, "Failed to deserialize inline document metadata");
            error
        })
        .ok()
        .flatten()
        .map(|meta| {
            let pack_id = meta.pack_id.or(meta.project_id).unwrap_or_else(Uuid::nil);
            DocumentSyncMeta {
            id: meta.id,
            pack_id,
            slug: meta.slug.unwrap_or_else(|| meta.filename.clone()),
            filename: meta.filename,
            path: meta.path,
            title: meta.title,
            kind: meta.kind,
            authority: meta.authority,
            summary: meta.summary,
            status: meta.status,
            tags: meta.tags,
            aliases: meta.aliases,
            keywords: meta.keywords,
            content_type: "application/octet-stream".to_string(),
            size_bytes: meta.size_bytes,
            updated_at: meta.updated_at.to_rfc3339(),
        }});

    let pid = metadata
        .as_ref()
        .map(|meta| meta.pack_id)
        .filter(|id| !id.is_nil())
        .or(project_id);

    let Some(pid) = pid else {
        warn!("Document change without knowledge pack id, skipping sync");
        return;
    };

    if action == ResourceAction::Deleted {
        if let Err(error) = store.remove_document(manifest, pid, resource_id) {
            warn!(%pid, %resource_id, error = %error, "Failed to update local knowledge manifest");
        }
        return;
    }

    let result = if applied_inline {
        store
            .sync_document_metadata(client, manifest, pid, resource_id, metadata.as_ref())
            .await
    } else {
        store
            .sync_document(client, manifest, pid, resource_id, metadata.as_ref())
            .await
    };
    if let Err(e) = result {
        warn!(%pid, %resource_id, error = %e, "Document sync failed");
    }
}
