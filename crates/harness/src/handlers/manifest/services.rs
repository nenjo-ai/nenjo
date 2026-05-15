use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::client::{DocumentSyncMeta, NenjoClient};
use nenjo_events::ResourceType;
use uuid::Uuid;

/// Platform services used by the manifest change handler.
///
/// The harness stores concrete service types so host integrations can avoid
/// trait-object dispatch.
pub struct ManifestServices<StoreRt, McpRt>
where
    StoreRt: ManifestStore,
    McpRt: McpRuntime,
{
    pub client: Arc<NenjoClient>,
    pub store: Arc<StoreRt>,
    pub mcp: Option<Arc<McpRt>>,
}

impl<StoreRt, McpRt> Clone for ManifestServices<StoreRt, McpRt>
where
    StoreRt: ManifestStore,
    McpRt: McpRuntime,
{
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            store: self.store.clone(),
            mcp: self.mcp.clone(),
        }
    }
}

#[async_trait]
/// Host-owned manifest persistence and document side-effect hooks.
pub trait ManifestStore: Send + Sync {
    /// Let the host normalize or materialize resource data before the manifest
    /// is swapped into the running provider and persisted.
    async fn prepare_resource(
        &self,
        _manifest: &mut nenjo::Manifest,
        _resource_type: ResourceType,
    ) -> Result<()> {
        Ok(())
    }

    /// Persist the current manifest cache for one resource type.
    async fn persist_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()>;

    /// Persist removal of one resource from the manifest cache.
    async fn remove_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
        _resource_id: Uuid,
    ) -> Result<()> {
        self.persist_resource(manifest, resource_type).await
    }

    /// Apply host-owned cleanup for a deleted resource using the optional
    /// inline tombstone payload sent with the delete event.
    async fn cleanup_deleted_resource(
        &self,
        _resource_type: ResourceType,
        _resource_id: Uuid,
        _payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        Ok(())
    }

    /// Rebuild the full manifest cache from the platform client.
    async fn full_refresh(&self, client: &NenjoClient) -> Result<nenjo::Manifest>;

    /// Sync document metadata after an inline manifest update.
    async fn sync_document_metadata(
        &self,
        _client: &NenjoClient,
        manifest: &nenjo::Manifest,
        _project_id: Uuid,
        _document_id: Uuid,
        _metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        self.persist_resource(manifest, ResourceType::Document)
            .await
    }

    /// Sync document content after a fetched manifest update.
    async fn sync_document(
        &self,
        _client: &NenjoClient,
        manifest: &nenjo::Manifest,
        _project_id: Uuid,
        _document_id: Uuid,
        _metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        self.persist_resource(manifest, ResourceType::Document)
            .await
    }

    /// Remove document content from the host's local cache.
    fn remove_document(
        &self,
        _manifest: &nenjo::Manifest,
        _project_id: Uuid,
        _document_id: Uuid,
    ) -> Result<()> {
        Ok(())
    }

    /// Write decrypted document content into the host's local cache.
    fn write_document_content(
        &self,
        _manifest: &nenjo::Manifest,
        _project_id: Uuid,
        _relative_path: &str,
        _content: &str,
    ) -> Result<()> {
        Ok(())
    }
}

/// No-op manifest store used when manifest event handling is disabled.
pub struct NoopManifestStore;

#[async_trait]
impl ManifestStore for NoopManifestStore {
    async fn persist_resource(
        &self,
        _manifest: &nenjo::Manifest,
        _resource_type: ResourceType,
    ) -> Result<()> {
        Ok(())
    }

    async fn full_refresh(&self, _client: &NenjoClient) -> Result<nenjo::Manifest> {
        Ok(nenjo::Manifest::default())
    }
}

#[async_trait]
impl<T> ManifestStore for Arc<T>
where
    T: ManifestStore + ?Sized,
{
    async fn prepare_resource(
        &self,
        manifest: &mut nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        (**self).prepare_resource(manifest, resource_type).await
    }

    async fn persist_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        (**self).persist_resource(manifest, resource_type).await
    }

    async fn remove_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
        resource_id: Uuid,
    ) -> Result<()> {
        (**self)
            .remove_resource(manifest, resource_type, resource_id)
            .await
    }

    async fn full_refresh(&self, client: &NenjoClient) -> Result<nenjo::Manifest> {
        (**self).full_refresh(client).await
    }

    async fn sync_document_metadata(
        &self,
        client: &NenjoClient,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        document_id: Uuid,
        metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        (**self)
            .sync_document_metadata(client, manifest, project_id, document_id, metadata)
            .await
    }

    async fn sync_document(
        &self,
        client: &NenjoClient,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        document_id: Uuid,
        metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        (**self)
            .sync_document(client, manifest, project_id, document_id, metadata)
            .await
    }

    fn remove_document(
        &self,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<()> {
        (**self).remove_document(manifest, project_id, document_id)
    }

    fn write_document_content(
        &self,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        relative_path: &str,
        content: &str,
    ) -> Result<()> {
        (**self).write_document_content(manifest, project_id, relative_path, content)
    }
}

#[async_trait]
/// Host-owned MCP reconciliation hook.
pub trait McpRuntime: Send + Sync {
    /// Reconcile active MCP servers after manifest changes.
    async fn reconcile_mcp(&self, servers: &[nenjo::manifest::McpServerManifest]);
}

/// No-op MCP runtime used when MCP reconciliation is not configured.
pub struct NoopMcpRuntime;

#[async_trait]
impl McpRuntime for NoopMcpRuntime {
    async fn reconcile_mcp(&self, _servers: &[nenjo::manifest::McpServerManifest]) {}
}

#[async_trait]
impl<T> McpRuntime for Arc<T>
where
    T: McpRuntime + ?Sized,
{
    async fn reconcile_mcp(&self, servers: &[nenjo::manifest::McpServerManifest]) {
        (**self).reconcile_mcp(servers).await;
    }
}
