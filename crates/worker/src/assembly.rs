use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use nenjo::Manifest;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::memory::MarkdownMemory;
use nenjo::{ManifestLoader, Provider};
use nenjo_crypto_auth::EnrollmentBackedKeyProvider;
use nenjo_harness::Harness;
use nenjo_knowledge::tools::KnowledgePackEntry;
use nenjo_knowledge::{KnowledgePack, PackageKnowledgePack};
use nenjo_nenpm::{
    NenpmLock, PackageInstallIndex, PackageSource, package_install_path_in_packages_dir,
};
use nenjo_packages::PackageKind;
use nenjo_platform::PlatformManifestClient;
use nenjo_platform::api_client::PayloadCodec;
use nenjo_platform::library_knowledge::LibraryKnowledgePack;
use nenjo_secure_envelope::SecureEnvelopeCodec;
use tracing::warn;
use uuid::Uuid;

use crate::api_client::ApiClient;
use crate::bootstrap::{BootstrapAuth, load_cached_bootstrap_auth};
use crate::config::Config;
use crate::crypto::WorkerAuthProvider;
use crate::external_mcp::ExternalMcpPool;
use crate::package_manifests::PackageManifestLoader;
use crate::providers::registry::ModelProviderRegistry;
use crate::sessions::{WorkerSessionRuntime, WorkerSessionStores};
use crate::skills::SkillRegistry;
use crate::tools::platform_payload::PlatformPayloadEncoder;
use crate::tools::platform_services::PlatformToolServices;
use crate::tools::{NativeRuntime, SecurityPolicy, WorkerToolFactory};

pub type WorkerProvider =
    Provider<ModelProviderRegistry, WorkerToolFactory<NativeRuntime>, MarkdownMemory>;

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
    let provider_registry =
        ModelProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);
    let mut security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    extend_runtime_roots(
        &mut security.allowed_runtime_roots,
        package_runtime_roots(config),
    );
    let platform_tools = build_platform_tool_services(config, auth_provider);
    let knowledge_packs = load_provider_knowledge_packs(&config.config_dir);
    let tool_factory = WorkerToolFactory::with_skill_registry(
        security,
        NativeRuntime,
        config.clone(),
        platform_tools,
        external_mcp.clone(),
        skill_registry,
    );

    let memory_dir = config.state_dir.join("memory");
    let mem = MarkdownMemory::new(&memory_dir, &config.state_dir);

    Provider::builder()
        .with_loader(global_package_manifest_loader(config))
        .with_loader(platform_package_manifest_loader(config))
        .with_loader(loader)
        .with_loader(workspace_package_manifest_loader(config))
        .with_model_factory(provider_registry)
        .with_tool_factory(tool_factory)
        .with_memory(mem)
        .with_agent_config(config.agent.clone())
        .with_knowledge_packs(knowledge_packs)
        .build()
        .await
        .context("Failed to build Provider")
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

fn extend_runtime_roots(target: &mut Vec<PathBuf>, roots: Vec<PathBuf>) {
    for root in roots {
        if !target.iter().any(|existing| existing == &root) {
            target.push(root);
        }
    }
}

