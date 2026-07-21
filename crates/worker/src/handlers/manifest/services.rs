use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo::Slug;
use nenjo::manifest::{KnowledgePackManifest, ManifestResource, ManifestResourceKind};
use nenjo_events::ResourceType;
use nenjo_platform::PlatformResourceKind;
use nenjo_platform::api_client::{ApiClient, KnowledgeDocumentRecord};
use uuid::Uuid;

use super::knowledge::DocumentEdgesSource;

/// One canonical cache change derived from a single platform manifest event.
///
/// The mutation deliberately contains no aggregate [`nenjo::Manifest`]. Runtime
/// manifests may include package and workspace overlays that are not platform
/// records and therefore must never be written into the canonical cache.
#[derive(Debug, Clone)]
pub enum ManifestCacheMutation {
    Upsert {
        resource_id: Option<Uuid>,
        previous_slug: Option<Slug>,
        resource: Box<ManifestResource>,
    },
    Delete {
        kind: ManifestResourceKind,
        resource_id: Option<Uuid>,
        slug: Slug,
        previous_slug: Option<Slug>,
    },
}

impl ManifestCacheMutation {
    /// Construct an event-scoped upsert. Agent cache entries require their
    /// platform id because `agents.json` retains that id alongside the manifest.
    pub fn upsert(
        resource_id: Option<Uuid>,
        previous_slug: Option<Slug>,
        resource: ManifestResource,
    ) -> Result<Self> {
        let kind = resource.kind();
        let slug = resource.slug();
        if kind == ManifestResourceKind::Agent && resource_id.is_none() {
            anyhow::bail!("agent '{}' is missing a platform id", slug);
        }
        Ok(Self::Upsert {
            previous_slug,
            resource_id,
            resource: Box::new(resource),
        })
    }

    /// Construct an event-scoped deletion. `previous_slug` also removes a stale
    /// alias when an upsert fetch concludes that a renamed resource is gone.
    pub fn delete(
        kind: ManifestResourceKind,
        resource_id: Option<Uuid>,
        slug: Slug,
        previous_slug: Option<Slug>,
    ) -> Self {
        Self::Delete {
            kind,
            slug,
            previous_slug,
            resource_id,
        }
    }

    pub fn kind(&self) -> ManifestResourceKind {
        match self {
            Self::Upsert { resource, .. } => resource.kind(),
            Self::Delete { kind, .. } => *kind,
        }
    }

    pub fn slug(&self) -> &Slug {
        match self {
            Self::Upsert { resource, .. } => resource.slug_ref(),
            Self::Delete { slug, .. } => slug,
        }
    }

    pub fn previous_slug(&self) -> Option<&Slug> {
        match self {
            Self::Upsert { previous_slug, .. } | Self::Delete { previous_slug, .. } => {
                previous_slug.as_ref()
            }
        }
    }

    pub fn resource_id(&self) -> Option<Uuid> {
        match self {
            Self::Upsert { resource_id, .. } | Self::Delete { resource_id, .. } => *resource_id,
        }
    }

    pub fn resource(&self) -> Option<&ManifestResource> {
        match self {
            Self::Upsert { resource, .. } => Some(resource.as_ref()),
            Self::Delete { .. } => None,
        }
    }

    pub fn is_delete(&self) -> bool {
        matches!(self, Self::Delete { .. })
    }
}

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

    /// Persist only the canonical resource affected by the current event.
    async fn persist_change(&self, mutation: &ManifestCacheMutation) -> Result<()>;

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

    /// Resolve the currently cached slug for a platform resource UUID.
    async fn platform_resource_slug_for_id(
        &self,
        _kind: PlatformResourceKind,
        _resource_id: Uuid,
    ) -> Result<Option<Slug>> {
        Ok(None)
    }

    /// Remove all slug aliases for one platform resource id (e.g. after rename + delete).
    async fn remove_platform_resource_id_by_id(
        &self,
        _kind: PlatformResourceKind,
        _resource_id: Uuid,
    ) -> Result<()> {
        Ok(())
    }

    /// Persist or remove pack-scoped knowledge document platform ids.
    async fn update_knowledge_document_resource_id(
        &self,
        _pack: &Slug,
        _doc: &Slug,
        _resource_id: Option<Uuid>,
    ) -> Result<()> {
        Ok(())
    }

    /// Remove all pack aliases for one knowledge document platform id.
    async fn remove_knowledge_document_resource_id_by_id(&self, _resource_id: Uuid) -> Result<()> {
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
    async fn sync_knowledge_pack(
        &self,
        _client: &ApiClient,
        _pack: &Slug,
    ) -> Result<Option<KnowledgePackManifest>> {
        Ok(None)
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
    async fn persist_change(&self, _mutation: &ManifestCacheMutation) -> Result<()> {
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

    async fn persist_change(&self, mutation: &ManifestCacheMutation) -> Result<()> {
        (**self).persist_change(mutation).await
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

    async fn platform_resource_slug_for_id(
        &self,
        kind: PlatformResourceKind,
        resource_id: Uuid,
    ) -> Result<Option<Slug>> {
        (**self)
            .platform_resource_slug_for_id(kind, resource_id)
            .await
    }

    async fn remove_platform_resource_id_by_id(
        &self,
        kind: PlatformResourceKind,
        resource_id: Uuid,
    ) -> Result<()> {
        (**self)
            .remove_platform_resource_id_by_id(kind, resource_id)
            .await
    }

    async fn update_knowledge_document_resource_id(
        &self,
        pack: &Slug,
        doc: &Slug,
        resource_id: Option<Uuid>,
    ) -> Result<()> {
        (**self)
            .update_knowledge_document_resource_id(pack, doc, resource_id)
            .await
    }

    async fn remove_knowledge_document_resource_id_by_id(&self, resource_id: Uuid) -> Result<()> {
        (**self)
            .remove_knowledge_document_resource_id_by_id(resource_id)
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

    async fn sync_knowledge_pack(
        &self,
        client: &ApiClient,
        pack: &Slug,
    ) -> Result<Option<KnowledgePackManifest>> {
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
