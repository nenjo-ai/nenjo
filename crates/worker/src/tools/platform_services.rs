use std::sync::Arc;

use nenjo::Manifest;
use nenjo_platform::{
    PlatformManifestBackend, PlatformManifestClient, PlatformResourceIdStore,
    tools::PlatformProjectToolsBackend,
};
use uuid::Uuid;

use crate::bootstrap::WorkerManifestStore;

use super::platform_payload::PlatformPayloadEncoder;

#[derive(Clone, Default)]
pub(crate) struct PlatformToolServices {
    pub manifest_backend:
        Option<Arc<PlatformManifestBackend<WorkerManifestStore, PlatformPayloadEncoder>>>,
    pub project_backend:
        Option<PlatformProjectToolsBackend<WorkerManifestStore, PlatformPayloadEncoder>>,
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
        let manifest_backend = platform_client.as_ref().map(|client| {
            let resource_ids = Arc::new(PlatformResourceIdStore::new(manifest_store.root()));
            let mut backend = PlatformManifestBackend::new(
                manifest_store.clone(),
                client.as_ref().clone(),
                payload_encoder.clone(),
            )
            .with_workspace_dir(workspace_dir)
            .with_library_dir(library_dir)
            .with_cached_org_id(cached_org_id)
            .with_resource_id_store(resource_ids);
            if let Some(manifest) = read_only_manifest.clone() {
                backend = backend.with_read_only_manifest(manifest);
            }
            Arc::new(backend)
        });

        let project_backend = platform_client
            .as_ref()
            .map(|client| PlatformProjectToolsBackend {
                client: client.clone(),
                manifest_store: manifest_store.clone(),
                payload_encoder: payload_encoder.clone(),
                cached_org_id,
            });

        Self {
            manifest_backend,
            project_backend,
            platform_client,
            payload_encoder: Some(payload_encoder),
            cached_org_id,
        }
    }
}
