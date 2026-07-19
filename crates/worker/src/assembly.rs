use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use nenjo::LocalRoutineExecutionWatcher;
use nenjo::Manifest;
use nenjo::arguments::{ArgumentValueType, ResolvedArgumentBinding};
use nenjo::manifest::HasManifestSlug;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::memory::MarkdownMemory;
use nenjo::{ManifestLoader, Provider};
use nenjo_crypto_auth::EnrollmentBackedKeyProvider;
use nenjo_events::PackageArgumentBindingUpdate;
use nenjo_harness::Harness;
use nenjo_platform::PlatformManifestClient;
use nenjo_platform::api_client::PayloadCodec;
use nenjo_platform::{PlatformResourceIdStore, PlatformResourceKind};
use nenjo_secure_envelope::{SecureEnvelopeCodec, SecureEnvelopeCodecConfig};
use tracing::warn;
use uuid::Uuid;

use crate::api_client::ApiClient;
use crate::bootstrap::{
    BootstrapAuth, ManifestRefreshHandle, ManifestSnapshotRefresher, WorkerManifestCache,
    WorkerManifestStore, load_cached_agent_model_assignments, load_cached_bootstrap_auth,
    load_cached_capability_defaults, load_cached_media_providers, load_cached_model_runtime,
};
use crate::config::Config;
use crate::crypto::WorkerAuthProvider;
use crate::external_mcp::ExternalMcpPool;
use crate::media::{MediaProviderResolver, ModelAssignmentResolver, ResourceRef};
use crate::package_manifests::PackageManifestLoader;
use crate::providers::registry::ModelProviderRegistry;
use crate::sessions::{WorkerSessionRuntime, WorkerSessionStores};
use crate::skills::SkillRegistry;
use crate::tools::platform_payload::PlatformPayloadEncoder;
use crate::tools::platform_services::PlatformToolServices;
use crate::tools::{NativeRuntime, SecurityPolicy, WorkerToolFactory};

pub type WorkerProvider =
    Provider<Arc<ModelProviderRegistry>, WorkerToolFactory<NativeRuntime>, MarkdownMemory>;

pub type WorkerHarness = Harness<WorkerProvider, WorkerSessionRuntime>;

/// Dependencies that are consumed together while constructing a provider.
pub(crate) struct ProviderBuildContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) local_manifest_loader: LocalManifestStore,
    pub(crate) auth_provider: Arc<WorkerAuthProvider>,
    pub(crate) external_mcp: Arc<ExternalMcpPool>,
    pub(crate) skill_registry: Arc<SkillRegistry>,
    pub(crate) provider_registry: Arc<ModelProviderRegistry>,
    pub(crate) manifest_cache: Arc<WorkerManifestCache>,
    pub(crate) manifest_refresh: ManifestRefreshHandle,
    pub(crate) local_execution_watcher: LocalRoutineExecutionWatcher,
}

/// Applies a platform mutation to the worker's canonical cache and live
/// harness before the originating platform tool returns.
struct WorkerManifestRefreshCoordinator {
    harness: WorkerHarness,
    config: Config,
    api: ApiClient,
    cache: Arc<WorkerManifestCache>,
    external_mcp: Arc<ExternalMcpPool>,
    skill_registry: Arc<SkillRegistry>,
    change_lock: Arc<tokio::sync::Mutex<()>>,
}

impl WorkerManifestRefreshCoordinator {
    async fn replace_provider_manifest(&self, manifest: Manifest) -> Result<()> {
        let argument_bindings = load_platform_package_argument_bindings(&self.config)
            .context("failed to reload package argument bindings after platform mutation")?;

        self.external_mcp.reconcile(&manifest.mcp_servers).await;
        self.skill_registry
            .reconcile(&manifest.skills, &manifest.hooks);
        self.harness
            .manifests()
            .replace_with_argument_bindings(manifest, argument_bindings)
            .await
            .context("failed to replace the running provider manifest after platform mutation")
    }
}

