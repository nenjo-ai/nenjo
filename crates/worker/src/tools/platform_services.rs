use std::sync::Arc;

use nenjo::Manifest;
use nenjo_platform::{
    PlatformManifestBackend, PlatformManifestClient, PlatformResourceIdStore,
    task_tools::PlatformTaskToolsBackend,
};
use uuid::Uuid;

use crate::bootstrap::WorkerManifestStore;

use super::platform_payload::PlatformPayloadEncoder;

#[derive(Clone, Default)]
pub(crate) struct PlatformToolServices {
    pub manifest_backend:
        Option<Arc<PlatformManifestBackend<WorkerManifestStore, PlatformPayloadEncoder>>>,
    pub task_backend: Option<PlatformTaskToolsBackend<PlatformPayloadEncoder>>,
    pub platform_client: Option<Arc<PlatformManifestClient>>,
    pub payload_encoder: Option<PlatformPayloadEncoder>,
    pub cached_org_id: Option<Uuid>,
}

impl PlatformToolServices {
    pub(crate) fn new(
        manifest_store: Arc<WorkerManifestStore>,
        platform_client: Option<Arc<PlatformManifestClient>>,
        payload_encoder: PlatformPayloadEncoder,
        cached_org_id: Option<Uuid>,
        workspace_dir: std::path::PathBuf,
        library_dir: std::path::PathBuf,
        read_only_manifest: Option<Arc<Manifest>>,
    ) -> Self {
        let resource_ids = Arc::new(PlatformResourceIdStore::new(manifest_store.root()));
        let manifest_backend = platform_client.as_ref().map(|client| {
            let mut backend = PlatformManifestBackend::new(
                manifest_store.clone(),
                client.as_ref().clone(),
                payload_encoder.clone(),
            )
            .with_workspace_dir(workspace_dir)
            .with_library_dir(library_dir)
            .with_cached_org_id(cached_org_id)
            .with_resource_id_store(resource_ids.clone());
            if let Some(manifest) = read_only_manifest.clone() {
                backend = backend.with_read_only_manifest(manifest);
            }
            Arc::new(backend)
        });

        let task_backend = platform_client
            .as_ref()
            .map(|client| PlatformTaskToolsBackend {
                client: client.clone(),
                payload_encoder: payload_encoder.clone(),
                resource_ids,
                cached_org_id,
            });

        Self {
            manifest_backend,
            task_backend,
            platform_client,
            payload_encoder: Some(payload_encoder),
            cached_org_id,
        }
    }
}
