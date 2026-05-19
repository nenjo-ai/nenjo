//! Platform manifest change handling.
mod apply;
mod delete;
mod fetch;
mod inline;
mod payload;
mod services;

use std::sync::Arc;

use uuid::Uuid;

use nenjo_events::{EncryptedPayload, ResourceAction, ResourceType};
use nenjo_harness::{Harness, HarnessError, ProviderRuntime, Result};

use apply::{ManifestChange, apply_manifest_change};
pub use services::{ManifestStore, McpRuntime, NoopManifestStore, NoopMcpRuntime};

use crate::api_client::NenjoClient;

#[derive(Clone)]
pub struct ManifestCommandContext<StoreRt, McpRt>
where
    StoreRt: ManifestStore,
    McpRt: McpRuntime,
{
    pub client: Arc<NenjoClient>,
    pub store: StoreRt,
    pub mcp: Option<McpRt>,
}

pub struct ManifestChangedCommand {
    pub resource_type: ResourceType,
    pub resource_id: Uuid,
    pub action: ResourceAction,
    pub project_id: Option<Uuid>,
    pub payload: Option<serde_json::Value>,
    pub encrypted_payload: Option<EncryptedPayload>,
}

#[async_trait::async_trait]
/// Worker integration methods for platform manifest change events.
///
/// This trait keeps platform event handling in the worker while using the
/// harness only to swap the provider manifest after the worker has fetched,
/// decrypted, persisted, and reconciled host-owned resources.
pub trait WorkerManifestHarnessExt<SessionRt, StoreRt, McpRt>
where
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    StoreRt: ManifestStore + 'static,
    McpRt: McpRuntime + 'static,
{
    /// Apply one manifest change event and refresh the running provider.
    async fn handle_manifest_changed(
        &self,
        ctx: &ManifestCommandContext<StoreRt, McpRt>,
        command: ManifestChangedCommand,
    ) -> Result<()>;
}

#[async_trait::async_trait]
impl<P, SessionRt, StoreRt, McpRt> WorkerManifestHarnessExt<SessionRt, StoreRt, McpRt>
    for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    StoreRt: ManifestStore + 'static,
    McpRt: McpRuntime + 'static,
{
    async fn handle_manifest_changed(
        &self,
        ctx: &ManifestCommandContext<StoreRt, McpRt>,
        command: ManifestChangedCommand,
    ) -> Result<()> {
        let ManifestChangedCommand {
            resource_type,
            resource_id,
            action,
            project_id,
            payload,
            encrypted_payload,
        } = command;
        let result = apply_manifest_change(
            ctx.client.as_ref(),
            &ctx.store,
            ctx.mcp.as_ref(),
            &self.manifests().snapshot(),
            ManifestChange {
                resource_type,
                resource_id,
                action,
                project_id,
                payload,
                encrypted_payload,
            },
        )
        .await
        .map_err(HarnessError::manifest_runtime)?;

        self.manifests().replace(result.manifest).await
    }
}