#[async_trait::async_trait]
impl ManifestSnapshotRefresher for WorkerManifestRefreshCoordinator {
    async fn refresh_provider_manifest(&self) -> Result<()> {
        let _change_guard = self.change_lock.lock().await;
        self.cache
            .refresh_after_platform_write(&self.api)
            .await
            .context("failed to refresh the canonical manifest cache after platform mutation")?;

        // The provider includes package overlays in addition to the platform
        // cache, so rebuilding only from manifests_dir would drop package
        // resources on every direct platform-tool mutation.
        let manifest = load_runtime_manifest(&self.config)
            .await
            .context("failed to rebuild the runtime manifest after platform mutation")?;
        self.replace_provider_manifest(manifest).await
    }
}

/// Fully assembled worker dependencies around the execution harness.
pub struct WorkerAssembly {
    pub harness: WorkerHarness,
    pub api: ApiClient,
    pub provider_registry: Arc<ModelProviderRegistry>,
    pub auth_provider: Arc<WorkerAuthProvider>,
    pub session_runtime: WorkerSessionRuntime,
    pub session_stores: WorkerSessionStores,
    pub external_mcp: Arc<ExternalMcpPool>,
    pub skill_registry: Arc<SkillRegistry>,
    pub manifest_cache: Arc<WorkerManifestCache>,
    pub manifest_change_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) local_execution_watcher: LocalRoutineExecutionWatcher,
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
        enrollment_api: ApiClient,
        codec_config: SecureEnvelopeCodecConfig,
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
            codec: Arc::new(SecureEnvelopeCodec::new_with_config(
                key_provider,
                auth.org_id,
                codec_config,
            )),
        })
    }

    pub fn configure_api_client(&self, client: ApiClient) -> ApiClient {
        let payload_codec: Arc<dyn PayloadCodec> = self.codec.clone();
        client.with_shared_payload_codec(payload_codec)
    }
}

impl WorkerAssembly {
    pub async fn from_bootstrapped(
        config: &Config,
        auth_provider: Arc<WorkerAuthProvider>,
        api: ApiClient,
    ) -> Result<Self> {
        let external_mcp = Arc::new(ExternalMcpPool::new());
        let skill_registry = Arc::new(SkillRegistry::default());
        let manifest_cache = Arc::new(WorkerManifestCache {
            manifests_dir: config.manifests_dir.clone(),
            workspace_dir: config.workspace_dir.clone(),
            state_dir: config.state_dir.clone(),
            config_dir: config.config_dir.clone(),
        });
        let manifest_change_lock = Arc::new(tokio::sync::Mutex::new(()));
        let manifest_refresh = ManifestRefreshHandle::default();
        let local_execution_watcher = LocalRoutineExecutionWatcher::default();

        let local_manifest_loader = LocalManifestStore::new(&config.manifests_dir);
        let provider_registry = Arc::new(ModelProviderRegistry::new(
            &config.model_provider_api_keys,
            &config.reliability,
        ));
        let provider = build_provider(ProviderBuildContext {
            config,
            local_manifest_loader,
            auth_provider: auth_provider.clone(),
            external_mcp: external_mcp.clone(),
            skill_registry: skill_registry.clone(),
            provider_registry: provider_registry.clone(),
            manifest_cache: manifest_cache.clone(),
            manifest_refresh: manifest_refresh.clone(),
            local_execution_watcher: local_execution_watcher.clone(),
        })
        .await?;
        let manifest = provider.manifest_snapshot();
        external_mcp.reconcile(&manifest.mcp_servers).await;
        skill_registry.reconcile(&manifest.skills, &manifest.hooks);

        let worker_name = config
            .harness_name
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| auth_provider.identity().worker_id.to_string());
        let session_stores = WorkerSessionStores::new(&config.state_dir);
        let session_runtime = WorkerSessionRuntime::with_host(session_stores.clone(), worker_name);

        let harness = nenjo_harness::Harness::builder(provider)
            .with_session_runtime(session_runtime.clone())
            .build();
        manifest_refresh.bind(Arc::new(WorkerManifestRefreshCoordinator {
            harness: harness.clone(),
            config: config.clone(),
            api: api.clone(),
            cache: manifest_cache.clone(),
            external_mcp: external_mcp.clone(),
            skill_registry: skill_registry.clone(),
            change_lock: manifest_change_lock.clone(),
        }))?;