fn load_library_knowledge_packs(nenjo_home: &Path) -> Vec<KnowledgePackEntry> {
    let mut packs = Vec::new();
    let library_dir = nenjo_home.join("library");
    if let Ok(entries) = std::fs::read_dir(&library_dir) {
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
    packs
}

fn load_package_knowledge_packs(nenjo_home: &Path) -> Vec<KnowledgePackEntry> {
    let packages_dir = nenjo_home.join("platform_pkgs");
    let lock_path = packages_dir.join("nenpm.lock.yml");
    let Ok(lock) = NenpmLock::load_file(&lock_path) else {
        return Vec::new();
    };
    let index = PackageInstallIndex::load_file(packages_dir.join(".nenpm-index.json")).ok();
    let mut packs = Vec::new();

    for package in lock.packages {
        let package_root = index
            .as_ref()
            .and_then(|index| index.get_package(&package.name, &package.version))
            .map(|entry| package_root_from_platform_index(nenjo_home, &packages_dir, &entry.root))
            .unwrap_or_else(|| {
                package_install_path_in_packages_dir(&packages_dir, &package.name, &package.version)
            });

        for module in package.modules {
            if module.kind != PackageKind::Knowledge {
                continue;
            }
            let manifest_path = package_root.join(&module.path);
            match PackageKnowledgePack::load(&manifest_path, package.version.as_str()) {
                Ok(pack) => {
                    let selector = package_knowledge_selector_name(
                        &package.name,
                        package.source.as_ref(),
                        pack.manifest().pack_id(),
                    );
                    match KnowledgePackEntry::package(selector, pack) {
                        Ok(entry) => packs.push(entry),
                        Err(error) => warn!(
                            package = %package.name,
                            error = %error,
                            "Skipping package knowledge pack with invalid package selector"
                        ),
                    }
                }
                Err(error) => warn!(
                    package = %package.name,
                    module = %module.path,
                    error = %error,
                    "Skipping package knowledge pack"
                ),
            }
        }
    }

    packs
}

fn package_knowledge_selector_name(
    package_name: &str,
    source: Option<&PackageSource>,
    pack_id: &str,
) -> String {
    let mut segments = package_source_selector_segments(source).unwrap_or_else(|| {
        package_name_scope_segment(package_name)
            .into_iter()
            .collect()
    });
    segments.push(package_leaf_segment(package_name));
    segments.push(knowledge_pack_leaf_segment(pack_id));
    segments
        .into_iter()
        .filter(|segment| !segment.trim().is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

fn package_source_selector_segments(source: Option<&PackageSource>) -> Option<Vec<String>> {
    let PackageSource::Git { url, .. } = source? else {
        return None;
    };
    github_owner_repo_from_url(url).map(|(owner, repo)| vec![owner, repo])
}

fn github_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim().trim_end_matches(".git").trim_end_matches('/');
    let path = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("git@github.com:"))?;
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn package_name_scope_segment(package_name: &str) -> Option<String> {
    package_name
        .trim()
        .trim_start_matches('@')
        .split_once('/')
        .map(|(scope, _)| scope.to_string())
        .filter(|scope| !scope.trim().is_empty())
}

fn package_leaf_segment(package_name: &str) -> String {
    package_name
        .trim()
        .trim_start_matches('@')
        .rsplit(['/', '.'])
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(package_name.trim())
        .to_string()
}

fn knowledge_pack_leaf_segment(pack_id: &str) -> String {
    pack_id
        .trim()
        .rsplit(['.', '/'])
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("knowledge")
        .to_string()
}

fn package_root_from_platform_index(
    _nenjo_home: &Path,
    packages_dir: &Path,
    indexed_root: &str,
) -> PathBuf {
    let indexed = Path::new(indexed_root);
    if indexed.is_absolute() {
        indexed.to_path_buf()
    } else if let Ok(relative_to_platform_pkgs) = indexed.strip_prefix("platform_pkgs") {
        packages_dir.join(relative_to_platform_pkgs)
    } else {
        packages_dir.join(indexed)
    }
}

fn load_provider_knowledge_packs(nenjo_home: &Path) -> Vec<KnowledgePackEntry> {
    let mut packs = load_library_knowledge_packs(nenjo_home);
    packs.extend(load_package_knowledge_packs(nenjo_home));
    packs
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

    fn write_library_pack(root: &Path) {
        let pack_dir = root.join("library").join("demo");
        let docs_dir = pack_dir.join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("intro.md"), "# Intro").unwrap();
        std::fs::write(
            pack_dir.join(LibraryKnowledgePack::MANIFEST_FILENAME),
            r#"{
              "pack_id": "demo",
              "version": "1",
              "schema_version": 1,
              "root_uri": "library://demo/",
              "content_hash": "",
              "synced_at": "",
              "docs": [
                {
                  "id": "intro",
                  "selector": "library://demo/intro.md",
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

    #[test]
    fn provider_knowledge_packs_include_local_cached_library_and_package_packs() {
        let temp = tempfile::tempdir().unwrap();
        write_library_pack(temp.path());

        assert_eq!(load_provider_knowledge_packs(temp.path()).len(), 1);
    }

    #[test]
    fn package_knowledge_packs_load_from_platform_install() {
        let temp = tempfile::tempdir().unwrap();
        let packages_dir = temp.path().join("platform_pkgs");
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

        let packs = load_package_knowledge_packs(temp.path());

        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].selector(), "pkg:nenjo-ai.packages.knowledge.core");
        assert!(
            packs[0]
                .pack()
                .read_doc("domain.nenjo")
                .unwrap()
                .content
                .contains("Knowledge content")
        );
        let vars = nenjo_knowledge::tools::knowledge_pack_prompt_vars(
            packs[0].knowledge_ref(),
            packs[0].pack().as_ref(),
        );
        assert!(vars.contains_key("pkg.nenjo_ai.packages.knowledge.core.domain.nenjo"));
    }

    #[test]
    fn package_knowledge_selector_uses_source_package_and_pack_leaf() {
        assert_eq!(
            package_knowledge_selector_name(
                "@nenjo-ai/knowledge",
                Some(&PackageSource::Git {
                    url: "https://github.com/nenjo-ai/packages.git".to_string(),
                    reference: "feat/v2".to_string(),
                    manifest_path: "packages.yaml".to_string(),
                }),
                "nenjo.core",
            ),
            "nenjo-ai.packages.knowledge.core"
        );
        assert_eq!(
            package_knowledge_selector_name("@acme/runbook", None, "acme.platform"),
            "acme.runbook.platform"
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
