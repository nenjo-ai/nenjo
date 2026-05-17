//! Worker bootstrap manifest cache.
//!
//! Calls `GET /api/v1/agents/bootstrap` and writes the response as individual
//! JSON files under `~/.nenjo/manifests/`. If the backend is unreachable the worker
//! continues with a warning; filesystem failures are hard errors.
//!
//! Abilities and context blocks are stored as directory trees:
//!   `manifests/abilities/{path}/{name}.json`
//!   `manifests/context_blocks/{path}/{name}.json`
//! Other resource types remain as flat JSON arrays.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, error, info, warn};

use crate::crypto::WorkerAuthProvider;
use crate::crypto::decrypt_text_with_provider;
use nenjo::agents::prompts::PromptConfig;
use nenjo::client::{DocumentSyncMeta, NenjoClient};
use nenjo::manifest::{ContextBlockManifest, Manifest, ManifestLoader};
use nenjo_events::{Capability, EncryptedPayload, ResourceType};
use nenjo_harness::handlers::manifest::ManifestStore;
use nenjo_platform::ManifestKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

static CACHE_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
struct BootstrapManifestResponse {
    auth: BootstrapAuth,
    #[serde(default)]
    routines: Vec<nenjo::manifest::RoutineManifest>,
    #[serde(default)]
    models: Vec<nenjo::manifest::ModelManifest>,
    #[serde(default)]
    agents: Vec<BootstrapAgentManifest>,
    #[serde(default)]
    councils: Vec<nenjo::manifest::CouncilManifest>,
    #[serde(default)]
    domains: Vec<nenjo::manifest::DomainManifest>,
    #[serde(default)]
    projects: Vec<BootstrapProjectManifest>,
    #[serde(default)]
    mcp_servers: Vec<nenjo::manifest::McpServerManifest>,
    #[serde(default)]
    abilities: Vec<nenjo::manifest::AbilityManifest>,
    #[serde(default)]
    context_blocks: Vec<BootstrapContextBlockManifest>,
    #[serde(default)]
    nats: BootstrapNatsConfig,
}