        Ok(Self {
            harness,
            api,
            provider_registry,
            auth_provider,
            session_runtime,
            session_stores,
            external_mcp,
            skill_registry,
            manifest_cache,
            manifest_change_lock,
            local_execution_watcher,
        })
    }
}

pub(crate) async fn build_provider(
    ProviderBuildContext {
        config,
        local_manifest_loader,
        auth_provider,
        external_mcp,
        skill_registry,
        provider_registry,
        manifest_cache,
        manifest_refresh,
        local_execution_watcher,
    }: ProviderBuildContext<'_>,
) -> Result<WorkerProvider> {
    let mut security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    extend_runtime_roots(
        &mut security.allowed_runtime_roots,
        package_runtime_roots(config),
    );
    let platform_tools =
        build_platform_tool_services(config, auth_provider, manifest_cache, manifest_refresh).await;
    let effective_config = config_with_cached_media_providers(config);
    let tool_factory = WorkerToolFactory::with_skill_registry_and_provider_registry(
        security,
        NativeRuntime,
        effective_config.clone(),
        provider_registry.clone(),
        platform_tools,
        external_mcp.clone(),
        skill_registry,
    )
    .with_local_execution_watcher(local_execution_watcher);

    let memory_dir = config.state_dir.join("memory");
    let mem = MarkdownMemory::new(&memory_dir, &config.state_dir);
    let live_manifest_reader = local_manifest_loader.clone();
    let argument_bindings = load_platform_package_argument_bindings(config)?;

    let provider = Provider::builder()
        .with_loader(global_package_manifest_loader(config))
        .with_loader(platform_package_manifest_loader(config))
        .with_loader(local_manifest_loader)
        .with_loader(workspace_package_manifest_loader(config))
        .with_model_factory(provider_registry.clone())
        .with_tool_factory(tool_factory)
        .with_memory(mem)
        .with_agent_config(config.agent.clone())
        .with_argument_bindings(argument_bindings)
        .with_live_manifest_reader(live_manifest_reader)
        .build()
        .await
        .context("Failed to build Provider")?;

    validate_runtime_media_requirements(
        &effective_config,
        provider_registry.as_ref(),
        &provider.manifest_snapshot(),
    )?;

    Ok(provider)
}

pub(crate) async fn load_runtime_manifest(config: &Config) -> Result<Manifest> {
    let loader = LocalManifestStore::new(&config.manifests_dir);
    let mut manifest = global_package_manifest_loader(config).load().await?;
    manifest.merge(platform_package_manifest_loader(config).load().await?);
    manifest.merge(ManifestLoader::load(&loader).await?);
    manifest.merge(workspace_package_manifest_loader(config).load().await?);
    Ok(manifest)
}

pub(crate) fn load_platform_package_argument_bindings(
    config: &Config,
) -> Result<Vec<ResolvedArgumentBinding>> {
    load_platform_package_argument_bindings_from(&config.config_dir.join("platform_pkgs"))
}

