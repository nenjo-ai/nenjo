use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use nenjo::client::PayloadCodec;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::memory::MarkdownMemory;
use nenjo::{ManifestLoader, Provider};
use nenjo_crypto_auth::EnrollmentBackedKeyProvider;
use nenjo_harness::Harness;
use nenjo_knowledge::KnowledgePack;
use nenjo_knowledge::tools::KnowledgePackEntry;
use nenjo_platform::PlatformManifestClient;
use nenjo_platform::library_knowledge::LibraryKnowledgePack;
use nenjo_secure_envelope::SecureEnvelopeCodec;
use tracing::warn;
use uuid::Uuid;

use crate::api_client::NenjoClient;
use crate::bootstrap::{BootstrapAuth, load_cached_bootstrap_auth};
use crate::config::Config;
use crate::crypto::WorkerAuthProvider;
use crate::external_mcp::ExternalMcpPool;
use crate::package_manifests::PackageManifestLoader;
use crate::providers::registry::ModelProviderRegistry;
use crate::sessions::{LocalSessionCoordinator, WorkerSessionRuntime, WorkerSessionStores};
use crate::tools::platform_payload::PlatformPayloadEncoder;
use crate::tools::platform_services::PlatformToolServices;
use crate::tools::{NativeRuntime, SecurityPolicy, WorkerToolFactory};

pub type WorkerProvider =
    Provider<ModelProviderRegistry, WorkerToolFactory<NativeRuntime>, MarkdownMemory>;

pub type WorkerHarness = Harness<WorkerProvider, WorkerSessionRuntime>;

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
        let session_runtime = WorkerSessionRuntime::with_coordinator(
            session_stores.clone(),
            session_coordinator,
            worker_name,
        );

        let harness = nenjo_harness::Harness::builder(provider)
            .with_session_runtime(session_runtime.clone())
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
    let library_knowledge_packs = provider_knowledge_packs(&config.config_dir, &platform_tools);
    let tool_factory = WorkerToolFactory::new(
        security,
        NativeRuntime,
        config.clone(),
        platform_tools,
        external_mcp.clone(),
    );

    let memory_dir = config.state_dir.join("memory");
    let mem = MarkdownMemory::new(&memory_dir, &config.state_dir);

    Provider::builder()
        .with_loader(loader)
        .with_loader(PackageManifestLoader::with_packages_dir(
            config.config_dir.clone(),
            config.config_dir.join("packages"),
        ))
        .with_loader(PackageManifestLoader::with_packages_dir(
            config.config_dir.join("platform_pkgs"),
            config.config_dir.join("platform_pkgs"),
        ))
        .with_loader(PackageManifestLoader::new(config.workspace_dir.clone()))
        .with_model_factory(provider_registry)
        .with_tool_factory(tool_factory)
        .with_memory(mem)
        .with_agent_config(config.agent.clone())
        .with_knowledge_packs(library_knowledge_packs)
        .build()
        .await
        .context("Failed to build Provider")
}

fn load_library_knowledge_packs(nenjo_home: &Path) -> Vec<KnowledgePackEntry> {
    let mut packs = Vec::new();
    let platform_dir = nenjo_home.join("library").join("platform");
    if let Ok(entries) = std::fs::read_dir(&platform_dir) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let slug = entry.file_name().to_string_lossy().to_string();
            if let Some(pack) = LibraryKnowledgePack::load(entry.path())
                && let Ok(entry) = KnowledgePackEntry::library(slug, pack)
            {
                packs.push(entry);
            }
        }
    }

    let repos_dir = nenjo_home.join("library").join("repos");
    for pack_dir in find_library_pack_dirs(&repos_dir) {
        let Some(pack) = LibraryKnowledgePack::load(&pack_dir) else {
            continue;
        };
        let package_name = pack.manifest().pack_id().to_string();
        if let Ok(entry) = KnowledgePackEntry::package(package_name, pack) {
            packs.push(entry);
        }
    }
    packs
}

fn provider_knowledge_packs(
    nenjo_home: &Path,
    platform_tools: &PlatformToolServices,
) -> Vec<KnowledgePackEntry> {
    if platform_tools.manifest_backend.is_some() {
        Vec::new()
    } else {
        load_library_knowledge_packs(nenjo_home)
    }
}

fn find_library_pack_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if LibraryKnowledgePack::load(&path).is_some() {
            found.push(path);
        } else {
            found.extend(find_library_pack_dirs(&path));
        }
    }
    found
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
        config.config_dir.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_library_pack(root: &Path) {
        let pack_dir = root.join("library").join("platform").join("demo");
        let docs_dir = pack_dir.join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("intro.md"), "# Intro").unwrap();
        std::fs::write(
            pack_dir.join(LibraryKnowledgePack::MANIFEST_FILENAME),
            r#"{
              "pack_id": "demo",
              "pack_version": "1",
              "schema_version": 1,
              "root_uri": "library://demo/",
              "content_hash": "",
              "synced_at": "",
              "docs": [
                {
                  "id": "intro",
                  "path": "library://demo/intro.md",
                  "source_path": "docs/intro.md",
                  "title": "Intro",
                  "summary": "Intro doc",
                  "kind": "reference",
                  "tags": [],
                  "related": [],
                  "updated_at": ""
                }
              ]
            }"#,
        )
        .unwrap();
    }

    fn platform_tools(root: &Path, with_manifest_backend: bool) -> PlatformToolServices {
        let auth_provider =
            Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
        let manifest_store = Arc::new(LocalManifestStore::new(root.join("manifests")));
        let platform_client = with_manifest_backend
            .then(|| PlatformManifestClient::new("http://localhost:1", "test-api-key").unwrap())
            .map(Arc::new);

        PlatformToolServices::new(
            manifest_store,
            platform_client,
            PlatformPayloadEncoder::new(auth_provider),
            None,
            root.to_path_buf(),
        )
    }

    #[test]
    fn provider_knowledge_packs_are_skipped_when_platform_manifest_backend_is_present() {
        let temp = tempfile::tempdir().unwrap();
        write_library_pack(temp.path());

        let without_platform = platform_tools(temp.path(), false);
        assert_eq!(
            provider_knowledge_packs(temp.path(), &without_platform).len(),
            1
        );

        let with_platform = platform_tools(temp.path(), true);
        assert!(
            provider_knowledge_packs(temp.path(), &with_platform).is_empty(),
            "platform manifest backend owns knowledge tools for platform workers"
        );
    }
}
