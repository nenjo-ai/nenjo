use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use nenjo::client::PayloadCodec;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::memory::MarkdownMemory;
use nenjo::{ManifestLoader, Provider};
use nenjo_crypto_auth::EnrollmentBackedKeyProvider;
use nenjo_harness::Harness;
use nenjo_harness::execution_trace::NoopExecutionTraceRuntime;
use nenjo_knowledge::tools::KnowledgePackEntry;
use nenjo_platform::PlatformManifestClient;
use nenjo_platform::library_knowledge::LibraryKnowledgePack;
use nenjo_secure_envelope::SecureEnvelopeCodec;
use tracing::warn;
use uuid::Uuid;

use crate::api_client::NenjoClient;
use crate::bootstrap::{BootstrapAuth, WorkerManifestCache, load_cached_bootstrap_auth};
use crate::config::Config;
use crate::crypto::WorkerAuthProvider;
use crate::external_mcp::ExternalMcpPool;
use crate::providers::registry::ModelProviderRegistry;
use crate::sessions::{LocalSessionCoordinator, WorkerSessionRuntime, WorkerSessionStores};
use crate::tools::platform_payload::PlatformPayloadEncoder;
use crate::tools::platform_services::PlatformToolServices;
use crate::tools::{NativeRuntime, SecurityPolicy, WorkerToolFactory};

pub type WorkerProvider =
    Provider<ModelProviderRegistry, WorkerToolFactory<NativeRuntime>, MarkdownMemory>;

pub type WorkerHarness = Harness<
    WorkerProvider,
    WorkerSessionRuntime,
    NoopExecutionTraceRuntime,
    WorkerManifestCache,
    Arc<ExternalMcpPool>,
>;

/// Fully assembled worker dependencies around the execution harness.
pub struct WorkerAssembly {
    pub harness: WorkerHarness,
    pub api: NenjoClient,
    pub auth_provider: Arc<WorkerAuthProvider>,
    pub session_runtime: WorkerSessionRuntime,
    pub session_stores: WorkerSessionStores,
    pub external_mcp: Arc<ExternalMcpPool>,
}

#[derive(Clone)]
pub struct WorkerCryptoContext {
    pub api_key_id: Uuid,
    pub actor_user_id: Uuid,
    pub org_id: Uuid,
    pub codec: Arc<SecureEnvelopeCodec>,
}

impl WorkerCryptoContext {
    pub fn from_bootstrap_auth(
        auth: &BootstrapAuth,
        auth_provider: Arc<WorkerAuthProvider>,
        enrollment_api: NenjoClient,
    ) -> Result<Self> {
        let api_key_id = auth.api_key_id.ok_or_else(|| {
            anyhow::anyhow!(
                "Backend did not return auth.api_key_id in manifest. \
                 Ensure the backend is updated and the API key is valid."
            )
        })?;
        let key_provider = EnrollmentBackedKeyProvider::new(
            auth_provider,
            enrollment_api,
            api_key_id,
            auth.user_id,
        );

        Ok(Self {
            api_key_id,
            actor_user_id: auth.user_id,
            org_id: auth.org_id,
            codec: Arc::new(SecureEnvelopeCodec::new(key_provider, auth.org_id)),
        })
    }

    pub fn configure_api_client(&self, client: NenjoClient) -> NenjoClient {
        let payload_codec: Arc<dyn PayloadCodec> = self.codec.clone();
        client.with_shared_payload_codec(payload_codec)
    }
}

impl WorkerAssembly {
    pub async fn from_bootstrapped(
        config: &Config,
        auth_provider: Arc<WorkerAuthProvider>,
        api: NenjoClient,
    ) -> Result<Self> {
        let loader = LocalManifestStore::new(&config.manifests_dir);
        let manifest = ManifestLoader::load(&loader).await?;

        let external_mcp = Arc::new(ExternalMcpPool::new());
        external_mcp.reconcile(&manifest.mcp_servers).await;

        let provider =
            build_provider(config, loader, auth_provider.clone(), external_mcp.clone()).await?;

        let worker_name = config
            .harness_name
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| auth_provider.identity().worker_id.to_string());
        let session_coordinator = LocalSessionCoordinator::new();
        let session_stores = WorkerSessionStores::new(&config.state_dir);
        let session_runtime =
            WorkerSessionRuntime::new(session_stores.clone(), session_coordinator, worker_name);

        let harness = nenjo_harness::Harness::builder(provider)
            .with_session_runtime(session_runtime.clone())
            .with_manifest_client(api.clone())
            .with_manifest_store(WorkerManifestCache {
                manifests_dir: config.manifests_dir.clone(),
                workspace_dir: config.workspace_dir.clone(),
                state_dir: config.state_dir.clone(),
            })
            .with_mcp_runtime(external_mcp.clone())
            .build();

        Ok(Self {
            harness,
            api,
            auth_provider,
            session_runtime,
            session_stores,
            external_mcp,
        })
    }
}

pub(crate) async fn build_provider(
    config: &Config,
    loader: LocalManifestStore,
    auth_provider: Arc<WorkerAuthProvider>,
    external_mcp: Arc<ExternalMcpPool>,
) -> Result<WorkerProvider> {
    let provider_registry =
        ModelProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);
    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let platform_tools = build_platform_tool_services(config, auth_provider);
    let tool_factory = WorkerToolFactory::new(
        security,
        NativeRuntime,
        config.clone(),
        platform_tools,
        external_mcp.clone(),
    );

    let memory_dir = config.state_dir.join("memory");
    let mem = MarkdownMemory::new(&memory_dir, &config.state_dir);

    let library_knowledge_packs = load_library_knowledge_packs(&config.workspace_dir);

    Provider::builder()
        .with_loader(loader)
        .with_model_factory(provider_registry)
        .with_tool_factory(tool_factory)
        .with_memory(mem)
        .with_agent_config(config.agent.clone())
        .with_knowledge_pack(
            "builtin:nenjo",
            nenjo_knowledge::builtin::nenjo_pack().clone(),
        )
        .with_knowledge_packs(library_knowledge_packs)
        .build()
        .await
        .context("Failed to build Provider")
}

fn load_library_knowledge_packs(workspace_dir: &Path) -> Vec<KnowledgePackEntry> {
    let library_dir = workspace_dir.join("library");
    let Ok(entries) = std::fs::read_dir(&library_dir) else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let slug = entry.file_name().to_string_lossy().to_string();
            LibraryKnowledgePack::load(entry.path())
                .map(|pack| KnowledgePackEntry::new(format!("workspace:{slug}"), pack))
        })
        .collect()
}

fn build_platform_tool_services(
    config: &Config,
    auth_provider: Arc<WorkerAuthProvider>,
) -> PlatformToolServices {
    let manifest_store = Arc::new(LocalManifestStore::new(config.manifests_dir.clone()));
    let platform_client = PlatformManifestClient::new(config.backend_api_url(), &config.api_key)
        .map(Arc::new)
        .map_err(|error| {
            warn!(error = %error, "Failed to initialize platform API client");
            error
        })
        .ok();
    let payload_encoder = PlatformPayloadEncoder::new(auth_provider);
    let cached_org_id = load_cached_bootstrap_auth(&config.manifests_dir)
        .map(|auth| auth.org_id)
        .filter(|org_id| !org_id.is_nil());

    PlatformToolServices::new(
        manifest_store,
        platform_client,
        payload_encoder,
        cached_org_id,
        config.workspace_dir.clone(),
    )
}
