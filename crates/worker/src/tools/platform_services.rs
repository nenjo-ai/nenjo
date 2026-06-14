use std::sync::Arc;

use nenjo::manifest::local::LocalManifestStore;
use nenjo_platform::{
    PlatformManifestBackend, PlatformManifestClient, PlatformResourceIdStore,
    tools::PlatformProjectToolsBackend,
};
use uuid::Uuid;

use super::platform_payload::PlatformPayloadEncoder;

#[derive(Clone, Default)]
pub(crate) struct PlatformToolServices {
    pub manifest_backend:
        Option<Arc<PlatformManifestBackend<LocalManifestStore, PlatformPayloadEncoder>>>,
    pub project_backend:
        Option<PlatformProjectToolsBackend<LocalManifestStore, PlatformPayloadEncoder>>,
    pub platform_client: Option<Arc<PlatformManifestClient>>,
    pub payload_encoder: Option<PlatformPayloadEncoder>,
    pub cached_org_id: Option<Uuid>,
}

impl PlatformToolServices {
    pub(crate) fn new(
        manifest_store: Arc<LocalManifestStore>,
        platform_client: Option<Arc<PlatformManifestClient>>,
        payload_encoder: PlatformPayloadEncoder,
        cached_org_id: Option<Uuid>,
        workspace_dir: std::path::PathBuf,
        library_dir: std::path::PathBuf,
    ) -> Self {
        let manifest_backend = platform_client.as_ref().map(|client| {
            let resource_ids = Arc::new(PlatformResourceIdStore::new(
                manifest_store.root().to_path_buf(),
            ));
            Arc::new(
                PlatformManifestBackend::new(
                    manifest_store.clone(),
                    client.as_ref().clone(),
                    payload_encoder.clone(),
                )
                .with_workspace_dir(workspace_dir)
                .with_library_dir(library_dir)
                .with_cached_org_id(cached_org_id)
                .with_resource_id_store(resource_ids),
            )
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
