use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use nenjo::Manifest;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::manifest::{HasManifestSlug, MediaRequirement};
use nenjo::memory::MarkdownMemory;
use nenjo::{ManifestLoader, Provider};
use nenjo_crypto_auth::EnrollmentBackedKeyProvider;
use nenjo_harness::Harness;
use nenjo_platform::PlatformManifestClient;
use nenjo_platform::api_client::PayloadCodec;
use nenjo_secure_envelope::{SecureEnvelopeCodec, SecureEnvelopeCodecConfig};
use tracing::warn;
use uuid::Uuid;

use crate::api_client::ApiClient;
use crate::bootstrap::{BootstrapAuth, load_cached_bootstrap_auth, load_cached_media_providers};
use crate::config::Config;
use crate::crypto::WorkerAuthProvider;
use crate::external_mcp::ExternalMcpPool;
use crate::media::MediaProviderResolver;
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

/// Fully assembled worker dependencies around the execution harness.
pub struct WorkerAssembly {
    pub harness: WorkerHarness,
    pub api: ApiClient,
    pub auth_provider: Arc<WorkerAuthProvider>,
    pub session_runtime: WorkerSessionRuntime,
    pub session_stores: WorkerSessionStores,
    pub external_mcp: Arc<ExternalMcpPool>,
    pub skill_registry: Arc<SkillRegistry>,
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

        let loader = LocalManifestStore::new(&config.manifests_dir);
        let provider = build_provider(
            config,
            loader,
            auth_provider.clone(),
            external_mcp.clone(),
            skill_registry.clone(),
        )
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

        Ok(Self {
            harness,
            api,
            auth_provider,
            session_runtime,
            session_stores,
            external_mcp,
            skill_registry,
        })
    }
}

pub(crate) async fn build_provider(
    config: &Config,
    loader: LocalManifestStore,
    auth_provider: Arc<WorkerAuthProvider>,
    external_mcp: Arc<ExternalMcpPool>,
    skill_registry: Arc<SkillRegistry>,
) -> Result<WorkerProvider> {
    let provider_registry = Arc::new(ModelProviderRegistry::new(
        &config.model_provider_api_keys,
        &config.reliability,
    ));
    let mut security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    extend_runtime_roots(
        &mut security.allowed_runtime_roots,
        package_runtime_roots(config),
    );
    let platform_tools = build_platform_tool_services(config, auth_provider);
    let effective_config = config_with_cached_media_providers(config);
    let tool_factory = WorkerToolFactory::with_skill_registry_and_provider_registry(
        security,
        NativeRuntime,
        effective_config.clone(),
        provider_registry.clone(),
        platform_tools,
        external_mcp.clone(),
        skill_registry,
    );

    let memory_dir = config.state_dir.join("memory");
    let mem = MarkdownMemory::new(&memory_dir, &config.state_dir);
    let live_manifest_reader = loader.clone();

    let provider = Provider::builder()
        .with_loader(global_package_manifest_loader(config))
        .with_loader(platform_package_manifest_loader(config))
        .with_loader(loader)
        .with_loader(workspace_package_manifest_loader(config))
        .with_model_factory(provider_registry.clone())
        .with_tool_factory(tool_factory)
        .with_memory(mem)
        .with_agent_config(config.agent.clone())
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
    let resolver = MediaProviderResolver::new(config.media_providers.clone(), capability_source);

    for agent in &manifest.agents {
        validate_media_requirements(
            &resolver,
            "agent",
            &agent.manifest_slug(),
            agent.media.as_slice(),
        )?;
    }
    for domain in &manifest.domains {
        validate_media_requirements(
            &resolver,
            "domain",
            &domain.manifest_slug(),
            domain.media.as_slice(),
        )?;
    }
    for ability in &manifest.abilities {
        validate_media_requirements(
            &resolver,
            "ability",
            &ability.manifest_slug(),
            ability.media.as_slice(),
        )?;
    }

    Ok(())
}

fn validate_media_requirements(
    resolver: &MediaProviderResolver,
    resource_kind: &str,
    resource_slug: &nenjo::Slug,
    requirements: &[MediaRequirement],
) -> Result<()> {
    for requirement in requirements {
        resolver.resolve(requirement).with_context(|| {
            format!(
                "media requirement {:?} for {resource_kind} '{}' is not resolvable",
                requirement.capability(),
                resource_slug
            )
        })?;
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
        config.config_dir.join("library"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MediaProviderConfig;
    use nenjo::Slug;
    use nenjo::agents::prompts::PromptConfig;
    use nenjo::manifest::{AgentManifest, MediaRequirement};
    use nenjo_models::NativeOperation;

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
                    capabilities: vec![NativeOperation::GenerateImage],
                },
                MediaProviderConfig {
                    slug: Slug::derive("xai_video"),
                    provider: "xai".to_string(),
                    model: "grok-imagine-video".to_string(),
                    capabilities: vec![NativeOperation::ReferenceToVideo],
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
                capabilities: vec![NativeOperation::GenerateImage],
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
    fn runtime_media_validation_rejects_unresolved_agent_requirement() {
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
                media: vec![MediaRequirement::Capability(NativeOperation::GenerateImage)],
                abilities: vec![],
                prompt_locked: false,
                heartbeat: None,
            }],
            ..Default::default()
        };

        let config = Config::default();
        let provider_registry =
            ModelProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);
        let error = validate_runtime_media_requirements(&config, &provider_registry, &manifest)
            .expect_err("missing media provider should fail validation");

        assert!(
            error
                .to_string()
                .contains("media requirement GenerateImage for agent 'image-tester'"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn runtime_media_validation_accepts_configured_agent_requirement() {
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
                media: vec![MediaRequirement::Capability(NativeOperation::GenerateImage)],
                abilities: vec![],
                prompt_locked: false,
                heartbeat: None,
            }],
            ..Default::default()
        };
        let config = Config {
            media_providers: vec![MediaProviderConfig {
                slug: Slug::derive("openai_image"),
                provider: "openai".to_string(),
                model: "gpt-image-1".to_string(),
                capabilities: vec![NativeOperation::GenerateImage],
            }],
            ..Default::default()
        };
        let provider_registry =
            ModelProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);

        validate_runtime_media_requirements(&config, &provider_registry, &manifest)
            .expect("configured media provider should satisfy requirement");
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
}
