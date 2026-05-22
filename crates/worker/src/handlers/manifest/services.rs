use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::client::{DocumentSyncMeta, NenjoClient};
use nenjo_events::ResourceType;
use uuid::Uuid;

/// Host-owned manifest persistence and document side-effect hooks.
///
/// Manifest change handling is worker-owned because local caches, document
/// files, decrypted payload materialization, and knowledge-pack sync are host
/// policy. Implementors persist the local manifest representation and perform
/// any filesystem or API side effects needed before the harness swaps provider
/// snapshots.
#[async_trait]
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
        _document_id: Uuid,
        _metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        Ok(())
    }

    /// Sync document content after a fetched manifest update.
    async fn sync_document(
        &self,
        _client: &NenjoClient,
        _document_id: Uuid,
        _metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        Ok(())
    }

    /// Remove document content from the host's local cache.
    async fn remove_document(
        &self,
        _document_id: Uuid,
        _metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        Ok(())
    }

    /// Sync a whole library knowledge pack into the host's local cache.
    async fn sync_knowledge_pack(&self, _client: &NenjoClient, _pack_id: Uuid) -> Result<()> {
        Ok(())
    }

    /// Write decrypted document content into the host's local cache.
    fn write_document_content(
        &self,
        _pack_id: Uuid,
        _pack_slug: Option<&str>,
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
        document_id: Uuid,
        metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        (**self)
            .sync_document_metadata(client, document_id, metadata)
            .await
    }

    async fn sync_document(
        &self,
        client: &NenjoClient,
        document_id: Uuid,
        metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        (**self).sync_document(client, document_id, metadata).await
    }

    async fn remove_document(
        &self,
        document_id: Uuid,
        metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        (**self).remove_document(document_id, metadata).await
    }

    async fn sync_knowledge_pack(&self, client: &NenjoClient, pack_id: Uuid) -> Result<()> {
        (**self).sync_knowledge_pack(client, pack_id).await
    }

    fn write_document_content(
        &self,
        pack_id: Uuid,
        pack_slug: Option<&str>,
        relative_path: &str,
        content: &str,
    ) -> Result<()> {
        (**self).write_document_content(pack_id, pack_slug, relative_path, content)
    }
}

/// Host-owned MCP reconciliation hook.
///
/// The worker calls this after manifest updates so external MCP server pools can
/// be started, stopped, or refreshed independently of harness execution.
#[async_trait]
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