fn load_platform_package_argument_bindings_from(
    root: &Path,
) -> Result<Vec<ResolvedArgumentBinding>> {
    let path = root.join("argument_bindings.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let bindings: Vec<PackageArgumentBindingUpdate> = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    bindings
        .into_iter()
        .map(resolved_platform_package_argument_binding)
        .collect()
}

fn resolved_platform_package_argument_binding(
    binding: PackageArgumentBindingUpdate,
) -> Result<ResolvedArgumentBinding> {
    let value_type = match binding.value_type.as_str() {
        "text" => ArgumentValueType::Text,
        "markdown" => ArgumentValueType::Markdown,
        "xml" => ArgumentValueType::Xml,
        "json" => ArgumentValueType::Json,
        other => bail!("unsupported package argument type '{other}'"),
    };
    ResolvedArgumentBinding::new(
        binding.package,
        binding.name,
        binding.selector,
        value_type,
        binding.value,
    )
    .map_err(Into::into)
}

pub(crate) async fn load_package_overlay_manifest(config: &Config) -> Result<Manifest> {
    let mut manifest = global_package_manifest_loader(config).load().await?;
    manifest.merge(platform_package_manifest_loader(config).load().await?);
    manifest.merge(workspace_package_manifest_loader(config).load().await?);
    Ok(manifest)
}

fn global_package_manifest_loader(config: &Config) -> PackageManifestLoader {
    PackageManifestLoader::with_packages_dir(
        config.config_dir.clone(),
        config.config_dir.join("packages"),
    )
}

fn platform_package_manifest_loader(config: &Config) -> PackageManifestLoader {
    let root = config.config_dir.join("platform_pkgs");
    PackageManifestLoader::with_packages_dir(root.clone(), root)
}

fn workspace_package_manifest_loader(config: &Config) -> PackageManifestLoader {
    PackageManifestLoader::new(config.workspace_dir.clone())
}

fn package_runtime_roots(config: &Config) -> Vec<PathBuf> {
    vec![
        config.config_dir.join("packages"),
        config.config_dir.join("platform_pkgs"),
        config.config_dir.join("skills"),
        config.config_dir.join("plugins"),
        config.workspace_dir.join(".nenjo").join("packages"),
        config.workspace_dir.join(".nenjo").join("skills"),
        config.workspace_dir.join(".nenjo").join("plugins"),
    ]
}

fn config_with_cached_media_providers(config: &Config) -> Config {
    let cached = load_cached_media_providers(&config.manifests_dir);
    if cached.is_empty() {
        return config.clone();
    }

    let mut effective = config.clone();
    for provider in cached {
        if effective
            .media_providers
            .iter()
            .all(|existing| existing.slug != provider.slug)
        {
            effective.media_providers.push(provider);
        }
    }
    effective
}

fn validate_runtime_media_requirements(
    config: &Config,
    capability_source: &dyn crate::media::MediaCapabilitySource,
    manifest: &Manifest,
) -> Result<()> {
    let resource_ids = PlatformResourceIdStore::new(&config.manifests_dir)
        .load()
        .unwrap_or_default();
    let assignment_resolver = ModelAssignmentResolver::new(
        load_cached_model_runtime(&config.manifests_dir),
        load_cached_agent_model_assignments(&config.manifests_dir),
        load_cached_capability_defaults(&config.manifests_dir),
    );
    let media_resolver =
        MediaProviderResolver::new(config.media_providers.clone(), capability_source);

    for agent in &manifest.agents {
        let slug = agent.manifest_slug();
        let resource_id = resource_ids.get(PlatformResourceKind::Agent, &slug);
        let resource = ResourceRef {
            resource_type: "agent",
            resource_id,
            resource_slug: Some(slug.as_str()),
        };
        // Agents: model_assignments only (local → package → org default).
        for capability in assignment_resolver.assigned_capabilities(resource) {
            assignment_resolver
                .resolve(resource, capability)
                .with_context(|| {
                    format!("model assignment {capability:?} for agent '{slug}' is not resolvable")
                })?;
        }
    }
    for domain in &manifest.domains {
        let slug = domain.manifest_slug();
        for requirement in &domain.media {
            media_resolver.resolve(requirement).with_context(|| {
                format!(
                    "media requirement {:?} for domain '{slug}' is not resolvable",
                    requirement.capability()
                )
            })?;
        }
    }
    for ability in &manifest.abilities {
        let slug = ability.manifest_slug();
        for requirement in &ability.media {
            media_resolver.resolve(requirement).with_context(|| {
                format!(
                    "media requirement {:?} for ability '{slug}' is not resolvable",
                    requirement.capability()
                )
            })?;
        }
    }

    Ok(())
}

fn extend_runtime_roots(target: &mut Vec<PathBuf>, roots: Vec<PathBuf>) {
    for root in roots {
        if !target.iter().any(|existing| existing == &root) {
            target.push(root);
        }
    }
}

async fn build_platform_tool_services(
    config: &Config,
    auth_provider: Arc<WorkerAuthProvider>,
    manifest_cache: Arc<WorkerManifestCache>,
    manifest_refresh: ManifestRefreshHandle,
) -> PlatformToolServices {
    let manifest_store = Arc::new(WorkerManifestStore::new(manifest_cache, manifest_refresh));
    let read_only_manifest = match load_package_overlay_manifest(config).await {
        Ok(manifest) => Some(Arc::new(manifest)),
        Err(error) => {
            warn!(error = %error, "Failed to load read-only package manifest overlay for platform tools");
            None
        }
    };
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
        config.config_dir.join("library"),
        read_only_manifest,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MediaProviderConfig;
    use nenjo::agents::prompts::PromptConfig;
    use nenjo::manifest::{AgentManifest, MediaRequirement};
    use nenjo::{ManifestWriter, Slug};
    use nenjo_models::MediaOperation;

    #[test]
    fn runtime_roots_keep_workspace_packages_skills_and_plugins_without_library() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config::new_for_dir(temp.path().join(".nenjo"));
        let roots = package_runtime_roots(&config);
        let workspace_runtime = config.workspace_dir.join(".nenjo");

        assert!(roots.contains(&workspace_runtime.join("packages")));
        assert!(roots.contains(&workspace_runtime.join("skills")));
        assert!(roots.contains(&workspace_runtime.join("plugins")));
        assert!(!roots.contains(&workspace_runtime.join("library")));
        assert!(roots.contains(&config.config_dir.join("packages")));
        assert!(roots.contains(&config.config_dir.join("platform_pkgs")));
    }

    #[tokio::test]
    async fn platform_write_refresh_replaces_the_live_provider_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            config_dir: temp.path().join("config"),
            workspace_dir: temp.path().join("workspace"),
            state_dir: temp.path().join("state"),
            manifests_dir: temp.path().join("manifests"),
            backend_api_url: Some("http://127.0.0.1:9".to_string()),
            api_key: "test-api-key".to_string(),
            ..Default::default()
        };
        let auth_provider =
            Arc::new(WorkerAuthProvider::load_or_create(temp.path().join("crypto")).unwrap());
        let external_mcp = Arc::new(ExternalMcpPool::new());
        let skill_registry = Arc::new(SkillRegistry::default());
        let provider_registry = Arc::new(ModelProviderRegistry::new(
            &config.model_provider_api_keys,
            &config.reliability,
        ));
        let cache = Arc::new(WorkerManifestCache {
            manifests_dir: config.manifests_dir.clone(),
            workspace_dir: config.workspace_dir.clone(),
            state_dir: config.state_dir.clone(),
            config_dir: config.config_dir.clone(),
        });
        let provider = build_provider(ProviderBuildContext {
            config: &config,
            local_manifest_loader: LocalManifestStore::new(&config.manifests_dir),
            auth_provider,
            external_mcp: external_mcp.clone(),
            skill_registry: skill_registry.clone(),
            provider_registry,
            manifest_cache: cache.clone(),
            manifest_refresh: ManifestRefreshHandle::default(),
            local_execution_watcher: LocalRoutineExecutionWatcher::default(),
        })
        .await
        .unwrap();
        let session_runtime = WorkerSessionRuntime::with_host(
            WorkerSessionStores::new(&config.state_dir),
            "manifest-refresh-test",
        );
        let harness = Harness::builder(provider)
            .with_session_runtime(session_runtime)
            .build();
        let coordinator = WorkerManifestRefreshCoordinator {
            harness: harness.clone(),
            config,
            api: ApiClient::new("http://127.0.0.1:9", "test-api-key"),
            cache,
            external_mcp,
            skill_registry,
            change_lock: Arc::new(tokio::sync::Mutex::new(())),
        };

        coordinator
            .replace_provider_manifest(Manifest {
                projects: vec![nenjo::manifest::ProjectManifest {
                    name: "Fresh project".into(),
                    slug: Slug::derive("fresh-project"),
                    description: None,
                    settings: serde_json::json!({}),
                }],
                ..Default::default()
            })
            .await
            .unwrap();

        let snapshot = harness.provider().manifest_snapshot();
        assert_eq!(snapshot.projects[0].slug, Slug::derive("fresh-project"));
    }

    #[test]
    fn effective_config_merges_cached_media_providers_without_replacing_local_provider() {
        let temp = tempfile::tempdir().unwrap();
        let manifests_dir = temp.path().join("manifests");
        std::fs::create_dir_all(&manifests_dir).unwrap();
        std::fs::write(
            manifests_dir.join("media_providers.json"),
            serde_json::to_string(&vec![
                MediaProviderConfig {
                    slug: Slug::derive("local_image"),
                    provider: "xai".to_string(),
                    model: "grok-imagine-image-quality".to_string(),
                    capabilities: vec![MediaOperation::GenerateImage],
                },
                MediaProviderConfig {
                    slug: Slug::derive("xai_video"),
                    provider: "xai".to_string(),
                    model: "grok-imagine-video".to_string(),
                    capabilities: vec![MediaOperation::ReferenceToVideo],
                },
            ])
            .unwrap(),
        )
        .unwrap();
        let config = Config {
            manifests_dir,
            media_providers: vec![MediaProviderConfig {
                slug: Slug::derive("local_image"),
                provider: "openai".to_string(),
                model: "gpt-image-1".to_string(),
                capabilities: vec![MediaOperation::GenerateImage],
            }],
            ..Default::default()
        };

        let effective = config_with_cached_media_providers(&config);

        assert_eq!(effective.media_providers.len(), 2);
        let local = effective
            .media_providers
            .iter()
            .find(|provider| provider.slug == Slug::derive("local_image"))
            .expect("local provider");
        assert_eq!(local.provider, "openai");
        assert!(
            effective
                .media_providers
                .iter()
                .any(|provider| provider.slug == Slug::derive("xai_video"))
        );
    }

    #[test]
    fn loads_platform_package_argument_bindings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("platform_pkgs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("argument_bindings.json"),
            serde_json::to_string(&vec![PackageArgumentBindingUpdate {
                package: "@acme/app".to_string(),
                name: "shop_hours".to_string(),
                selector: "args.shop.hours".to_string(),
                value_type: "markdown".to_string(),
                value: "Mon-Fri 9-5".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

        let bindings = load_platform_package_argument_bindings_from(&root).unwrap();

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].package, "@acme/app");
        assert_eq!(bindings[0].selector.as_str(), "args.shop.hours");
        assert_eq!(bindings[0].render_value().unwrap(), "Mon-Fri 9-5");
    }

    #[test]
    fn runtime_media_validation_ignores_agent_legacy_media_without_assignments() {
        // Agents use model_assignments only; leftover agent.media rows do not fail validation.
        let manifest = Manifest {
            agents: vec![AgentManifest {
                name: "image tester".into(),
                slug: Slug::derive("image-tester"),
                description: None,
                prompt_config: PromptConfig::default(),
                color: None,
                model: None,
                domains: vec![],
                platform_scopes: vec![],
                mcp_servers: vec![],
                script_tools: Vec::new(),
                media: vec![MediaRequirement::Capability(MediaOperation::GenerateImage)],
                abilities: vec![],
                prompt_locked: false,
                source_type: None,
                metadata: serde_json::json!({}),
            }],
            ..Default::default()
        };

        let config = Config::default();
        let provider_registry =
            ModelProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);
        validate_runtime_media_requirements(&config, &provider_registry, &manifest)
            .expect("agent without model_assignments should not fail on legacy media");
    }

    #[test]
    fn runtime_media_validation_accepts_domain_media_with_provider() {
        use nenjo::manifest::{DomainManifest, DomainPromptConfig};

        let manifest = Manifest {
            domains: vec![DomainManifest {
                name: "creative".to_string(),
                path: "domains".to_string(),
                description: None,
                command: "#creative".to_string(),
                platform_scopes: Vec::new(),
                abilities: Vec::new(),
                mcp_servers: Vec::new(),
                script_tools: Vec::new(),
                media: vec![MediaRequirement::Capability(MediaOperation::GenerateImage)],
                prompt_config: DomainPromptConfig::default(),
            }],
            ..Default::default()
        };
        let config = Config {
            media_providers: vec![MediaProviderConfig {
                slug: Slug::derive("openai_image"),
                provider: "openai".to_string(),
                model: "gpt-image-1".to_string(),
                capabilities: vec![MediaOperation::GenerateImage],
            }],
            ..Default::default()
        };
        let provider_registry =
            ModelProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);

        validate_runtime_media_requirements(&config, &provider_registry, &manifest)
            .expect("configured media provider should satisfy domain requirement");
    }

    #[tokio::test]
    async fn runtime_manifest_loads_platform_package_knowledge_packs() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            config_dir: temp.path().to_path_buf(),
            ..Default::default()
        };
        let packages_dir = config.config_dir.join("platform_pkgs");
        let package_root = packages_dir.join("@nenjo-ai/knowledge@0.1.0");
        std::fs::create_dir_all(package_root.join("core/docs/domain")).unwrap();
        std::fs::write(
            packages_dir.join(".nenpm-index.json"),
            r#"{
              "schema": "nenjo.package-index.v1",
              "packages": {
                "@nenjo-ai/knowledge@0.1.0": {
                  "name": "@nenjo-ai/knowledge",
                  "version": "0.1.0",
                  "root": "@nenjo-ai/knowledge@0.1.0",
                  "manifest_path": "package.yaml"
                }
              }
            }"#,
        )
        .unwrap();
        std::fs::write(
            packages_dir.join("nenpm.lock.yml"),
            r#"
schema: nenjo.lock.v1
packages:
- name: '@nenjo-ai/knowledge'
  version: 0.1.0
  manifest_path: nenjo/knowledge/package.yaml
  hash: sha256:test
  source:
    kind: git
    url: https://github.com/nenjo-ai/packages.git
    reference: test
    manifest_path: nenjo/knowledge/package.yaml
  dependencies: {}
  modules:
  - path: core/manifest.yaml
    resource: Nenjo Core
    source_path: nenjo/knowledge/core/manifest.yaml
    schema: nenjo.knowledge.v1
    kind: knowledge
    name: Nenjo Core
    hash: sha256:test
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("core/manifest.yaml"),
            r#"
schema: nenjo.knowledge.v1
selector: ignored.authored.selector
root_uri: pkg://nenjo/knowledge/
manifest:
  name: Nenjo Core
  pack_id: nenjo.core
  version: 0.1.0
  schema_version: 1
  docs:
    - id: nenjo.domain.nenjo
      source_path: docs/domain/nenjo.md
      title: Nenjo
      summary: Platform overview.
      kind: domain
      tags: [domain:nenjo]
      related: []
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("core/docs/domain/nenjo.md"),
            "# Nenjo\n\nKnowledge content.",
        )
        .unwrap();
        std::fs::write(
            package_root.join("package.yaml"),
            r#"
schema: nenjo.package.v1
name: "@nenjo-ai/knowledge"
version: "0.1.0"
modules:
  - core/manifest.yaml
"#,
        )
        .unwrap();

        let manifest = platform_package_manifest_loader(&config)
            .load()
            .await
            .unwrap();

        assert_eq!(manifest.knowledge_packs.len(), 1);
        let pack = &manifest.knowledge_packs[0];
        assert_eq!(pack.selector, "pkg:nenjo_ai.packages.knowledge.core");
        assert_eq!(pack.root_uri, "pkg://nenjo/knowledge/");
        assert_eq!(
            pack.root_path.as_ref().unwrap(),
            &package_root.join("core/manifest.yaml")
        );
    }

    #[tokio::test]
    async fn package_overlay_manifest_does_not_include_local_cache() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            config_dir: temp.path().join("config"),
            workspace_dir: temp.path().join("workspace"),
            state_dir: temp.path().join("state"),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };
        let store = LocalManifestStore::new(&config.manifests_dir);
        store
            .replace_manifest(&Manifest {
                agents: vec![AgentManifest {
                    name: "cached agent".into(),
                    slug: Slug::derive("cached-agent"),
                    description: None,
                    prompt_config: PromptConfig::default(),
                    color: None,
                    model: None,
                    domains: vec![],
                    platform_scopes: vec![],
                    mcp_servers: vec![],
                    script_tools: Vec::new(),
                    media: Vec::new(),
                    abilities: vec![],
                    prompt_locked: false,
                    source_type: None,
                    metadata: serde_json::json!({}),
                }],
                ..Default::default()
            })
            .await
            .unwrap();

        let runtime = load_runtime_manifest(&config).await.unwrap();
        assert_eq!(runtime.agents.len(), 1);

        let overlay = load_package_overlay_manifest(&config).await.unwrap();
        assert!(overlay.agents.is_empty());
    }

    #[tokio::test]
    async fn runtime_manifest_loads_platform_installed_skills() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        let packages_dir = config_dir.join("platform_pkgs");
        let package_root = packages_dir.join("skills@0.1.0");
        std::fs::create_dir_all(package_root.join("skills/review")).unwrap();
        std::fs::write(
            packages_dir.join(".nenpm-index.json"),
            r#"{
              "schema": "nenjo.package-index.v1",
              "packages": {
                "skills@0.1.0": {
                  "name": "skills",
                  "version": "0.1.0",
                  "root": "skills@0.1.0",
                  "manifest_path": "package.yaml"
                }
              }
            }"#,
        )
        .unwrap();
        std::fs::write(
            packages_dir.join("nenpm.lock.yml"),
            r#"
schema: nenjo.lock.v1
packages:
- name: skills
  version: 0.1.0
  manifest_path: package.yaml
  hash: sha256:test
  modules:
  - path: skills/review
    resource: review
    source_path: skills/review/SKILL.md
    schema: nenjo.skill.v1
    kind: skill
    name: review
    hash: sha256:test
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("package.yaml"),
            r#"
schema: nenjo.package.v1
name: skills
version: "0.1.0"
modules:
  - skills/review/
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("skills/review/SKILL.md"),
            r#"---
name: review
description: Review code changes.
---

# Review
"#,
        )
        .unwrap();

        let config = Config {
            config_dir,
            workspace_dir: temp.path().join("workspace"),
            state_dir: temp.path().join("state"),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };

        let manifest = load_runtime_manifest(&config).await.unwrap();

        assert_eq!(manifest.skills.len(), 1);
        assert_eq!(manifest.skills[0].name, "review");
        assert_eq!(
            manifest.skills[0].root_dir,
            package_root.join("skills/review")
        );
    }

    #[tokio::test]
    async fn runtime_manifest_loads_nested_platform_package_commands() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        let packages_dir = config_dir.join("platform_pkgs");
        let package_root = packages_dir.join("@nenjo-ai").join("nenji@1.0.0");
        std::fs::create_dir_all(package_root.join("nenjo/nenji/commands/design")).unwrap();
        std::fs::write(
            packages_dir.join("nenpm.lock.yml"),
            r#"
schema: nenjo.lock.v1
packages:
- name: "@nenjo-ai/nenji"
  version: "1.0.0"
  manifest_path: nenjo/nenji/package.yaml
  hash: sha256:test
  modules:
  - path: commands/design.yaml
    resource: design
    source_path: nenjo/nenji/commands/design.yaml
    schema: nenjo.command.v1
    kind: command
    name: design
    hash: sha256:test
"#,
        )
        .unwrap();
        std::fs::write(
            packages_dir.join(".nenpm-index.json"),
            r#"{
              "schema": "nenjo.package-index.v1",
              "packages": {
                "@nenjo-ai/nenji@1.0.0": {
                  "name": "@nenjo-ai/nenji",
                  "version": "1.0.0",
                  "root": "@nenjo-ai/nenji@1.0.0",
                  "manifest_path": "package.yaml"
                }
              }
            }"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("nenjo/nenji/package.yaml"),
            r#"
schema: nenjo.package.v1
name: "@nenjo-ai/nenji"
version: "1.0.0"
modules:
  - commands/design.yaml
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("nenjo/nenji/commands/design.yaml"),
            r#"
schema: nenjo.command.v1
manifest:
  name: design
  command: /design
  content_path: nenjo/nenji/commands/design/command.md
"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("nenjo/nenji/commands/design/command.md"),
            "Design the requested artifact.\n",
        )
        .unwrap();

        let config = Config {
            config_dir,
            workspace_dir: temp.path().join("workspace"),
            state_dir: temp.path().join("state"),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };

        let manifest = load_runtime_manifest(&config).await.unwrap();

        assert_eq!(manifest.commands.len(), 1);
        let command = &manifest.commands[0];
        assert_eq!(command.name, "design");
        assert_eq!(
            command.root_dir,
            package_root.join("nenjo/nenji/commands/design")
        );
        assert_eq!(command.entry_path, "command.md");
    }
}
