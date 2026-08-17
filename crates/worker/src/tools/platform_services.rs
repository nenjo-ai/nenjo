use std::path::PathBuf;
use std::sync::Arc;

use nenjo::Manifest;
use nenjo_platform::{
    PlatformManifestBackend, PlatformManifestClient, PlatformResourceIdStore,
    artifacts::{PlatformArtifactMaterializer, PlatformArtifactRepository},
    task_tools::PlatformTaskToolsBackend,
};
use uuid::Uuid;

use crate::bootstrap::WorkerManifestStore;

use super::platform_payload::PlatformPayloadEncoder;

pub(crate) struct PlatformToolServiceDependencies {
    pub manifest_store: Arc<WorkerManifestStore>,
    pub platform_client: Option<Arc<PlatformManifestClient>>,
    pub payload_encoder: PlatformPayloadEncoder,
}

pub(crate) struct PlatformToolServiceConfig {
    pub cached_org_id: Option<Uuid>,
    pub workspace_dir: PathBuf,
    pub library_dir: PathBuf,
    pub state_dir: PathBuf,
    pub read_only_manifest: Option<Arc<Manifest>>,
}

#[derive(Clone, Default)]
pub(crate) struct PlatformToolServices {
    pub manifest_backend:
        Option<Arc<PlatformManifestBackend<WorkerManifestStore, PlatformPayloadEncoder>>>,
    pub task_backend: Option<PlatformTaskToolsBackend<PlatformPayloadEncoder>>,
    pub platform_client: Option<Arc<PlatformManifestClient>>,
    pub payload_encoder: Option<PlatformPayloadEncoder>,
    pub cached_org_id: Option<Uuid>,
    pub artifact_materializer: Option<Arc<PlatformArtifactMaterializer<PlatformPayloadEncoder>>>,
}

impl PlatformToolServices {
    pub(crate) fn new(
        dependencies: PlatformToolServiceDependencies,
        config: PlatformToolServiceConfig,
    ) -> Self {
        let PlatformToolServiceDependencies {
            manifest_store,
            platform_client,
            payload_encoder,
        } = dependencies;
        let PlatformToolServiceConfig {
            cached_org_id,
            workspace_dir,
            library_dir,
            state_dir,
            read_only_manifest,
        } = config;
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
        let artifact_materializer = platform_client.as_ref().map(|client| {
            Arc::new(PlatformArtifactMaterializer::new(
                PlatformArtifactRepository::new(client.clone()),
                payload_encoder.clone(),
                state_dir.join("artifact-cache"),
            ))
        });

        Self {
            manifest_backend,
            task_backend,
            platform_client,
            payload_encoder: Some(payload_encoder),
            cached_org_id,
            artifact_materializer,
        }
    }
}