struct HydratedBootstrap {
    auth: BootstrapAuth,
    manifest: Manifest,
    nats: BootstrapNatsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapAuth {
    pub user_id: Uuid,
    pub org_id: Uuid,
    #[serde(default)]
    pub api_key_id: Option<Uuid>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BootstrapNatsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub tls_required: bool,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub auth: BootstrapNatsAuth,
    #[serde(default)]
    pub stream: BootstrapNatsStream,
    #[serde(default)]
    pub reconnect: BootstrapNatsReconnect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapNatsAuth {
    #[serde(default = "default_nats_auth_method")]
    pub method: String,
}

impl Default for BootstrapNatsAuth {
    fn default() -> Self {
        Self {
            method: default_nats_auth_method(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapNatsStream {
    #[serde(default = "default_nats_stream_name")]
    pub name: String,
}

impl Default for BootstrapNatsStream {
    fn default() -> Self {
        Self {
            name: default_nats_stream_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapNatsReconnect {
    #[serde(default = "default_nats_max_reconnects")]
    pub max_reconnects: i32,
    #[serde(default = "default_nats_initial_delay_ms")]
    pub initial_delay_ms: u64,
    #[serde(default = "default_nats_max_delay_ms")]
    pub max_delay_ms: u64,
    #[serde(default = "default_nats_jitter_ms")]
    pub jitter_ms: u64,
}

impl Default for BootstrapNatsReconnect {
    fn default() -> Self {
        Self {
            max_reconnects: default_nats_max_reconnects(),
            initial_delay_ms: default_nats_initial_delay_ms(),
            max_delay_ms: default_nats_max_delay_ms(),
            jitter_ms: default_nats_jitter_ms(),
        }
    }
}

fn default_nats_auth_method() -> String {
    "api_key_token".to_string()
}

fn default_nats_stream_name() -> String {
    "AGENT_WORK_REQUESTS".to_string()
}

fn default_nats_max_reconnects() -> i32 {
    -1
}

fn default_nats_initial_delay_ms() -> u64 {
    250
}

fn default_nats_max_delay_ms() -> u64 {
    30_000
}

fn default_nats_jitter_ms() -> u64 {
    500
}

#[derive(Debug, Deserialize)]
struct BootstrapAuthEnvelope {
    auth: BootstrapAuth,
}

#[derive(Debug, Deserialize)]
struct BootstrapAgentManifest {
    id: Uuid,
    name: String,
    description: Option<String>,
    color: Option<String>,
    model_id: Option<Uuid>,
    #[serde(default, alias = "domain_ids")]
    domains: Vec<Uuid>,
    #[serde(default)]
    platform_scopes: Vec<String>,
    #[serde(default)]
    mcp_server_ids: Vec<Uuid>,
    #[serde(default, alias = "ability_ids")]
    abilities: Vec<Uuid>,
    #[serde(default)]
    prompt_locked: bool,
    #[serde(default)]
    heartbeat: Option<nenjo::manifest::AgentHeartbeatManifest>,
    #[serde(default)]
    encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Deserialize)]
struct BootstrapContextBlockManifest {
    id: Uuid,
    name: String,
    #[serde(default)]
    path: String,
    display_name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    template: String,
    #[serde(default)]
    encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Deserialize)]
struct BootstrapProjectManifest {
    id: Uuid,
    name: String,
    slug: String,
    description: Option<String>,
    #[serde(default)]
    settings: serde_json::Value,
    #[serde(default, alias = "settings_encrypted_payload")]
    encrypted_payload: Option<EncryptedPayload>,
}

/// Trait for manifest items that can be stored as tree files.
pub trait TreeItem: serde::Serialize {
    fn path(&self) -> &str;
    fn name(&self) -> &str;
}

impl TreeItem for nenjo::manifest::AbilityManifest {
    fn path(&self) -> &str {
        &self.path
    }
    fn name(&self) -> &str {
        &self.name
    }
}

impl TreeItem for nenjo::manifest::DomainManifest {
    fn path(&self) -> &str {
        &self.path
    }
    fn name(&self) -> &str {
        &self.name
    }
}

impl TreeItem for ContextBlockManifest {
    fn path(&self) -> &str {
        &self.path
    }
    fn name(&self) -> &str {
        &self.name
    }
}

/// Loads context blocks from a local `.nenjo/context/` directory.
///
/// Convention: every `.md` file in `.nenjo/context/` becomes a context block.
/// - **name** = filename without extension (e.g. `coding_standards`)
/// - **path** = `"local"`
/// - **template** = file contents
pub struct LocalManifestLoader {
    root: PathBuf,
}

impl LocalManifestLoader {
    /// Create a loader that scans `root/.nenjo/context/*.md`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait::async_trait]
impl ManifestLoader for LocalManifestLoader {
    async fn load(&self) -> Result<Manifest> {
        let context_dir = self.root.join(".nenjo").join("context");
        let mut blocks = Vec::new();

        if context_dir.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&context_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                .collect();
            entries.sort_by_key(|e| e.file_name());

            for entry in entries {
                let path = entry.path();
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let template = std::fs::read_to_string(&path)?;
                blocks.push(ContextBlockManifest {
                    id: Uuid::new_v4(),
                    name,
                    path: "local".to_string(),
                    display_name: None,
                    description: None,
                    template,
                });
            }
        }

        Ok(Manifest {
            context_blocks: blocks,
            ..Default::default()
        })
    }
}

/// Fetch manifest data from the backend and cache it locally.
///
/// Creates `data` if it doesn't exist, calls the manifest endpoint, and
/// writes `projects.json`, `routines.json`, `agents.json`, etc.
///
/// On network / API errors the function logs a warning and returns `Ok(())`
/// so the worker can still start. Filesystem errors are propagated.
pub async fn sync(
    api: &NenjoClient,
    manifests_dir: &Path,
    workspace_dir: &Path,
    state_dir: &Path,
    nenjo_home: &Path,
) -> Result<()> {
    // Ensure the data directory exists (filesystem error = hard fail)
    std::fs::create_dir_all(manifests_dir).with_context(|| {
        format!(
            "Failed to create manifests directory: {}",
            manifests_dir.display()
        )
    })?;

    // Fetch bootstrap data — soft-fail on network/API errors
    let bootstrap = match api.fetch_manifest_json().await {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "Bootstrap fetch failed — worker will continue without cached data");
            return Ok(());
        }
    };

    let auth: BootstrapAuthEnvelope = serde_json::from_value(bootstrap.clone())
        .context("Failed to deserialize bootstrap auth response")?;
    ensure_worker_ack(
        api,
        state_dir,
        Some(auth.auth.user_id),
        auth.auth.api_key_id,
    )
    .await
    .context("Worker enrollment missing ACK required for bootstrap decrypt")?;
    let runtime_assets_home = workspace_dir.join(".nenjo");
    let data = hydrate_bootstrap_manifest(api, bootstrap, state_dir, &runtime_assets_home).await?;
    let manifest = &data.manifest;

    info!(
        projects = manifest.projects.len(),
        routines = manifest.routines.len(),
        models = manifest.models.len(),
        agents = manifest.agents.len(),
        councils = manifest.councils.len(),
        domains = manifest.domains.len(),
        mcp_servers = manifest.mcp_servers.len(),
        "Manifest fetched successfully"
    );

    // Write auth info used for org-scoped transport setup and ACK routing.
    atomic_write_json(manifests_dir, "auth.json", &data.auth)?;
    atomic_write_json(manifests_dir, "nats.json", &data.nats)?;
    atomic_write_json(manifests_dir, "projects.json", &manifest.projects)?;
    atomic_write_json(manifests_dir, "routines.json", &manifest.routines)?;
    atomic_write_json(manifests_dir, "models.json", &manifest.models)?;
    atomic_write_json(manifests_dir, "agents.json", &manifest.agents)?;
    atomic_write_json(manifests_dir, "councils.json", &manifest.councils)?;
    atomic_write_json(manifests_dir, "mcp_servers.json", &manifest.mcp_servers)?;
    sync_tree(&manifests_dir.join("domains"), &manifest.domains)?;
    sync_tree(&manifests_dir.join("abilities"), &manifest.abilities)?;
    sync_tree(
        &manifests_dir.join("context_blocks"),
        &manifest.context_blocks,
    )?;

    // Sync platform-uploaded and repo-backed library knowledge under ~/.nenjo/library.
    crate::local_documents::sync_all(api, nenjo_home, state_dir, &manifest.projects).await?;

    Ok(())
}

async fn hydrate_bootstrap_manifest(
    api: &NenjoClient,
    bootstrap: serde_json::Value,
    state_dir: &Path,
    nenjo_home: &Path,
) -> Result<HydratedBootstrap> {
    let bootstrap: BootstrapManifestResponse = match serde_json::from_value(bootstrap.clone()) {
        Ok(value) => value,
        Err(err) => {
            log_bootstrap_deserialize_failure(&bootstrap, &err);
            return Err(err).context("Failed to deserialize bootstrap manifest response");
        }
    };

    let mut agents = Vec::with_capacity(bootstrap.agents.len());
    for agent in bootstrap.agents {
        let prompt_config = resolve_bootstrap_prompt_config(api, &agent, state_dir).await?;
        agents.push(nenjo::manifest::AgentManifest {
            id: agent.id,
            name: agent.name,
            description: agent.description,
            prompt_config,
            color: agent.color,
            model_id: agent.model_id,
            domain_ids: agent.domains,
            platform_scopes: agent.platform_scopes,
            mcp_server_ids: agent.mcp_server_ids,
            ability_ids: agent.abilities,
            prompt_locked: agent.prompt_locked,
            heartbeat: agent.heartbeat,
        });
    }

    let mut context_blocks = Vec::with_capacity(bootstrap.context_blocks.len());
    for block in bootstrap.context_blocks {
        let template = resolve_bootstrap_context_block_template(&block, state_dir).await?;
        context_blocks.push(ContextBlockManifest {
            id: block.id,
            name: block.name,
            path: block.path,
            display_name: block.display_name,
            description: block.description,
            template,
        });
    }

    let mut projects = Vec::with_capacity(bootstrap.projects.len());
    for project in bootstrap.projects {
        let settings = resolve_bootstrap_project_settings(&project, state_dir).await?;
        projects.push(nenjo::manifest::ProjectManifest {
            id: project.id,
            name: project.name,
            slug: project.slug,
            description: project.description,
            settings,
        });
    }

    let mut abilities = Vec::with_capacity(bootstrap.abilities.len());
    for ability in bootstrap.abilities {
        abilities.push(crate::marketplace::hydrate_skill_ability(ability, nenjo_home).await?);
    }

    let mut mcp_servers = Vec::with_capacity(bootstrap.mcp_servers.len());
    for server in bootstrap.mcp_servers {
        mcp_servers.push(crate::marketplace::hydrate_plugin_mcp_server(server, nenjo_home).await?);
    }

    Ok(HydratedBootstrap {
        auth: bootstrap.auth.clone(),
        manifest: Manifest {
            routines: bootstrap.routines,
            models: bootstrap.models,
            agents,
            councils: bootstrap.councils,
            domains: bootstrap.domains,
            projects,
            mcp_servers,
            abilities,
            context_blocks,
        },
        nats: bootstrap.nats,
    })
}

pub fn load_cached_bootstrap_auth(manifests_dir: &Path) -> Option<BootstrapAuth> {
    let path = manifests_dir.join("auth.json");
    let content = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<BootstrapAuth>(&content) {
        Ok(auth) => Some(auth),
        Err(error) => {
            warn!(file = %path.display(), %error, "Failed to parse cached bootstrap auth");
            None
        }
    }
}

pub fn load_cached_nats_config(manifests_dir: &Path) -> Option<BootstrapNatsConfig> {
    let path = manifests_dir.join("nats.json");
    let content = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<BootstrapNatsConfig>(&content) {
        Ok(config) => Some(config),
        Err(error) => {
            warn!(file = %path.display(), %error, "Failed to parse cached NATS bootstrap config");
            None
        }
    }
}

fn log_bootstrap_deserialize_failure(bootstrap: &serde_json::Value, err: &serde_json::Error) {
    let preview = serde_json::to_string(bootstrap)
        .ok()
        .map(|text| text.chars().take(1000).collect::<String>())
        .unwrap_or_else(|| "<unavailable>".to_string());
    error!(
        error = %err,
        line = err.line(),
        column = err.column(),
        body_preview = %preview,
        "Failed to deserialize bootstrap manifest response"
    );

    let Some(object) = bootstrap.as_object() else {
        error!("Bootstrap payload was not a JSON object");
        return;
    };

    macro_rules! check_section {
        ($field:literal, $ty:ty) => {
            if let Some(value) = object.get($field) {
                if let Err(section_err) = serde_json::from_value::<$ty>(value.clone()) {
                    error!(
                        section = $field,
                        error = %section_err,
                        line = section_err.line(),
                        column = section_err.column(),
                        "Bootstrap section failed to deserialize"
                    );
                }
            }
        };
    }

    check_section!("routines", Vec<nenjo::manifest::RoutineManifest>);
    check_section!("models", Vec<nenjo::manifest::ModelManifest>);
    check_section!("agents", Vec<BootstrapAgentManifest>);
    check_section!("councils", Vec<nenjo::manifest::CouncilManifest>);
    check_section!("domains", Vec<nenjo::manifest::DomainManifest>);
    check_section!("projects", Vec<nenjo::manifest::ProjectManifest>);
    check_section!("mcp_servers", Vec<nenjo::manifest::McpServerManifest>);
    check_section!("abilities", Vec<nenjo::manifest::AbilityManifest>);
    check_section!("context_blocks", Vec<BootstrapContextBlockManifest>);
}

pub struct WorkerManifestCache {
    pub manifests_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl WorkerManifestCache {
    pub fn persist_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        let manifests_dir = &self.manifests_dir;
        match resource_type {
            ResourceType::Model => {
                atomic_write_json(manifests_dir, "models.json", &manifest.models)
            }
            ResourceType::Agent => {
                atomic_write_json(manifests_dir, "agents.json", &manifest.agents)
            }
            ResourceType::Routine => {
                atomic_write_json(manifests_dir, "routines.json", &manifest.routines)
            }
            ResourceType::Project => {
                atomic_write_json(manifests_dir, "projects.json", &manifest.projects)
            }
            ResourceType::Council => {
                atomic_write_json(manifests_dir, "councils.json", &manifest.councils)
            }
            ResourceType::Ability => {
                sync_tree(&manifests_dir.join("abilities"), &manifest.abilities)
            }
            ResourceType::ContextBlock => sync_tree(
                &manifests_dir.join("context_blocks"),
                &manifest.context_blocks,
            ),
            ResourceType::McpServer => {
                atomic_write_json(manifests_dir, "mcp_servers.json", &manifest.mcp_servers)
            }
            ResourceType::Domain => sync_tree(&manifests_dir.join("domains"), &manifest.domains),
            ResourceType::Document => Ok(()),
        }
    }

    pub async fn full_refresh(&self, api: &NenjoClient) -> Result<nenjo::Manifest> {
        sync(
            api,
            &self.manifests_dir,
            &self.workspace_dir,
            &self.state_dir,
            &self.config_dir,
        )
        .await?;
        let loader = nenjo::LocalManifestStore::new(&self.manifests_dir);
        nenjo::ManifestLoader::load(&loader).await
    }

    async fn knowledge_pack_dir(&self, api: &NenjoClient, pack_id: Uuid) -> Result<PathBuf> {
        let slug = api
            .list_knowledge_packs()
            .await?
            .into_iter()
            .find(|pack| pack.id == pack_id)
            .map(|pack| pack.slug)
            .unwrap_or_else(|| pack_id.to_string());
        Ok(self.config_dir.join("library").join("platform").join(slug))
    }

    fn knowledge_pack_slug_dir(&self, slug: &str) -> Option<PathBuf> {
        let slug = slug.trim();
        (!slug.is_empty()).then(|| self.config_dir.join("library").join("platform").join(slug))
    }

    fn platform_library_root(&self) -> PathBuf {
        self.config_dir.join("library").join("platform")
    }
}

#[async_trait::async_trait]
impl ManifestStore for WorkerManifestCache {
    async fn prepare_resource(
        &self,
        manifest: &mut nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        match resource_type {
            ResourceType::Ability => {
                let abilities = std::mem::take(&mut manifest.abilities);
                let mut hydrated = Vec::with_capacity(abilities.len());
                for ability in abilities {
                    hydrated.push(
                        crate::marketplace::hydrate_skill_ability(
                            ability,
                            &self.workspace_dir.join(".nenjo"),
                        )
                        .await?,
                    );
                }
                manifest.abilities = hydrated;
            }
            ResourceType::McpServer => {
                let servers = std::mem::take(&mut manifest.mcp_servers);
                let mut hydrated = Vec::with_capacity(servers.len());
                for server in servers {
                    hydrated.push(
                        crate::marketplace::hydrate_plugin_mcp_server(
                            server,
                            &self.workspace_dir.join(".nenjo"),
                        )
                        .await?,
                    );
                }
                manifest.mcp_servers = hydrated;
            }
            _ => {}
        }
        Ok(())
    }

    async fn persist_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        WorkerManifestCache::persist_resource(self, manifest, resource_type)
    }

    async fn remove_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
        _resource_id: Uuid,
    ) -> Result<()> {
        WorkerManifestCache::persist_resource(self, manifest, resource_type)
    }

    async fn cleanup_deleted_resource(
        &self,
        resource_type: ResourceType,
        resource_id: Uuid,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        let nenjo_home = self.workspace_dir.join(".nenjo");
        match resource_type {
            ResourceType::Ability => {
                let Some(metadata) = deleted_resource_metadata(payload, "skill") else {
                    return Ok(());
                };
                crate::marketplace::uninstall_skill_ability(resource_id, metadata, &nenjo_home)
                    .await?;
            }
            ResourceType::McpServer => {
                let Some(metadata) = deleted_resource_metadata(payload, "plugin") else {
                    return Ok(());
                };
                crate::marketplace::uninstall_plugin_mcp_server(resource_id, metadata, &nenjo_home)
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn full_refresh(&self, client: &NenjoClient) -> Result<nenjo::Manifest> {
        WorkerManifestCache::full_refresh(self, client).await
    }

    async fn sync_document_metadata(
        &self,
        client: &NenjoClient,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        document_id: Uuid,
        metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        let pack_dir = if let Some(meta) = metadata {
            if let Some(pack_dir) = self.knowledge_pack_slug_dir(&meta.slug) {
                pack_dir
            } else {
                self.knowledge_pack_dir(client, meta.pack_id).await?
            }
        } else {
            crate::local_documents::manifest_library_dir(
                manifest,
                &self.platform_library_root(),
                project_id,
            )
        };
        crate::local_documents::sync_document_metadata(
            client,
            &pack_dir,
            project_id,
            document_id,
            metadata,
        )
        .await
    }

    async fn sync_document(
        &self,
        client: &NenjoClient,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        document_id: Uuid,
        metadata: Option<&DocumentSyncMeta>,
    ) -> Result<()> {
        let pack_dir = if let Some(meta) = metadata {
            if let Some(pack_dir) = self.knowledge_pack_slug_dir(&meta.slug) {
                pack_dir
            } else {
                self.knowledge_pack_dir(client, meta.pack_id).await?
            }
        } else {
            crate::local_documents::manifest_library_dir(
                manifest,
                &self.platform_library_root(),
                project_id,
            )
        };
        crate::local_documents::sync_document(
            client,
            &pack_dir,
            project_id,
            document_id,
            &self.state_dir,
            metadata,
        )
        .await
    }

    fn remove_document(
        &self,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        document_id: Uuid,
    ) -> Result<()> {
        crate::local_documents::remove_manifest_document(
            manifest,
            &self.platform_library_root(),
            project_id,
            document_id,
        )
    }

    fn write_document_content(
        &self,
        manifest: &nenjo::Manifest,
        project_id: Uuid,
        relative_path: &str,
        content: &str,
    ) -> Result<()> {
        let pack_dir = crate::local_documents::manifest_library_dir(
            manifest,
            &self.platform_library_root(),
            project_id,
        );
        crate::local_documents::write_document_content(&pack_dir, relative_path, content)
    }
}

fn deleted_resource_metadata(
    payload: Option<&serde_json::Value>,
    expected_source_type: &str,
) -> Option<serde_json::Value> {
    let payload = payload?;
    if payload
        .get("source_type")
        .and_then(serde_json::Value::as_str)
        != Some(expected_source_type)
    {
        return None;
    }
    payload
        .get("metadata")
        .cloned()
        .or_else(|| payload.get("install_metadata").cloned())
}

async fn ensure_worker_ack(
    api: &NenjoClient,
    state_dir: &Path,
    ack_actor_user_id: Option<Uuid>,
    api_key_id: Option<Uuid>,
) -> Result<crate::crypto::ContentKey> {
    let ack_actor_user_id = ack_actor_user_id
        .context("Bootstrap response did not include auth.user_id for ACK routing")?;
    let api_key_id =
        api_key_id.context("Bootstrap response did not include auth.api_key_id for enrollment")?;
    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("Failed to load worker auth provider for bootstrap")?;
    auth_provider
        .sync_worker_enrollment(api, api_key_id, ack_actor_user_id, None)
        .await
        .context("Failed to sync worker enrollment before bootstrap")?;
    auth_provider
        .load_ack_for_user(ack_actor_user_id)
        .await
        .context("Failed to load ACK for bootstrap decrypt")?
        .context("Worker has no enrolled ACK yet")
}

async fn resolve_bootstrap_prompt_config(
    api: &NenjoClient,
    agent: &BootstrapAgentManifest,
    state_dir: &Path,
) -> Result<PromptConfig> {
    let Some(payload) = agent.encrypted_payload.as_ref() else {
        let Some(response) = api.fetch_agent_prompt_config(agent.id).await? else {
            return Ok(PromptConfig::default());
        };

        if let Some(payload) = response.encrypted_payload.as_ref() {
            return decrypt_prompt_config_payload(payload, state_dir, agent.id).await;
        }

        return Ok(response.prompt_config.unwrap_or_default());
    };

    decrypt_prompt_config_payload(payload, state_dir, agent.id).await
}

async fn decrypt_prompt_config_payload(
    payload: &EncryptedPayload,
    state_dir: &Path,
    agent_id: Uuid,
) -> Result<PromptConfig> {
    if payload.object_type
        != ManifestKind::Agent
            .encrypted_object_type()
            .expect("agent prompt object type")
    {
        anyhow::bail!(
            "Unsupported encrypted bootstrap payload type '{}' for agent {}",
            payload.object_type,
            agent_id
        );
    }

    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("Failed to load worker auth provider for bootstrap prompt decrypt")?;
    let plaintext = decrypt_text_with_provider(&auth_provider, payload)
        .await
        .with_context(|| {
            format!(
                "Failed to decrypt bootstrap prompt payload for agent {}",
                agent_id
            )
        })?;

    serde_json::from_str::<PromptConfig>(&plaintext).with_context(|| {
        format!(
            "Failed to parse decrypted bootstrap prompt config JSON for agent {}",
            agent_id
        )
    })
}

async fn resolve_bootstrap_context_block_template(
    block: &BootstrapContextBlockManifest,
    state_dir: &Path,
) -> Result<String> {
    let Some(payload) = block.encrypted_payload.as_ref() else {
        return Ok(block.template.clone());
    };

    decrypt_context_block_template_payload(payload, state_dir, block.id).await
}

async fn decrypt_context_block_template_payload(
    payload: &EncryptedPayload,
    state_dir: &Path,
    block_id: Uuid,
) -> Result<String> {
    if payload.object_type
        != ManifestKind::ContextBlock
            .encrypted_object_type()
            .expect("context block content object type")
    {
        anyhow::bail!(
            "Unsupported encrypted bootstrap payload type '{}' for context block {}",
            payload.object_type,
            block_id
        );
    }

    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("Failed to load worker auth provider for bootstrap context block decrypt")?;
    let plaintext = decrypt_text_with_provider(&auth_provider, payload)
        .await
        .with_context(|| {
            format!(
                "Failed to decrypt bootstrap context block payload for context block {}",
                block_id
            )
        })?;

    serde_json::from_str::<String>(&plaintext).with_context(|| {
        format!(
            "Failed to parse decrypted bootstrap context block template JSON for context block {}",
            block_id
        )
    })
}

async fn resolve_bootstrap_project_settings(
    project: &BootstrapProjectManifest,
    state_dir: &Path,
) -> Result<serde_json::Value> {
    let mut settings = project.settings.clone();
    let Some(payload) = project.encrypted_payload.as_ref() else {
        return Ok(settings);
    };

    let decrypted = decrypt_project_settings_payload(payload, state_dir, project.id).await?;
    merge_json_object(&mut settings, decrypted).with_context(|| {
        format!(
            "Failed to merge decrypted bootstrap project settings for project {}",
            project.id
        )
    })?;
    Ok(settings)
}

async fn decrypt_project_settings_payload(
    payload: &EncryptedPayload,
    state_dir: &Path,
    project_id: Uuid,
) -> Result<serde_json::Value> {
    if payload.object_type != "project.settings" {
        anyhow::bail!(
            "Unsupported encrypted bootstrap payload type '{}' for project {}",
            payload.object_type,
            project_id
        );
    }

    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("Failed to load worker auth provider for bootstrap project settings decrypt")?;
    let plaintext = decrypt_text_with_provider(&auth_provider, payload)
        .await
        .with_context(|| {
            format!(
                "Failed to decrypt bootstrap project settings payload for project {}",
                project_id
            )
        })?;

    serde_json::from_str::<serde_json::Value>(&plaintext).with_context(|| {
        format!(
            "Failed to parse decrypted bootstrap project settings JSON for project {}",
            project_id
        )
    })
}

fn merge_json_object(target: &mut serde_json::Value, source: serde_json::Value) -> Result<()> {
    if target.is_null() {
        *target = serde_json::json!({});
    }
    let target = target
        .as_object_mut()
        .context("bootstrap JSON merge target was not an object")?;
    let source = source
        .as_object()
        .context("decrypted bootstrap JSON merge source was not an object")?;
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
    Ok(())
}

/// Sync a list of tree items to a directory tree on disk.
///
/// Each item is written to `{base_dir}/{path}/{name}.json`. Stale files that
/// are not in the expected set are removed, and empty directories are cleaned up.
pub fn sync_tree<T: TreeItem>(base_dir: &Path, items: &[T]) -> Result<()> {
    std::fs::create_dir_all(base_dir)
        .with_context(|| format!("Failed to create tree dir: {}", base_dir.display()))?;

    // Build the set of expected file paths.
    let mut expected: HashSet<PathBuf> = HashSet::new();
    for item in items {
        let file_path = tree_item_path(base_dir, item.path(), item.name());
        expected.insert(file_path);
    }

    // Remove stale files.
    if base_dir.is_dir() {
        remove_stale_files(base_dir, &expected)?;
    }

    // Write each item.
    for item in items {
        let file_path = tree_item_path(base_dir, item.path(), item.name());
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create dir: {}", parent.display()))?;
        }
        let tmp = unique_tree_tmp_path(&file_path);
        let json = serde_json::to_string_pretty(item)
            .with_context(|| format!("Failed to serialize tree item: {}", file_path.display()))?;
        std::fs::write(&tmp, json.as_bytes())
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &file_path).with_context(|| {
            format!(
                "Failed to rename {} → {}",
                tmp.display(),
                file_path.display()
            )
        })?;
    }

    debug!(dir = %base_dir.display(), count = items.len(), "Tree synced");
    Ok(())
}

/// Compute the file path for a tree item: `{base_dir}/{path}/{name}.json`
pub fn tree_item_path(base_dir: &Path, path: &str, name: &str) -> PathBuf {
    if path.is_empty() {
        base_dir.join(format!("{name}.json"))
    } else {
        base_dir.join(path).join(format!("{name}.json"))
    }
}

fn unique_tree_tmp_path(file_path: &Path) -> PathBuf {
    let nonce = CACHE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let filename = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("resource.json");
    file_path.with_file_name(format!(".{filename}.{pid}.{nonce}.tmp"))
}

/// Recursively remove files in `dir` that are not in the expected set, and clean up
/// empty directories.
fn remove_stale_files(dir: &Path, expected: &HashSet<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            remove_stale_files(&path, expected)?;
            // Remove directory if now empty.
            if path.read_dir().map_or(true, |mut d| d.next().is_none()) {
                let _ = std::fs::remove_dir(&path);
            }
        } else if path.extension().is_some_and(|ext| ext == "json") && !expected.contains(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Write `value` as pretty-printed JSON to `dir/filename` via a temporary file
/// and atomic rename, so readers never see a partial write.
fn atomic_write_json<T: serde::Serialize>(dir: &Path, filename: &str, value: &T) -> Result<()> {
    let target = dir.join(filename);
    let tmp = unique_cache_tmp_path(&target, filename);
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create manifest cache dir {}", dir.display()))?;

    let json = serde_json::to_string_pretty(value)
        .with_context(|| format!("Failed to serialize {filename}"))?;

    std::fs::write(&tmp, json.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;

    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;

    Ok(())
}

fn unique_cache_tmp_path(target: &Path, filename: &str) -> PathBuf {
    let nonce = CACHE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    target.with_file_name(format!(".{filename}.{pid}.{nonce}.tmp"))
}
