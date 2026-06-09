use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::Slug;
use nenjo_events::ResourceType;
use nenjo_platform::PlatformResourceKind;
use nenjo_platform::api_client::{ApiClient, KnowledgeDocumentRecord};
use uuid::Uuid;

use super::knowledge::DocumentEdgesSource;

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
        _resource: &Slug,
    ) -> Result<()> {
        self.persist_resource(manifest, resource_type).await
    }

    /// Apply host-owned cleanup for a deleted resource using the optional
    /// inline tombstone payload sent with the delete event.
    async fn cleanup_deleted_resource(
        &self,
        _resource_type: ResourceType,
        _resource: &Slug,
        _resource_id: Option<Uuid>,
        _payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        Ok(())
    }

    /// Persist or remove platform-private resource id metadata for encrypted write paths.
    async fn update_platform_resource_id(
        &self,
        _kind: PlatformResourceKind,
        _resource: &Slug,
        _resource_id: Option<Uuid>,
    ) -> Result<()> {
        Ok(())
    }

    /// Rebuild the full manifest cache from the platform client.
    async fn full_refresh(&self, client: &ApiClient) -> Result<nenjo::Manifest>;

    /// Sync document metadata after an inline manifest update.
    async fn sync_document_metadata(
        &self,
        _client: &ApiClient,
        _doc: &Slug,
        _metadata: Option<&KnowledgeDocumentRecord>,
        _edges: Option<DocumentEdgesSource<'_>>,
    ) -> Result<()> {
        Ok(())
    }

    /// Sync document content after a fetched manifest update.
    async fn sync_document(
        &self,
        _client: &ApiClient,
        _doc: &Slug,
        _metadata: Option<&KnowledgeDocumentRecord>,
    ) -> Result<()> {
        Ok(())
    }

    /// Remove document content from the host's local cache.
    async fn remove_document(
        &self,
        _doc: &Slug,
        _metadata: Option<&KnowledgeDocumentRecord>,
    ) -> Result<()> {
        Ok(())
    }

    /// Sync a whole library knowledge pack into the host's local cache.
    async fn sync_knowledge_pack(&self, _client: &ApiClient, _pack: &Slug) -> Result<()> {
        Ok(())
    }

    /// Write decrypted document content into the host's local cache.
    fn write_document_content(
        &self,
        _pack: &Slug,
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

    async fn full_refresh(&self, _client: &ApiClient) -> Result<nenjo::Manifest> {
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
        resource: &Slug,
    ) -> Result<()> {
        (**self)
            .remove_resource(manifest, resource_type, resource)
            .await
    }

    async fn cleanup_deleted_resource(
        &self,
        resource_type: ResourceType,
        resource: &Slug,
        resource_id: Option<Uuid>,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        (**self)
            .cleanup_deleted_resource(resource_type, resource, resource_id, payload)
            .await
    }

    async fn full_refresh(&self, client: &ApiClient) -> Result<nenjo::Manifest> {
        (**self).full_refresh(client).await
    }

    async fn update_platform_resource_id(
        &self,
        kind: PlatformResourceKind,
        resource: &Slug,
        resource_id: Option<Uuid>,
    ) -> Result<()> {
        (**self)
            .update_platform_resource_id(kind, resource, resource_id)
            .await
    }

    async fn sync_document_metadata(
        &self,
        client: &ApiClient,
        doc: &Slug,
        metadata: Option<&KnowledgeDocumentRecord>,
        edges: Option<DocumentEdgesSource<'_>>,
    ) -> Result<()> {
        (**self)
            .sync_document_metadata(client, doc, metadata, edges)
            .await
    }

    async fn sync_document(
        &self,
        client: &ApiClient,
        doc: &Slug,
        metadata: Option<&KnowledgeDocumentRecord>,
    ) -> Result<()> {
        (**self).sync_document(client, doc, metadata).await
    }

    async fn remove_document(
        &self,
        doc: &Slug,
        metadata: Option<&KnowledgeDocumentRecord>,
    ) -> Result<()> {
        (**self).remove_document(doc, metadata).await
    }

    async fn sync_knowledge_pack(&self, client: &ApiClient, pack: &Slug) -> Result<()> {
        (**self).sync_knowledge_pack(client, pack).await
    }

    fn write_document_content(
        &self,
        pack: &Slug,
        relative_path: &str,
        content: &str,
    ) -> Result<()> {
        (**self).write_document_content(pack, relative_path, content)
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
