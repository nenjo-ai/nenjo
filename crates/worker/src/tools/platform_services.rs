use std::sync::Arc;

use nenjo::manifest::local::LocalManifestStore;
use nenjo_platform::{
    PlatformManifestBackend, PlatformManifestClient, tools::PlatformProjectToolsBackend,
};
use uuid::Uuid;

use super::platform_payload::PlatformPayloadEncoder;

#[derive(Clone)]
pub(crate) struct PlatformToolServices {
    pub manifest_backend:
        Option<Arc<PlatformManifestBackend<LocalManifestStore, PlatformPayloadEncoder>>>,
    pub project_backend:
        Option<PlatformProjectToolsBackend<LocalManifestStore, PlatformPayloadEncoder>>,
}

impl PlatformToolServices {
    pub(crate) fn new(
        manifest_store: Arc<LocalManifestStore>,
        platform_client: Option<Arc<PlatformManifestClient>>,
        payload_encoder: PlatformPayloadEncoder,
        cached_org_id: Option<Uuid>,
        workspace_dir: std::path::PathBuf,
    ) -> Self {
        let manifest_backend = platform_client.as_ref().map(|client| {
            Arc::new(
                PlatformManifestBackend::new(
                    manifest_store.clone(),
                    client.as_ref().clone(),
                    payload_encoder.clone(),
                )
                .with_workspace_dir(workspace_dir)
                .with_cached_org_id(cached_org_id),
            )
        });

        let project_backend = platform_client.map(|client| PlatformProjectToolsBackend {
            client,
            manifest_store: manifest_store.clone(),
            payload_encoder,
            cached_org_id,
        });

        Self {
            manifest_backend,
            project_backend,
        }
    }
}
