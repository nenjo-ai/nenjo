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

use anyhow::{Context, Result, anyhow};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, error, info, warn};

use crate::crypto::WorkerAuthProvider;
use crate::crypto::decrypt_text_with_provider;
use nenjo::Slug;
use nenjo::agents::prompts::PromptConfig;
use nenjo::manifest::{
    CommandManifest, ContextBlockManifest, HasManifestSlug, Manifest, ManifestLoader,
    ManifestResource, ManifestResourceKind,
};
use nenjo::{LocalManifestStore, ManifestReader, ManifestWriter};
use nenjo_events::{
    Capability, EncryptedPayload, ManifestResourcePayload, ModelAssignmentBinding,
    ModelAssignmentsManifestUpdate, ModelCapabilityDefaultBinding,
    ModelCapabilityDefaultsManifestUpdate, PackageArgumentBindingUpdate, ResourceAction,
    ResourceType,
};
use nenjo_platform::api_client::{ApiClient, KnowledgeDocumentRecord};
use nenjo_platform::manifest_contract::ModelRecord;
use nenjo_platform::{
    PlatformResourceIdSnapshot, PlatformResourceIdStore, PlatformResourceKind, SensitiveContentKind,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::config::MediaProviderConfig;
use crate::handlers::manifest::ManifestStore;
use crate::media::{AgentModelAssignments, ModelRuntimeConfig};

static CACHE_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
struct BootstrapManifestResponse {
    auth: BootstrapAuth,
    #[serde(default)]
    routines: Vec<BootstrapRoutineManifest>,
    #[serde(default)]
    models: Vec<BootstrapModelManifest>,
    #[serde(default)]
    media_providers: Vec<MediaProviderConfig>,
    #[serde(default)]
    capability_defaults: Vec<ModelCapabilityDefaultBinding>,
    #[serde(default)]
    agents: Vec<BootstrapAgentManifest>,
    #[serde(default)]
    councils: Vec<nenjo::manifest::CouncilManifest>,
    #[serde(default)]
    domains: Vec<BootstrapDomainManifest>,
    #[serde(default)]
    projects: Vec<BootstrapProjectManifest>,
    #[serde(default)]
    mcp_servers: Vec<nenjo::manifest::McpServerManifest>,
    #[serde(default)]
    commands: Vec<BootstrapCommandManifest>,
    #[serde(default)]
    hooks: Vec<nenjo::manifest::HookManifest>,
    #[serde(default)]
    script_tools: Vec<nenjo::manifest::ScriptToolManifest>,
    #[serde(default)]
    abilities: Vec<BootstrapAbilityManifest>,
    #[serde(default)]
    context_blocks: Vec<BootstrapContextBlockManifest>,
    #[serde(default)]
    nats: BootstrapNatsConfig,
    #[serde(default)]
    packages: Option<BootstrapPackages>,
}

struct HydratedBootstrap {
    auth: BootstrapAuth,
    manifest: Manifest,
    resource_ids: PlatformResourceIdSnapshot,
    media_providers: Vec<MediaProviderConfig>,
    cached_models: Vec<CachedModelManifest>,
    cached_agents: Vec<CachedAgentManifest>,
    capability_defaults: Vec<ModelCapabilityDefaultBinding>,
    nats: BootstrapNatsConfig,
    packages: Option<BootstrapPackages>,
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
pub struct BootstrapPackages {
    pub schema: String,
    pub nenpm_yml: String,
    pub nenpm_lock_yml: String,
    #[serde(default)]
    pub argument_bindings: Vec<PackageArgumentBindingUpdate>,
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

/// Canonical worker cache entry for a configured model.
///
/// Extra routing fields are stored alongside the core manifest fields in
/// `models.json`; generic manifest loaders safely ignore them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedModelManifest {
    pub id: Uuid,
    /// Assignable operation capability IDs from the platform models inventory.
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(flatten)]
    pub manifest: nenjo::manifest::ModelManifest,
}

type BootstrapModelManifest = CachedModelManifest;

/// Canonical worker cache entry for an agent and its configured model bindings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAgentManifest {
    pub id: Uuid,
    #[serde(flatten)]
    pub manifest: nenjo::manifest::AgentManifest,
    #[serde(default)]
    pub model_assignments: Vec<ModelAssignmentBinding>,
}

#[derive(Debug, Deserialize)]
struct BootstrapAgentManifest {
    id: Uuid,
    name: String,
    #[serde(default)]
    slug: Option<Slug>,
    description: Option<String>,
    color: Option<String>,
    model: Option<Slug>,
    #[serde(default)]
    model_assignments: Vec<ModelAssignmentBinding>,
    #[serde(default)]
    domains: Vec<Slug>,
    #[serde(default)]
    platform_scopes: Vec<String>,
    #[serde(default)]
    mcp_servers: Vec<Slug>,
    #[serde(default)]
    script_tools: Vec<Slug>,
    #[serde(default)]
    media: Vec<nenjo::manifest::MediaRequirement>,
    #[serde(default)]
    abilities: Vec<String>,
    #[serde(default)]
    prompt_locked: bool,
    #[serde(default)]
    source_type: Option<String>,
    #[serde(default)]
    metadata: serde_json::Value,
    #[serde(default)]
    heartbeat: Option<nenjo::manifest::AgentHeartbeatManifest>,
    #[serde(default)]
    encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Deserialize)]
struct BootstrapAbilityManifest {
    id: Uuid,
    #[serde(flatten)]
    manifest: nenjo::manifest::AbilityManifest,
    #[serde(default)]
    encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Deserialize)]
struct BootstrapDomainManifest {
    id: Uuid,
    #[serde(flatten)]
    manifest: nenjo::manifest::DomainManifest,
    #[serde(default)]
    encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Deserialize)]
struct BootstrapContextBlockManifest {
    id: Uuid,
    name: String,
    #[serde(default)]
    path: String,
    description: Option<String>,
    #[serde(default)]
    template: String,
    #[serde(default)]
    encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Deserialize)]
struct BootstrapCommandManifest {
    id: Uuid,
    #[serde(flatten)]
    manifest: CommandManifest,
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
    #[serde(default)]
    encrypted_payload: Option<EncryptedPayload>,
}

#[derive(Debug, Deserialize)]
struct BootstrapRoutineManifest {
    name: String,
    #[serde(default)]
    slug: Option<Slug>,
    description: Option<String>,
    #[serde(default)]
    trigger: nenjo::manifest::RoutineTrigger,
    #[serde(default)]
    metadata: nenjo::manifest::RoutineMetadata,
    #[serde(default)]
    steps: Vec<BootstrapRoutineStepManifest>,
    #[serde(default)]
    edges: Vec<BootstrapRoutineEdgeManifest>,
}

#[derive(Debug, Deserialize)]
struct BootstrapRoutineStepManifest {
    #[serde(default)]
    slug: Option<Slug>,
    #[serde(default)]
    routine: Option<Slug>,
    name: String,
    step_type: nenjo::manifest::RoutineStepType,
    #[serde(default)]
    council: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    config: serde_json::Value,
    #[serde(default)]
    order_index: i32,
}

#[derive(Debug, Deserialize)]
struct BootstrapRoutineEdgeManifest {
    #[serde(default)]
    routine: Option<Slug>,
    source_step: String,
    target_step: String,
    condition: nenjo::manifest::RoutineEdgeCondition,
    #[serde(default)]
    metadata: serde_json::Value,
}

/// Trait for manifest items that can be stored as tree files.
pub trait TreeItem: serde::Serialize {
    fn path(&self) -> &str;
    fn name(&self) -> &str;
}

impl TreeItem for nenjo::manifest::AbilityManifest {
    fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("")
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
                    name,
                    path: "local".to_string(),
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
    api: &ApiClient,
    manifests_dir: &Path,
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

    sync_bootstrap_manifest(api, manifests_dir, state_dir, nenjo_home, bootstrap).await
}

/// Fetch and apply bootstrap data, failing when the worker cannot observe the
/// platform state it just wrote.
///
/// Startup uses [`sync`] so an offline worker can continue from its cache.
/// Callers that have already committed a platform mutation use this strict
/// variant instead: reporting success while retaining an old provider snapshot
/// would make the mutation appear to have been lost until pub/sub catches up.
pub async fn sync_required(
    api: &ApiClient,
    manifests_dir: &Path,
    state_dir: &Path,
    nenjo_home: &Path,
) -> Result<()> {
    std::fs::create_dir_all(manifests_dir).with_context(|| {
        format!(
            "Failed to create manifests directory: {}",
            manifests_dir.display()
        )
    })?;
    let bootstrap = api
        .fetch_manifest_json()
        .await
        .context("Bootstrap fetch failed after a platform mutation")?;

    sync_bootstrap_manifest(api, manifests_dir, state_dir, nenjo_home, bootstrap).await
}

async fn sync_bootstrap_manifest(
    api: &ApiClient,
    manifests_dir: &Path,
    state_dir: &Path,
    nenjo_home: &Path,
    bootstrap: serde_json::Value,
) -> Result<()> {
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
    let data = hydrate_bootstrap_manifest(api, bootstrap, state_dir).await?;
    let manifest = &data.manifest;

    info!(
        projects = manifest.projects.len(),
        routines = manifest.routines.len(),
        models = manifest.models.len(),
        agents = manifest.agents.len(),
        councils = manifest.councils.len(),
        domains = manifest.domains.len(),
        mcp_servers = manifest.mcp_servers.len(),
        commands = manifest.commands.len(),
        hooks = manifest.hooks.len(),
        script_tools = manifest.script_tools.len(),
        "Manifest fetched successfully"
    );

    // Write auth info used for org-scoped transport setup and ACK routing.
    atomic_write_json(manifests_dir, "auth.json", &data.auth)?;
    atomic_write_json(manifests_dir, "nats.json", &data.nats)?;
    atomic_write_json(manifests_dir, "media_providers.json", &data.media_providers)?;
    atomic_write_json(
        manifests_dir,
        "capability_defaults.json",
        &data.capability_defaults,
    )?;
    PlatformResourceIdStore::new(manifests_dir).replace(&data.resource_ids)?;
    atomic_write_json(manifests_dir, "projects.json", &manifest.projects)?;
    atomic_write_json(manifests_dir, "routines.json", &manifest.routines)?;
    atomic_write_json(manifests_dir, "models.json", &data.cached_models)?;
    atomic_write_json(manifests_dir, "agents.json", &data.cached_agents)?;
    remove_legacy_model_cache_files(manifests_dir)?;
    atomic_write_json(manifests_dir, "councils.json", &manifest.councils)?;
    atomic_write_json(manifests_dir, "mcp_servers.json", &manifest.mcp_servers)?;
    atomic_write_json(manifests_dir, "commands.json", &manifest.commands)?;
    atomic_write_json(manifests_dir, "hooks.json", &manifest.hooks)?;
    atomic_write_json(manifests_dir, "script_tools.json", &manifest.script_tools)?;
    sync_tree(&manifests_dir.join("domains"), &manifest.domains)?;
    sync_tree(&manifests_dir.join("abilities"), &manifest.abilities)?;
    sync_tree(
        &manifests_dir.join("context_blocks"),
        &manifest.context_blocks,
    )?;
    if let Some(packages) = &data.packages
        && let Err(error) = sync_platform_packages(nenjo_home, packages).await
    {
        warn!(error = ?error, "Platform package install failed; cached manifest resources remain available");
    }

    // Sync user-uploaded library knowledge under ~/.nenjo/library.
    crate::local_documents::sync_all(
        api,
        nenjo_home,
        state_dir,
        manifests_dir,
        &manifest.projects,
    )
    .await?;

    Ok(())
}

async fn hydrate_bootstrap_manifest(
    api: &ApiClient,
    bootstrap: serde_json::Value,
    state_dir: &Path,
) -> Result<HydratedBootstrap> {
    let bootstrap: BootstrapManifestResponse = match serde_json::from_value(bootstrap.clone()) {
        Ok(value) => value,
        Err(err) => {
            log_bootstrap_deserialize_failure(&bootstrap, &err);
            return Err(err).context("Failed to deserialize bootstrap manifest response");
        }
    };

    let mut resource_ids = PlatformResourceIdSnapshot::default();

    let mut models = Vec::with_capacity(bootstrap.models.len());
    let mut cached_models = Vec::with_capacity(bootstrap.models.len());
    for model in bootstrap.models {
        resource_ids.insert(PlatformResourceKind::Model, &model.manifest.slug, model.id);
        models.push(model.manifest.clone());
        cached_models.push(model);
    }

    let mut agents = Vec::with_capacity(bootstrap.agents.len());
    let mut cached_agents = Vec::with_capacity(bootstrap.agents.len());
    for agent in bootstrap.agents {
        let prompt_config = resolve_bootstrap_prompt_config(api, &agent, state_dir).await?;
        let slug = agent
            .slug
            .clone()
            .unwrap_or_else(|| Slug::derive(&agent.name));
        resource_ids.insert(PlatformResourceKind::Agent, &slug, agent.id);
        let manifest = nenjo::manifest::AgentManifest {
            name: agent.name,
            slug,
            description: agent.description,
            prompt_config,
            color: agent.color,
            model: agent.model,
            domains: agent.domains,
            platform_scopes: agent.platform_scopes,
            mcp_servers: agent.mcp_servers,
            script_tools: agent.script_tools,
            media: agent.media,
            abilities: agent.abilities,
            prompt_locked: agent.prompt_locked,
            heartbeat: agent.heartbeat,
            source_type: agent.source_type,
            metadata: agent.metadata,
        };
        cached_agents.push(CachedAgentManifest {
            id: agent.id,
            manifest: manifest.clone(),
            model_assignments: agent.model_assignments,
        });
        agents.push(manifest);
    }

    let mut domains = Vec::with_capacity(bootstrap.domains.len());
    for domain in bootstrap.domains {
        let prompt_config = resolve_bootstrap_domain_prompt_config(&domain, state_dir).await?;
        let mut manifest = domain.manifest;
        manifest.prompt_config = prompt_config;
        resource_ids.insert(
            PlatformResourceKind::Domain,
            &manifest.manifest_slug(),
            domain.id,
        );
        domains.push(manifest);
    }

    let mut abilities = Vec::with_capacity(bootstrap.abilities.len());
    for ability in bootstrap.abilities {
        let prompt_config = resolve_bootstrap_ability_prompt_config(&ability, state_dir).await?;
        let mut manifest = ability.manifest;
        manifest.prompt_config = prompt_config;
        resource_ids.insert(
            PlatformResourceKind::Ability,
            &manifest.manifest_slug(),
            ability.id,
        );
        abilities.push(manifest);
    }

    let mut context_blocks = Vec::with_capacity(bootstrap.context_blocks.len());
    for block in bootstrap.context_blocks {
        let template = resolve_bootstrap_context_block_template(&block, state_dir).await?;
        let context_block = ContextBlockManifest {
            name: block.name,
            path: block.path,
            description: block.description,
            template,
        };
        resource_ids.insert(
            PlatformResourceKind::ContextBlock,
            &context_block.manifest_slug(),
            block.id,
        );
        context_blocks.push(context_block);
    }

    let mut commands = Vec::with_capacity(bootstrap.commands.len());
    for command in bootstrap.commands {
        let content = resolve_bootstrap_command_content(&command, state_dir).await?;
        let mut manifest = command.manifest;
        manifest.content = content;
        resource_ids.insert(
            PlatformResourceKind::Command,
            &manifest.manifest_slug(),
            command.id,
        );
        commands.push(manifest);
    }

    let mut projects = Vec::with_capacity(bootstrap.projects.len());
    for project in bootstrap.projects {
        let settings = resolve_bootstrap_project_settings(&project, state_dir).await?;
        let project_manifest = nenjo::manifest::ProjectManifest {
            name: project.name,
            slug: Slug::derive(&project.slug),
            description: project.description,
            settings,
        };
        resource_ids.insert(
            PlatformResourceKind::Project,
            &project_manifest.slug,
            project.id,
        );
        projects.push(project_manifest);
    }

    let routines = bootstrap
        .routines
        .into_iter()
        .map(bootstrap_routine_manifest)
        .collect();

    Ok(HydratedBootstrap {
        auth: bootstrap.auth.clone(),
        manifest: Manifest {
            routines,
            models,
            agents,
            councils: bootstrap.councils,
            domains,
            projects,
            mcp_servers: bootstrap.mcp_servers,
            abilities,
            context_blocks,
            skills: Vec::new(),
            commands,
            hooks: bootstrap.hooks,
            script_tools: bootstrap.script_tools,
            knowledge_packs: Vec::new(),
        },
        resource_ids,
        media_providers: bootstrap.media_providers,
        cached_models,
        cached_agents,
        capability_defaults: bootstrap.capability_defaults,
        nats: bootstrap.nats,
        packages: bootstrap.packages,
    })
}

pub(crate) async fn sync_platform_packages(
    nenjo_home: &Path,
    packages: &BootstrapPackages,
) -> Result<PlatformPackageSyncStatus> {
    if packages.schema != "nenjo.platform_packages.v1" {
        warn!(
            schema = %packages.schema,
            "Ignoring unsupported platform package bootstrap schema"
        );
        return Ok(PlatformPackageSyncStatus::UnsupportedSchema);
    }
    let root = nenjo_home.join("platform_pkgs");
    let staging_root = unique_cache_tmp_path(&root, "platform_pkgs");
    let sync_result = sync_platform_packages_staged(&staging_root, packages).await;
    if sync_result.is_err()
        && let Err(error) = remove_cache_path(&staging_root)
    {
        warn!(
            path = %staging_root.display(),
            error = ?error,
            "Failed to clean failed platform package staging directory"
        );
    }
    sync_result?;
    replace_package_cache(&root, &staging_root)?;
    Ok(PlatformPackageSyncStatus::Applied)
}

async fn sync_platform_packages_staged(
    staging_root: &Path,
    packages: &BootstrapPackages,
) -> Result<()> {
    write_text_if_changed(staging_root, "nenpm.yml", &packages.nenpm_yml)?;
    write_text_if_changed(staging_root, "nenpm.lock.yml", &packages.nenpm_lock_yml)?;
    let argument_bindings = serde_json::to_string_pretty(&packages.argument_bindings)
        .context("failed to serialize platform package argument bindings")?;
    write_text_if_changed(staging_root, "argument_bindings.json", &argument_bindings)?;
    let install_root = staging_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        nenjo_nenpm::install(
            nenjo_nenpm::InstallOptions::new(&install_root)
                .packages_dir(&install_root)
                .locked(true)
                .fetch_mode(nenjo_nenpm::FetchMode::Provider),
        )
    })
    .await
    .context("platform package install task failed")?
    .context("failed to install platform packages")?;
    Ok(())
}

fn replace_package_cache(root: &Path, staging_root: &Path) -> Result<()> {
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create package cache parent {}", parent.display())
        })?;
    }
    let backup = unique_cache_tmp_path(root, "platform_pkgs-prev");
    let had_root = root.exists();
    if had_root {
        std::fs::rename(root, &backup).with_context(|| {
            format!(
                "Failed to move current package cache {} to {}",
                root.display(),
                backup.display()
            )
        })?;
    }

    if let Err(error) = std::fs::rename(staging_root, root) {
        if had_root && let Err(restore_error) = std::fs::rename(&backup, root) {
            return Err(anyhow!(
                "Failed to activate staged package cache {} as {}: {error}; also failed to restore previous cache {}: {restore_error}",
                staging_root.display(),
                root.display(),
                backup.display()
            ));
        }
        return Err(error).with_context(|| {
            format!(
                "Failed to activate staged package cache {} as {}",
                staging_root.display(),
                root.display()
            )
        });
    }

    if had_root && let Err(error) = remove_cache_path(&backup) {
        warn!(
            path = %backup.display(),
            error = ?error,
            "Failed to remove previous platform package cache backup"
        );
    }
    Ok(())
}

fn remove_cache_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove {}", path.display()))
    } else {
        std::fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlatformPackageSyncStatus {
    Applied,
    UnsupportedSchema,
}

fn write_text_if_changed(dir: &Path, filename: &str, content: &str) -> Result<()> {
    let target = dir.join(filename);
    if std::fs::read_to_string(&target)
        .map(|current| current == content)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let tmp = unique_cache_tmp_path(&target, filename);
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create package cache dir {}", dir.display()))?;
    std::fs::write(&tmp, content.as_bytes())
        .with_context(|| format!("Failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), target.display()))?;
    Ok(())
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

pub fn load_cached_media_providers(manifests_dir: &Path) -> Vec<MediaProviderConfig> {
    load_cached_json_vec(manifests_dir, "media_providers.json")
}

/// Load the canonical configured model inventory cached in `models.json`.
pub fn load_cached_models(manifests_dir: &Path) -> Vec<CachedModelManifest> {
    load_cached_json_vec(manifests_dir, "models.json")
}

/// Derive the runtime model inventory from the canonical models cache.
pub fn load_cached_model_runtime(manifests_dir: &Path) -> Vec<ModelRuntimeConfig> {
    load_cached_models(manifests_dir)
        .into_iter()
        .map(|model| ModelRuntimeConfig {
            id: model.id,
            slug: model.manifest.slug,
            model: model.manifest.model,
            model_provider: model.manifest.model_provider,
            base_url: model
                .manifest
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            capabilities: model
                .capabilities
                .into_iter()
                .map(|capability| capability.trim().to_owned())
                .filter(|capability| !capability.is_empty())
                .collect(),
        })
        .collect()
}

/// Load agent-owned configured model assignments from `agents.json`.
pub fn load_cached_agent_model_assignments(manifests_dir: &Path) -> Vec<AgentModelAssignments> {
    load_cached_json_vec::<CachedAgentManifest>(manifests_dir, "agents.json")
        .into_iter()
        .map(|agent| AgentModelAssignments {
            agent_id: agent.id,
            agent_slug: agent.manifest.slug,
            assignments: agent.model_assignments,
        })
        .collect()
}

/// Load bootstrap org capability defaults.
pub fn load_cached_capability_defaults(manifests_dir: &Path) -> Vec<ModelCapabilityDefaultBinding> {
    load_cached_json_vec(manifests_dir, "capability_defaults.json")
}

fn load_cached_json_vec<T>(manifests_dir: &Path, filename: &str) -> Vec<T>
where
    T: for<'de> Deserialize<'de>,
{
    let path = manifests_dir.join(filename);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_str::<Vec<T>>(&content) {
        Ok(items) => items,
        Err(error) => {
            warn!(file = %path.display(), %error, "Failed to parse cached bootstrap file");
            Vec::new()
        }
    }
}

fn remove_legacy_model_cache_files(manifests_dir: &Path) -> Result<()> {
    for filename in ["model_runtime.json", "model_assignments.json"] {
        let path = manifests_dir.join(filename);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| {
                format!("Failed to remove legacy cache file {}", path.display())
            })?;
        }
    }
    Ok(())
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

    check_section!("routines", Vec<BootstrapRoutineManifest>);
    check_section!("models", Vec<BootstrapModelManifest>);
    check_section!("media_providers", Vec<MediaProviderConfig>);
    check_section!("capability_defaults", Vec<ModelCapabilityDefaultBinding>);
    check_section!("agents", Vec<BootstrapAgentManifest>);
    check_section!("councils", Vec<nenjo::manifest::CouncilManifest>);
    check_section!("domains", Vec<BootstrapDomainManifest>);
    check_section!("projects", Vec<BootstrapProjectManifest>);
    check_section!("mcp_servers", Vec<nenjo::manifest::McpServerManifest>);
    check_section!("commands", Vec<BootstrapCommandManifest>);
    check_section!("abilities", Vec<BootstrapAbilityManifest>);
    check_section!("context_blocks", Vec<BootstrapContextBlockManifest>);
}

fn bootstrap_routine_manifest(
    routine: BootstrapRoutineManifest,
) -> nenjo::manifest::RoutineManifest {
    let routine_slug = routine
        .slug
        .clone()
        .unwrap_or_else(|| Slug::derive(&routine.name));
    nenjo::manifest::RoutineManifest {
        name: routine.name,
        slug: routine_slug.clone(),
        description: routine.description,
        trigger: routine.trigger,
        metadata: routine.metadata,
        steps: routine
            .steps
            .into_iter()
            .map(|step| nenjo::manifest::RoutineStepManifest {
                slug: step.slug.unwrap_or_else(|| Slug::derive(&step.name)),
                routine: step.routine.unwrap_or_else(|| routine_slug.clone()),
                name: step.name,
                step_type: step.step_type,
                council: step.council.map(Slug::derive),
                agent: step.agent.map(Slug::derive),
                config: step.config,
                order_index: step.order_index,
            })
            .collect(),
        edges: routine
            .edges
            .into_iter()
            .map(|edge| nenjo::manifest::RoutineEdgeManifest {
                routine: edge.routine.unwrap_or_else(|| routine_slug.clone()),
                source_step: Slug::derive(edge.source_step),
                target_step: Slug::derive(edge.target_step),
                condition: edge.condition,
                metadata: edge.metadata,
            })
            .collect(),
    }
}

pub struct WorkerManifestCache {
    pub manifests_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
}

/// Refreshes the provider snapshot after a worker-local platform mutation.
///
/// The platform backend commits first, then the manifest writer calls this
/// hook before returning control to the tool. This closes the interval where
/// the local cache has changed but the running provider is still stale while
/// waiting for the corresponding manifest event.
#[async_trait::async_trait]
pub(crate) trait ManifestSnapshotRefresher: Send + Sync {
    async fn refresh_provider_manifest(&self) -> Result<()>;
}

/// A two-phase handle used because platform tools are assembled before their
/// owning harness exists. Worker assembly binds it immediately after creating
/// the harness, before the worker accepts any commands.
#[derive(Clone, Default)]
pub(crate) struct ManifestRefreshHandle {
    refresher: Arc<tokio::sync::OnceCell<Arc<dyn ManifestSnapshotRefresher>>>,
}

impl ManifestRefreshHandle {
    pub(crate) fn bind(&self, refresher: Arc<dyn ManifestSnapshotRefresher>) -> Result<()> {
        self.refresher
            .set(refresher)
            .map_err(|_| anyhow!("provider manifest refresher was already bound"))
    }

    async fn refresh_provider_manifest(&self) -> Result<()> {
        let refresher = self.refresher.get().context(
            "provider manifest refresher is unavailable while worker assembly is incomplete",
        )?;
        refresher.refresh_provider_manifest().await
    }
}

/// Manifest reader/writer exposed to worker-local platform tools.
///
/// Core SDK manifests remain readable through the generic local store, while
/// the platform remains authoritative for every mutation. The writer refreshes
/// the canonical bootstrap cache and the running provider snapshot instead of
/// applying a potentially incomplete tool response to local JSON files.
#[derive(Clone)]
pub struct WorkerManifestStore {
    cache: Arc<WorkerManifestCache>,
    manifest_refresher: ManifestRefreshHandle,
}

impl WorkerManifestStore {
    pub(crate) fn new(
        cache: Arc<WorkerManifestCache>,
        manifest_refresher: ManifestRefreshHandle,
    ) -> Self {
        Self {
            cache,
            manifest_refresher,
        }
    }

    pub fn root(&self) -> &Path {
        &self.cache.manifests_dir
    }

    fn local_store(&self) -> LocalManifestStore {
        LocalManifestStore::new(&self.cache.manifests_dir)
    }

    fn persist_noncanonical_resources(&self, manifest: &Manifest) -> Result<()> {
        let store = self.local_store();
        for kind in [
            ManifestResourceKind::Project,
            ManifestResourceKind::Routine,
            ManifestResourceKind::Council,
            ManifestResourceKind::Domain,
            ManifestResourceKind::McpServer,
            ManifestResourceKind::Ability,
            ManifestResourceKind::ContextBlock,
            ManifestResourceKind::Skill,
            ManifestResourceKind::Command,
            ManifestResourceKind::Hook,
            ManifestResourceKind::ScriptTool,
            ManifestResourceKind::KnowledgePack,
        ] {
            store.persist_resource_kind(manifest, kind)?;
        }
        Ok(())
    }

    async fn refresh_provider_manifest(&self) -> Result<()> {
        self.manifest_refresher.refresh_provider_manifest().await
    }
}

impl WorkerManifestCache {
    /// Upsert one configured model in the canonical `models.json` cache.
    pub fn upsert_model(&self, model: &CachedModelManifest) -> Result<()> {
        let mut models = load_cached_models(&self.manifests_dir);
        if let Some(position) = models.iter().position(|existing| existing.id == model.id) {
            models[position] = model.clone();
        } else {
            models.push(model.clone());
        }
        atomic_write_json(&self.manifests_dir, "models.json", &models)
    }

    /// Remove a configured model from the canonical `models.json` cache.
    pub fn remove_model(&self, model_id: Uuid) -> Result<()> {
        let mut models = load_cached_models(&self.manifests_dir);
        models.retain(|model| model.id != model_id);
        atomic_write_json(&self.manifests_dir, "models.json", &models)
    }

    fn persist_agent_assignment_snapshot(
        &self,
        agent_id: Uuid,
        assignments: Vec<ModelAssignmentBinding>,
    ) -> Result<()> {
        let mut agents =
            load_cached_json_vec::<CachedAgentManifest>(&self.manifests_dir, "agents.json");
        let agent = agents
            .iter_mut()
            .find(|agent| agent.id == agent_id)
            .ok_or_else(|| anyhow!("agent {} is missing from the cached manifest", agent_id))?;
        agent.model_assignments = assignments;
        atomic_write_json(&self.manifests_dir, "agents.json", &agents)
    }

    fn persist_capability_default_snapshot(
        &self,
        defaults: Vec<ModelCapabilityDefaultBinding>,
    ) -> Result<()> {
        atomic_write_json(&self.manifests_dir, "capability_defaults.json", &defaults)
    }

    fn persist_manifest_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        let manifests_dir = &self.manifests_dir;
        match resource_type {
            ResourceType::Model => Ok(()),
            ResourceType::Agent => self.persist_agents(manifest),
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
            ResourceType::Command => {
                atomic_write_json(manifests_dir, "commands.json", &manifest.commands)
            }
            ResourceType::ContextBlock => sync_tree(
                &manifests_dir.join("context_blocks"),
                &manifest.context_blocks,
            ),
            ResourceType::McpServer => {
                atomic_write_json(manifests_dir, "mcp_servers.json", &manifest.mcp_servers)
            }
            ResourceType::Domain => sync_tree(&manifests_dir.join("domains"), &manifest.domains),
            ResourceType::ModelAssignment | ResourceType::ModelCapabilityDefault => Ok(()),
            ResourceType::Document => Ok(()),
            ResourceType::KnowledgePack => atomic_write_json(
                manifests_dir,
                "knowledge_packs.json",
                &manifest.knowledge_packs,
            ),
        }
    }

    /// Apply the resource-specific portion of a manifest event to the
    /// canonical bootstrap cache.
    ///
    /// The generic [`ManifestStore`] contract persists manifest resources from
    /// a completed `Manifest`. These three cache entries contain platform ids
    /// and routing data that are intentionally not part of that core type, so
    /// their event snapshots are applied here at the worker boundary.
    pub(crate) fn persist_manifest_event(
        &self,
        resource_type: ResourceType,
        action: ResourceAction,
        resource_id: Option<Uuid>,
        resource: &nenjo::Slug,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        match resource_type {
            ResourceType::Model => {
                self.persist_model_change(action, resource_id, resource, payload)
            }
            ResourceType::ModelAssignment => {
                self.persist_model_assignment_change(action, resource_id, resource, payload)
            }
            ResourceType::ModelCapabilityDefault => {
                self.persist_capability_default_change(action, payload)
            }
            _ => Ok(()),
        }
    }

    fn persist_model_change(
        &self,
        action: ResourceAction,
        resource_id: Option<Uuid>,
        resource: &nenjo::Slug,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        if action == ResourceAction::Deleted {
            let Some(model_id) = resource_id else {
                warn!(%resource, "Model delete event is missing a model id");
                return Ok(());
            };
            return self.remove_model(model_id);
        }

        let Some(record) = payload
            .and_then(ManifestResourcePayload::<ModelRecord>::parse)
            .map(|payload| payload.data)
        else {
            warn!(%resource, "Model update is missing a valid inline snapshot");
            return Ok(());
        };
        self.upsert_model(&CachedModelManifest {
            id: record.id,
            manifest: record.to_manifest(),
            capabilities: record
                .capabilities
                .iter()
                .map(|capability| capability.as_str().trim().to_owned())
                .filter(|capability| !capability.is_empty())
                .collect(),
        })
    }

    fn persist_model_assignment_change(
        &self,
        action: ResourceAction,
        resource_id: Option<Uuid>,
        resource: &nenjo::Slug,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        let update = if action == ResourceAction::Deleted {
            let Some(agent_id) = resource_id else {
                warn!(%resource, "Model assignment delete event is missing an agent id");
                return Ok(());
            };
            ModelAssignmentsManifestUpdate {
                agent_id,
                assignments: Vec::new(),
            }
        } else {
            let Some(update) = payload
                .and_then(ManifestResourcePayload::<ModelAssignmentsManifestUpdate>::parse)
                .map(|payload| payload.data)
            else {
                warn!(%resource, ?action, "Model assignment update is missing a valid inline snapshot");
                return Ok(());
            };
            if let Some(event_agent_id) = resource_id
                && event_agent_id != update.agent_id
            {
                warn!(
                    %resource,
                    %event_agent_id,
                    update_agent_id = %update.agent_id,
                    "Model assignment update agent id does not match its event"
                );
                return Ok(());
            }
            update
        };
        self.persist_agent_assignment_snapshot(update.agent_id, update.assignments)
    }

    fn persist_capability_default_change(
        &self,
        action: ResourceAction,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        let defaults = if action == ResourceAction::Deleted {
            Vec::new()
        } else {
            let Some(update) = payload
                .and_then(ManifestResourcePayload::<ModelCapabilityDefaultsManifestUpdate>::parse)
                .map(|payload| payload.data)
            else {
                warn!(
                    ?action,
                    "Capability defaults update is missing a valid inline snapshot"
                );
                return Ok(());
            };
            update.defaults
        };
        self.persist_capability_default_snapshot(defaults)
    }

    fn persist_agents(&self, manifest: &nenjo::Manifest) -> Result<()> {
        let existing =
            load_cached_json_vec::<CachedAgentManifest>(&self.manifests_dir, "agents.json");
        let ids = PlatformResourceIdStore::new(&self.manifests_dir).load()?;
        let agents = manifest
            .agents
            .iter()
            .map(|manifest| {
                let existing = existing
                    .iter()
                    .find(|agent| agent.manifest.slug == manifest.slug);
                let id = existing
                    .map(|agent| agent.id)
                    .or_else(|| ids.get(PlatformResourceKind::Agent, &manifest.slug))
                    .ok_or_else(|| anyhow!("agent '{}' is missing a platform id", manifest.slug))?;
                Ok(CachedAgentManifest {
                    id,
                    manifest: manifest.clone(),
                    model_assignments: existing
                        .map(|agent| agent.model_assignments.clone())
                        .unwrap_or_default(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        atomic_write_json(&self.manifests_dir, "agents.json", &agents)
    }

    pub async fn full_refresh(&self, api: &ApiClient) -> Result<nenjo::Manifest> {
        sync(api, &self.manifests_dir, &self.state_dir, &self.config_dir).await?;
        let loader = nenjo::LocalManifestStore::new(&self.manifests_dir);
        nenjo::ManifestLoader::load(&loader).await
    }

    /// Rebuild the canonical cache after a successful platform write.
    ///
    /// Unlike startup refreshes, this does not hide a failed bootstrap fetch:
    /// the tool must not claim success while its provider still sees the old
    /// configuration.
    pub async fn refresh_after_platform_write(&self, api: &ApiClient) -> Result<nenjo::Manifest> {
        sync_required(api, &self.manifests_dir, &self.state_dir, &self.config_dir).await?;
        let loader = nenjo::LocalManifestStore::new(&self.manifests_dir);
        nenjo::ManifestLoader::load(&loader).await
    }

    fn knowledge_pack_dir(&self, metadata: &KnowledgeDocumentRecord) -> PathBuf {
        self.library_root().join(metadata.pack_slug.trim())
    }

    fn library_root(&self) -> PathBuf {
        self.config_dir.join("library")
    }
}

#[async_trait::async_trait]
impl ManifestStore for WorkerManifestCache {
    async fn persist_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        self.persist_manifest_resource(manifest, resource_type)
    }

    async fn remove_resource(
        &self,
        manifest: &nenjo::Manifest,
        resource_type: ResourceType,
        _resource: &nenjo::Slug,
    ) -> Result<()> {
        self.persist_manifest_resource(manifest, resource_type)
    }

    async fn cleanup_deleted_resource(
        &self,
        resource_type: ResourceType,
        resource: &nenjo::Slug,
        _resource_id: Option<Uuid>,
        _payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        if resource_type != ResourceType::KnowledgePack {
            return Ok(());
        }
        let pack_dir = self.library_root().join(resource.as_str());
        if pack_dir.exists() {
            std::fs::remove_dir_all(&pack_dir).with_context(|| {
                format!(
                    "Failed to remove knowledge pack cache {}",
                    pack_dir.display()
                )
            })?;
        }
        PlatformResourceIdStore::new(&self.manifests_dir).remove_knowledge_pack(resource)?;
        Ok(())
    }

    async fn full_refresh(&self, client: &ApiClient) -> Result<nenjo::Manifest> {
        WorkerManifestCache::full_refresh(self, client).await
    }

    async fn update_platform_resource_id(
        &self,
        kind: PlatformResourceKind,
        resource: &nenjo::Slug,
        resource_id: Option<Uuid>,
    ) -> Result<()> {
        let store = PlatformResourceIdStore::new(&self.manifests_dir);
        match resource_id {
            Some(id) => store.upsert(kind, resource, id),
            None => store.remove(kind, resource),
        }
    }

    async fn remove_platform_resource_id_by_id(
        &self,
        kind: PlatformResourceKind,
        resource_id: Uuid,
    ) -> Result<()> {
        PlatformResourceIdStore::new(&self.manifests_dir).remove_by_id(kind, resource_id)
    }

    async fn update_knowledge_document_resource_id(
        &self,
        pack: &nenjo::Slug,
        doc: &nenjo::Slug,
        resource_id: Option<Uuid>,
    ) -> Result<()> {
        let store = PlatformResourceIdStore::new(&self.manifests_dir);
        match resource_id {
            Some(id) => store.upsert_knowledge_document(pack, doc, id),
            None => store.remove_knowledge_document(pack, doc),
        }
    }

    async fn remove_knowledge_document_resource_id_by_id(&self, resource_id: Uuid) -> Result<()> {
        PlatformResourceIdStore::new(&self.manifests_dir)
            .remove_knowledge_document_by_id(resource_id)
    }

    async fn sync_document_metadata(
        &self,
        client: &ApiClient,
        doc: &nenjo::Slug,
        metadata: Option<&KnowledgeDocumentRecord>,
        edges: Option<crate::handlers::manifest::DocumentEdgesSource<'_>>,
    ) -> Result<()> {
        let Some(meta) = metadata else {
            return Ok(());
        };
        let pack_dir = self.knowledge_pack_dir(meta);
        crate::local_documents::sync_document_metadata(
            client,
            &pack_dir,
            doc,
            &self.manifests_dir,
            metadata,
            edges,
        )
        .await
    }

    async fn sync_document(
        &self,
        client: &ApiClient,
        doc: &nenjo::Slug,
        metadata: Option<&KnowledgeDocumentRecord>,
    ) -> Result<()> {
        let Some(meta) = metadata else {
            return Ok(());
        };
        let pack_dir = self.knowledge_pack_dir(meta);
        crate::local_documents::sync_document(
            client,
            &pack_dir,
            doc,
            &self.state_dir,
            &self.manifests_dir,
            metadata,
        )
        .await
    }

    async fn remove_document(
        &self,
        doc: &nenjo::Slug,
        metadata: Option<&KnowledgeDocumentRecord>,
    ) -> Result<()> {
        if let Some(meta) = metadata {
            let pack_dir = self.knowledge_pack_dir(meta);
            crate::local_documents::remove_manifest_document_from_pack_dir(
                &pack_dir,
                doc,
                Some(meta),
            )
        } else {
            Ok(())
        }
    }

    async fn sync_knowledge_pack(&self, client: &ApiClient, pack: &nenjo::Slug) -> Result<()> {
        crate::local_documents::sync_pack_by_slug(
            client,
            &self.config_dir,
            &self.state_dir,
            &self.manifests_dir,
            pack,
        )
        .await
    }

    fn write_document_content(
        &self,
        pack: &nenjo::Slug,
        relative_path: &str,
        content: &str,
    ) -> Result<()> {
        let pack_dir = self.library_root().join(pack.as_str());
        crate::local_documents::write_document_content(&pack_dir, relative_path, content)
    }
}

#[async_trait::async_trait]
impl ManifestReader for WorkerManifestStore {
    async fn load_manifest(&self) -> Result<Manifest> {
        self.local_store().load_manifest().await
    }

    async fn list_agents(&self) -> Result<Vec<nenjo::manifest::AgentManifest>> {
        self.local_store().list_agents().await
    }

    async fn get_agent(&self, slug: &Slug) -> Result<Option<nenjo::manifest::AgentManifest>> {
        self.local_store().get_agent(slug).await
    }

    async fn list_models(&self) -> Result<Vec<nenjo::manifest::ModelManifest>> {
        self.local_store().list_models().await
    }

    async fn get_model(&self, slug: &Slug) -> Result<Option<nenjo::manifest::ModelManifest>> {
        self.local_store().get_model(slug).await
    }

    async fn list_routines(&self) -> Result<Vec<nenjo::manifest::RoutineManifest>> {
        self.local_store().list_routines().await
    }

    async fn get_routine(&self, slug: &Slug) -> Result<Option<nenjo::manifest::RoutineManifest>> {
        self.local_store().get_routine(slug).await
    }

    async fn list_projects(&self) -> Result<Vec<nenjo::manifest::ProjectManifest>> {
        self.local_store().list_projects().await
    }

    async fn get_project(&self, slug: &Slug) -> Result<Option<nenjo::manifest::ProjectManifest>> {
        self.local_store().get_project(slug).await
    }

    async fn list_councils(&self) -> Result<Vec<nenjo::manifest::CouncilManifest>> {
        self.local_store().list_councils().await
    }

    async fn get_council(&self, slug: &Slug) -> Result<Option<nenjo::manifest::CouncilManifest>> {
        self.local_store().get_council(slug).await
    }

    async fn list_domains(&self) -> Result<Vec<nenjo::manifest::DomainManifest>> {
        self.local_store().list_domains().await
    }

    async fn get_domain(&self, slug: &Slug) -> Result<Option<nenjo::manifest::DomainManifest>> {
        self.local_store().get_domain(slug).await
    }

    async fn list_mcp_servers(&self) -> Result<Vec<nenjo::manifest::McpServerManifest>> {
        self.local_store().list_mcp_servers().await
    }

    async fn get_mcp_server(
        &self,
        slug: &Slug,
    ) -> Result<Option<nenjo::manifest::McpServerManifest>> {
        self.local_store().get_mcp_server(slug).await
    }

    async fn list_abilities(&self) -> Result<Vec<nenjo::manifest::AbilityManifest>> {
        self.local_store().list_abilities().await
    }

    async fn get_ability(&self, slug: &Slug) -> Result<Option<nenjo::manifest::AbilityManifest>> {
        self.local_store().get_ability(slug).await
    }

    async fn list_context_blocks(&self) -> Result<Vec<nenjo::manifest::ContextBlockManifest>> {
        self.local_store().list_context_blocks().await
    }

    async fn get_context_block(
        &self,
        slug: &Slug,
    ) -> Result<Option<nenjo::manifest::ContextBlockManifest>> {
        self.local_store().get_context_block(slug).await
    }
}

#[async_trait::async_trait]
impl ManifestWriter for WorkerManifestStore {
    async fn replace_manifest(&self, manifest: &Manifest) -> Result<()> {
        let current = self.load_manifest().await?;
        if serde_json::to_value(&manifest.models)? != serde_json::to_value(&current.models)?
            || serde_json::to_value(&manifest.agents)? != serde_json::to_value(&current.agents)?
        {
            anyhow::bail!(
                "worker manifest store cannot replace configured models or agents; refresh bootstrap instead"
            );
        }
        self.persist_noncanonical_resources(manifest)
    }

    async fn upsert_resource(&self, resource: &ManifestResource) -> Result<ManifestResource> {
        self.refresh_provider_manifest().await?;
        Ok(resource.clone())
    }

    async fn cache_resource(&self, resource: &ManifestResource) -> Result<ManifestResource> {
        // Models and agents carry worker-owned bootstrap metadata (platform
        // ids, capabilities, and model assignments). A read-through response
        // lacks that envelope, so only bootstrap may update those entries.
        match resource.kind() {
            ManifestResourceKind::Model | ManifestResourceKind::Agent => Ok(resource.clone()),
            _ => self.local_store().upsert_resource(resource).await,
        }
    }

    async fn delete_resource(&self, kind: ManifestResourceKind, slug: &Slug) -> Result<()> {
        let _ = (kind, slug);
        self.refresh_provider_manifest().await
    }
}

async fn ensure_worker_ack(
    api: &ApiClient,
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
    api: &ApiClient,
    agent: &BootstrapAgentManifest,
    state_dir: &Path,
) -> Result<PromptConfig> {
    let agent_slug = agent
        .slug
        .as_ref()
        .cloned()
        .unwrap_or_else(|| Slug::derive(&agent.name));
    let Some(payload) = agent.encrypted_payload.as_ref() else {
        let Some(response) = api.fetch_agent_prompt_config(&agent_slug).await? else {
            return Ok(PromptConfig::default());
        };

        if let Some(payload) = response.agent.encrypted_payload.as_ref() {
            return decrypt_prompt_config_payload(payload, state_dir, agent.id).await;
        }

        return Ok(response.agent.prompt_config.unwrap_or_default());
    };

    decrypt_prompt_config_payload(payload, state_dir, agent.id).await
}

async fn decrypt_prompt_config_payload(
    payload: &EncryptedPayload,
    state_dir: &Path,
    agent_id: Uuid,
) -> Result<PromptConfig> {
    if payload.object_type != SensitiveContentKind::AgentPrompt.encrypted_object_type() {
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

async fn resolve_bootstrap_ability_prompt_config(
    ability: &BootstrapAbilityManifest,
    state_dir: &Path,
) -> Result<nenjo::manifest::AbilityPromptConfig> {
    let Some(payload) = ability.encrypted_payload.as_ref() else {
        return Ok(ability.manifest.prompt_config.clone());
    };

    decrypt_ability_prompt_config_payload(payload, state_dir, ability.id).await
}

async fn decrypt_ability_prompt_config_payload(
    payload: &EncryptedPayload,
    state_dir: &Path,
    ability_id: Uuid,
) -> Result<nenjo::manifest::AbilityPromptConfig> {
    if payload.object_type != SensitiveContentKind::AbilityPrompt.encrypted_object_type() {
        anyhow::bail!(
            "Unsupported encrypted bootstrap payload type '{}' for ability {}",
            payload.object_type,
            ability_id
        );
    }

    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("Failed to load worker auth provider for bootstrap ability prompt decrypt")?;
    let plaintext = decrypt_text_with_provider(&auth_provider, payload)
        .await
        .with_context(|| {
            format!(
                "Failed to decrypt bootstrap ability prompt payload for ability {}",
                ability_id
            )
        })?;

    serde_json::from_str::<nenjo::manifest::AbilityPromptConfig>(&plaintext).with_context(|| {
        format!(
            "Failed to parse decrypted bootstrap ability prompt config JSON for ability {}",
            ability_id
        )
    })
}

async fn resolve_bootstrap_domain_prompt_config(
    domain: &BootstrapDomainManifest,
    state_dir: &Path,
) -> Result<nenjo::manifest::DomainPromptConfig> {
    let Some(payload) = domain.encrypted_payload.as_ref() else {
        return Ok(domain.manifest.prompt_config.clone());
    };

    decrypt_domain_prompt_config_payload(payload, state_dir, domain.id).await
}

async fn decrypt_domain_prompt_config_payload(
    payload: &EncryptedPayload,
    state_dir: &Path,
    domain_id: Uuid,
) -> Result<nenjo::manifest::DomainPromptConfig> {
    if payload.object_type != SensitiveContentKind::DomainPrompt.encrypted_object_type() {
        anyhow::bail!(
            "Unsupported encrypted bootstrap payload type '{}' for domain {}",
            payload.object_type,
            domain_id
        );
    }

    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("Failed to load worker auth provider for bootstrap domain prompt decrypt")?;
    let plaintext = decrypt_text_with_provider(&auth_provider, payload)
        .await
        .with_context(|| {
            format!(
                "Failed to decrypt bootstrap domain prompt payload for domain {}",
                domain_id
            )
        })?;

    serde_json::from_str::<nenjo::manifest::DomainPromptConfig>(&plaintext).with_context(|| {
        format!(
            "Failed to parse decrypted bootstrap domain prompt config JSON for domain {}",
            domain_id
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
    if payload.object_type != SensitiveContentKind::ContextBlockContent.encrypted_object_type() {
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

async fn resolve_bootstrap_command_content(
    command: &BootstrapCommandManifest,
    state_dir: &Path,
) -> Result<String> {
    let Some(payload) = command.encrypted_payload.as_ref() else {
        return Ok(command.manifest.content.clone());
    };

    decrypt_command_content_payload(payload, state_dir, command.id).await
}

async fn decrypt_command_content_payload(
    payload: &EncryptedPayload,
    state_dir: &Path,
    command_id: Uuid,
) -> Result<String> {
    if payload.object_type != SensitiveContentKind::CommandContent.encrypted_object_type() {
        anyhow::bail!(
            "Unsupported encrypted bootstrap payload type '{}' for command {}",
            payload.object_type,
            command_id
        );
    }

    if payload.object_id != command_id {
        anyhow::bail!(
            "Encrypted bootstrap command content object_id {} did not match command {}",
            payload.object_id,
            command_id
        );
    }

    let auth_provider = WorkerAuthProvider::load_or_create(state_dir.join("crypto"))
        .context("Failed to load worker auth provider for bootstrap command decrypt")?;
    let plaintext = decrypt_text_with_provider(&auth_provider, payload)
        .await
        .with_context(|| {
            format!(
                "Failed to decrypt bootstrap command content payload for command {}",
                command_id
            )
        })?;
    Ok(serde_json::from_str::<String>(&plaintext).unwrap_or(plaintext))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct RecordingManifestRefresher {
        refreshes: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ManifestSnapshotRefresher for RecordingManifestRefresher {
        async fn refresh_provider_manifest(&self) -> Result<()> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn bootstrap_model_keeps_catalog_context_window() {
        let model: BootstrapModelManifest = serde_json::from_value(serde_json::json!({
            "id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "name": "Catalog model",
            "slug": "openrouter-openai-gpt-4-1",
            "model": "openai/gpt-4.1",
            "model_provider": "openrouter",
            "context_window": 1_000_000,
        }))
        .unwrap();

        assert_eq!(model.manifest.context_window, Some(1_000_000));
    }

    #[test]
    fn bootstrap_agent_assignments_deserialize_with_configured_model_ids() {
        let assignment_id = Uuid::from_u128(1);
        let model_id = Uuid::from_u128(2);
        let bootstrap: BootstrapManifestResponse = serde_json::from_value(serde_json::json!({
            "auth": {
                "user_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "org_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
            },
            "agents": [{
                "agent_id": assignment_id,
                "id": assignment_id,
                "name": "voice-agent",
                "model_assignments": [{
                    "capability": "transcribe_audio",
                    "model_id": model_id,
                    "assignment_source": "local"
                }]
            }],
            "capability_defaults": [{
                "capability": "generate_speech",
                "model_id": model_id
            }]
        }))
        .unwrap();

        let [agent] = bootstrap.agents.as_slice() else {
            panic!("expected one agent")
        };
        let [assignment] = agent.model_assignments.as_slice() else {
            panic!("expected one model assignment")
        };
        assert_eq!(assignment.model_id, model_id);
        assert_eq!(assignment.capability, "transcribe_audio");
        let [default] = bootstrap.capability_defaults.as_slice() else {
            panic!("expected one capability default")
        };
        assert_eq!(default.model_id, model_id);
    }

    #[tokio::test]
    async fn manifest_event_cache_updates_only_the_target_assignment_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let cache = WorkerManifestCache {
            manifests_dir: root.path().join("manifests"),
            workspace_dir: root.path().join("workspace"),
            state_dir: root.path().join("state"),
            config_dir: root.path().join("config"),
        };
        let agent_id = Uuid::from_u128(1);
        let other_agent_id = Uuid::from_u128(2);
        atomic_write_json(
            &cache.manifests_dir,
            "agents.json",
            &vec![
                cached_agent(
                    agent_id,
                    "voice-agent",
                    vec![ModelAssignmentBinding {
                        capability: "transcribe_audio".into(),
                        model_id: Uuid::from_u128(3),
                        assignment_source: "local".into(),
                    }],
                ),
                cached_agent(
                    other_agent_id,
                    "other-agent",
                    vec![ModelAssignmentBinding {
                        capability: "generate_image".into(),
                        model_id: Uuid::from_u128(4),
                        assignment_source: "local".into(),
                    }],
                ),
            ],
        )
        .unwrap();

        let assignment_resource = Slug::derive("voice-agent");
        let assignment_payload = ManifestResourcePayload::new(ModelAssignmentsManifestUpdate {
            agent_id,
            assignments: vec![nenjo_events::ModelAssignmentBinding {
                capability: "transcribe_audio".into(),
                model_id: Uuid::from_u128(5),
                assignment_source: "local".into(),
            }],
        })
        .into_value();
        cache
            .persist_manifest_event(
                ResourceType::ModelAssignment,
                ResourceAction::Updated,
                Some(agent_id),
                &assignment_resource,
                Some(&assignment_payload),
            )
            .unwrap();

        let defaults_resource = Slug::derive("organization-defaults");
        let defaults_payload =
            ManifestResourcePayload::new(ModelCapabilityDefaultsManifestUpdate {
                defaults: vec![nenjo_events::ModelCapabilityDefaultBinding {
                    capability: "generate_speech".into(),
                    model_id: Uuid::from_u128(6),
                }],
            })
            .into_value();
        cache
            .persist_manifest_event(
                ResourceType::ModelCapabilityDefault,
                ResourceAction::Updated,
                None,
                &defaults_resource,
                Some(&defaults_payload),
            )
            .unwrap();

        let assignments = load_cached_agent_model_assignments(&cache.manifests_dir);
        assert_eq!(assignments.len(), 2);
        assert!(assignments.iter().any(|agent| {
            agent.agent_id == other_agent_id && agent.assignments[0].model_id == Uuid::from_u128(4)
        }));
        assert!(assignments.iter().any(|agent| {
            agent.agent_id == agent_id && agent.assignments[0].model_id == Uuid::from_u128(5)
        }));
        let defaults = load_cached_capability_defaults(&cache.manifests_dir);
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].model_id, Uuid::from_u128(6));
    }

    #[test]
    fn inline_model_updates_replace_the_canonical_model_entry() {
        let root = tempfile::tempdir().unwrap();
        let cache = WorkerManifestCache {
            manifests_dir: root.path().join("manifests"),
            workspace_dir: root.path().join("workspace"),
            state_dir: root.path().join("state"),
            config_dir: root.path().join("config"),
        };
        let model_id = Uuid::from_u128(7);
        atomic_write_json(
            &cache.manifests_dir,
            "models.json",
            &vec![CachedModelManifest {
                id: model_id,
                manifest: nenjo::manifest::ModelManifest {
                    slug: Slug::derive("old-model"),
                    name: "old-model".into(),
                    description: None,
                    model: "old-model-id".into(),
                    model_provider: "openai-compatible".into(),
                    temperature: None,
                    context_window: None,
                    base_url: Some("http://127.0.0.1:8080/v1".into()),
                    native_tools: Vec::new(),
                },
                capabilities: vec!["chat".into()],
            }],
        )
        .unwrap();

        cache
            .upsert_model(&CachedModelManifest {
                id: model_id,
                manifest: nenjo::manifest::ModelManifest {
                    slug: Slug::derive("updated-model"),
                    name: "updated-model".into(),
                    description: None,
                    model: "updated-model-id".into(),
                    model_provider: "openai-compatible".into(),
                    temperature: None,
                    context_window: None,
                    base_url: Some("http://127.0.0.1:11434/v1".into()),
                    native_tools: Vec::new(),
                },
                capabilities: vec!["chat".into(), "transcribe_audio".into()],
            })
            .unwrap();

        let runtime = load_cached_model_runtime(&cache.manifests_dir);
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].id, model_id);
        assert_eq!(runtime[0].slug, Slug::derive("updated-model"));
        assert_eq!(
            runtime[0].base_url.as_deref(),
            Some("http://127.0.0.1:11434/v1")
        );
        assert_eq!(runtime[0].capabilities, ["chat", "transcribe_audio"]);

        cache.remove_model(model_id).unwrap();
        assert!(load_cached_model_runtime(&cache.manifests_dir).is_empty());
    }

    #[test]
    fn bootstrap_cache_drops_legacy_model_sidecars() {
        let root = tempfile::tempdir().unwrap();
        let manifests_dir = root.path().join("manifests");
        fs::create_dir_all(&manifests_dir).unwrap();
        fs::write(manifests_dir.join("model_runtime.json"), "[]").unwrap();
        fs::write(manifests_dir.join("model_assignments.json"), "[]").unwrap();

        remove_legacy_model_cache_files(&manifests_dir).unwrap();

        assert!(!manifests_dir.join("model_runtime.json").exists());
        assert!(!manifests_dir.join("model_assignments.json").exists());
    }

    #[tokio::test]
    async fn worker_manifest_store_never_rewrites_canonical_model_or_agent_envelopes() {
        let root = tempfile::tempdir().unwrap();
        let cache = Arc::new(WorkerManifestCache {
            manifests_dir: root.path().join("manifests"),
            workspace_dir: root.path().join("workspace"),
            state_dir: root.path().join("state"),
            config_dir: root.path().join("config"),
        });
        let model_id = Uuid::from_u128(1);
        let agent_id = Uuid::from_u128(2);
        atomic_write_json(
            &cache.manifests_dir,
            "models.json",
            &vec![CachedModelManifest {
                id: model_id,
                manifest: nenjo::manifest::ModelManifest {
                    slug: Slug::derive("speech-model"),
                    name: "speech model".into(),
                    description: None,
                    model: "speech-model".into(),
                    model_provider: "openai-compatible".into(),
                    temperature: None,
                    context_window: None,
                    base_url: Some("http://127.0.0.1:8080/v1".into()),
                    native_tools: Vec::new(),
                },
                capabilities: vec!["transcribe_audio".into()],
            }],
        )
        .unwrap();
        atomic_write_json(
            &cache.manifests_dir,
            "agents.json",
            &vec![cached_agent(
                agent_id,
                "voice-agent",
                vec![ModelAssignmentBinding {
                    capability: "transcribe_audio".into(),
                    model_id,
                    assignment_source: "local".into(),
                }],
            )],
        )
        .unwrap();

        let store = WorkerManifestStore::new(cache.clone(), ManifestRefreshHandle::default());
        let manifest = store.load_manifest().await.unwrap();
        store.replace_manifest(&manifest).await.unwrap();

        let models = load_cached_models(&cache.manifests_dir);
        let [model] = models.as_slice() else {
            panic!("expected canonical model cache entry")
        };
        assert_eq!(model.id, model_id);
        assert_eq!(model.capabilities, ["transcribe_audio"]);
        let agents = load_cached_agent_model_assignments(&cache.manifests_dir);
        let [agent] = agents.as_slice() else {
            panic!("expected canonical agent cache entry")
        };
        assert_eq!(agent.agent_id, agent_id);
        assert_eq!(agent.assignments[0].model_id, model_id);

        let mut incompatible = manifest;
        incompatible.models[0].name = "not a canonical update".into();
        assert!(store.replace_manifest(&incompatible).await.is_err());
    }

    #[tokio::test]
    async fn worker_manifest_store_refreshes_provider_after_every_direct_mutation() {
        let root = tempfile::tempdir().unwrap();
        let cache = Arc::new(WorkerManifestCache {
            manifests_dir: root.path().join("manifests"),
            workspace_dir: root.path().join("workspace"),
            state_dir: root.path().join("state"),
            config_dir: root.path().join("config"),
        });
        let refresher = Arc::new(RecordingManifestRefresher::default());
        let refresh_handle = ManifestRefreshHandle::default();
        refresh_handle.bind(refresher.clone()).unwrap();
        let store = WorkerManifestStore::new(cache.clone(), refresh_handle);
        let project = nenjo::manifest::ProjectManifest {
            name: "Live project".into(),
            slug: Slug::derive("live-project"),
            description: None,
            settings: serde_json::json!({}),
        };
        let model = nenjo::manifest::ModelManifest {
            slug: Slug::derive("live-model"),
            name: "Live model".into(),
            description: None,
            model: "live-model".into(),
            model_provider: "openai-compatible".into(),
            temperature: None,
            context_window: None,
            base_url: None,
            native_tools: Vec::new(),
        };

        store
            .upsert_resource(&ManifestResource::Project(project))
            .await
            .unwrap();
        store
            .upsert_resource(&ManifestResource::Model(model))
            .await
            .unwrap();
        store
            .delete_resource(ManifestResourceKind::Project, &Slug::derive("live-project"))
            .await
            .unwrap();

        assert_eq!(refresher.refreshes.load(Ordering::SeqCst), 3);
        assert!(store.load_manifest().await.unwrap().projects.is_empty());

        store
            .cache_resource(&ManifestResource::Project(
                nenjo::manifest::ProjectManifest {
                    name: "Read-through project".into(),
                    slug: Slug::derive("read-through-project"),
                    description: None,
                    settings: serde_json::json!({}),
                },
            ))
            .await
            .unwrap();
        assert_eq!(refresher.refreshes.load(Ordering::SeqCst), 3);
        assert_eq!(
            store.load_manifest().await.unwrap().projects[0].slug,
            Slug::derive("read-through-project")
        );
    }

    fn cached_agent(
        id: Uuid,
        name: &str,
        model_assignments: Vec<ModelAssignmentBinding>,
    ) -> CachedAgentManifest {
        CachedAgentManifest {
            id,
            manifest: nenjo::manifest::AgentManifest {
                name: name.into(),
                slug: Slug::derive(name),
                description: None,
                prompt_config: PromptConfig::default(),
                color: None,
                model: None,
                domains: Vec::new(),
                platform_scopes: Vec::new(),
                mcp_servers: Vec::new(),
                script_tools: Vec::new(),
                media: Vec::new(),
                abilities: Vec::new(),
                prompt_locked: false,
                heartbeat: None,
                source_type: None,
                metadata: serde_json::json!({}),
            },
            model_assignments,
        }
    }

    #[tokio::test]
    async fn sync_platform_packages_writes_lockfile_and_installs_locked_tree() {
        let package_root = tempfile::tempdir().unwrap();
        write_test_package(package_root.path());
        let nenpm_yml = test_nenpm_yml(package_root.path());
        let nenpm_lock_yml = build_test_lockfile(&nenpm_yml);
        let nenjo_home = tempfile::tempdir().unwrap();
        let packages = BootstrapPackages {
            schema: "nenjo.platform_packages.v1".to_string(),
            nenpm_yml,
            nenpm_lock_yml,
            argument_bindings: vec![PackageArgumentBindingUpdate {
                package: "@acme/core".to_string(),
                name: "shop_name".to_string(),
                selector: "args.shop.name".to_string(),
                value_type: "text".to_string(),
                value: "Acme Auto".to_string(),
            }],
        };

        let status = sync_platform_packages(nenjo_home.path(), &packages)
            .await
            .unwrap();
        assert_eq!(status, PlatformPackageSyncStatus::Applied);

        let root = nenjo_home.path().join("platform_pkgs");
        assert!(root.join("nenpm.yml").exists());
        assert!(root.join("nenpm.lock.yml").exists());
        assert!(root.join("argument_bindings.json").exists());
        let argument_bindings = fs::read_to_string(root.join("argument_bindings.json")).unwrap();
        assert!(argument_bindings.contains("Acme Auto"));
        assert!(root.join("@acme/core@0.1.0/context.yaml").exists());
        assert!(root.join(".nenpm-index.json").exists());
    }

    #[tokio::test]
    async fn sync_platform_packages_empty_graph_prunes_cached_packages() {
        let package_root = tempfile::tempdir().unwrap();
        write_test_package(package_root.path());
        let nenpm_yml = test_nenpm_yml(package_root.path());
        let nenpm_lock_yml = build_test_lockfile(&nenpm_yml);
        let nenjo_home = tempfile::tempdir().unwrap();
        let packages = BootstrapPackages {
            schema: "nenjo.platform_packages.v1".to_string(),
            nenpm_yml,
            nenpm_lock_yml,
            argument_bindings: Vec::new(),
        };
        sync_platform_packages(nenjo_home.path(), &packages)
            .await
            .unwrap();

        let root = nenjo_home.path().join("platform_pkgs");
        assert!(root.join("@acme/core@0.1.0/context.yaml").exists());

        let empty_nenpm_yml = empty_test_nenpm_yml();
        let empty_packages = BootstrapPackages {
            schema: "nenjo.platform_packages.v1".to_string(),
            nenpm_lock_yml: build_test_lockfile(&empty_nenpm_yml),
            nenpm_yml: empty_nenpm_yml,
            argument_bindings: Vec::new(),
        };
        let status = sync_platform_packages(nenjo_home.path(), &empty_packages)
            .await
            .unwrap();

        assert_eq!(status, PlatformPackageSyncStatus::Applied);
        assert!(root.join("nenpm.yml").exists());
        assert!(root.join("nenpm.lock.yml").exists());
        assert!(root.join("argument_bindings.json").exists());
        assert!(root.join(".nenpm-index.json").exists());
        assert!(!root.join("@acme/core@0.1.0/context.yaml").exists());
        assert!(!root.join("@acme").exists());
    }

    #[tokio::test]
    async fn sync_platform_packages_failed_update_preserves_existing_cache() {
        let package_root = tempfile::tempdir().unwrap();
        write_test_package(package_root.path());
        let initial_nenpm_yml = test_nenpm_yml(package_root.path());
        let initial_nenpm_lock_yml = build_test_lockfile(&initial_nenpm_yml);
        let nenjo_home = tempfile::tempdir().unwrap();
        let packages = BootstrapPackages {
            schema: "nenjo.platform_packages.v1".to_string(),
            nenpm_yml: initial_nenpm_yml.clone(),
            nenpm_lock_yml: initial_nenpm_lock_yml.clone(),
            argument_bindings: Vec::new(),
        };
        sync_platform_packages(nenjo_home.path(), &packages)
            .await
            .unwrap();

        let root = nenjo_home.path().join("platform_pkgs");
        assert!(root.join("@acme/core@0.1.0/context.yaml").exists());

        let missing_root = tempfile::tempdir().unwrap();
        let bad_nenpm_yml = format!(
            r#"
schema: nenjo.dependencies.v1

dependencies:
  "@acme/missing": "0.1.0"

overrides:
  "@acme/missing":
    kind: local
    root: {}
    manifest_path: nenjo.package.yaml
"#,
            missing_root.path().join("does-not-exist").display()
        );
        let bad_packages = BootstrapPackages {
            schema: "nenjo.platform_packages.v1".to_string(),
            nenpm_yml: bad_nenpm_yml,
            nenpm_lock_yml: initial_nenpm_lock_yml.clone(),
            argument_bindings: Vec::new(),
        };

        let error = sync_platform_packages(nenjo_home.path(), &bad_packages)
            .await
            .expect_err("bad platform package update should fail");
        assert!(
            error
                .to_string()
                .contains("failed to install platform packages")
        );
        assert_eq!(
            fs::read_to_string(root.join("nenpm.yml")).unwrap(),
            initial_nenpm_yml
        );
        assert_eq!(
            fs::read_to_string(root.join("nenpm.lock.yml")).unwrap(),
            initial_nenpm_lock_yml
        );
        assert!(root.join("@acme/core@0.1.0/context.yaml").exists());
    }

    #[tokio::test]
    async fn sync_platform_packages_installs_external_registry_dependencies() {
        let core_root = tempfile::tempdir().unwrap();
        write_test_file(
            core_root.path(),
            "packages.yaml",
            r#"
schema: nenjo.registry.v1
packages:
  core: packages/core/nenjo.package.yaml
"#,
        );
        write_test_file(
            core_root.path(),
            "packages/core/nenjo.package.yaml",
            r#"
schema: nenjo.package.v1
name: core
version: "0.1.0"
modules:
  - path: context/methodology.yaml
"#,
        );
        write_test_file(
            core_root.path(),
            "packages/core/context/methodology.yaml",
            r#"
schema: nenjo.context_block.v1
manifest:
  name: methodology
  template: Use the external methodology.
"#,
        );

        let app_root = tempfile::tempdir().unwrap();
        write_test_file(
            app_root.path(),
            "packages.yaml",
            &format!(
                r#"
schema: nenjo.registry.v1
registries:
  - kind: local
    root: {}
    manifest_path: packages.yaml
    scope: "@bar"
packages:
  app: packages/app/nenjo.package.yaml
"#,
                core_root.path().display()
            ),
        );
        write_test_file(
            app_root.path(),
            "packages/app/nenjo.package.yaml",
            r#"
schema: nenjo.package.v1
name: app
version: "0.1.0"
dependencies:
  "@bar/core": "^0.1.0"
modules:
  - path: agents/app.yaml
"#,
        );
        write_test_file(
            app_root.path(),
            "packages/app/agents/app.yaml",
            r#"
schema: nenjo.agent.v1
manifest:
  name: App Agent
"#,
        );

        let nenpm_yml = format!(
            r#"
schema: nenjo.dependencies.v1

dependencies:
  "@foo/app": "0.1.0"
  "@bar/core": "0.1.0"

registries:
  - kind: local
    root: {}
    manifest_path: packages.yaml
    scope: "@foo"
"#,
            app_root.path().display()
        );
        let nenpm_lock_yml = build_test_lockfile(&nenpm_yml);
        let nenjo_home = tempfile::tempdir().unwrap();
        let packages = BootstrapPackages {
            schema: "nenjo.platform_packages.v1".to_string(),
            nenpm_yml,
            nenpm_lock_yml,
            argument_bindings: Vec::new(),
        };

        let status = sync_platform_packages(nenjo_home.path(), &packages)
            .await
            .unwrap();
        assert_eq!(status, PlatformPackageSyncStatus::Applied);

        let root = nenjo_home.path().join("platform_pkgs");
        assert!(
            root.join("@bar/core@0.1.0/context/methodology.yaml")
                .exists()
        );
        assert!(root.join("@foo/app@0.1.0/agents/app.yaml").exists());
        assert!(root.join(".nenpm-index.json").exists());
    }

    fn write_test_package(root: &Path) {
        fs::write(
            root.join("nenjo.package.yaml"),
            r#"
schema: nenjo.package.v1
name: "@acme/core"
version: "0.1.0"
modules:
  - context.yaml
"#,
        )
        .unwrap();
        fs::write(
            root.join("context.yaml"),
            r#"
schema: nenjo.context_block.v1
manifest:
  name: core_context
  template: Use the core context.
"#,
        )
        .unwrap();
    }

    fn write_test_file(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn test_nenpm_yml(package_root: &Path) -> String {
        format!(
            r#"
schema: nenjo.dependencies.v1

dependencies:
  "@acme/core": "0.1.0"

overrides:
  "@acme/core":
    kind: local
    root: {}
    manifest_path: nenjo.package.yaml
"#,
            package_root.display()
        )
    }

    fn empty_test_nenpm_yml() -> String {
        r#"
schema: nenjo.dependencies.v1

dependencies: {}
"#
        .to_string()
    }

    fn build_test_lockfile(nenpm_yml: &str) -> String {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("nenpm.yml"), nenpm_yml).unwrap();
        let report = nenjo_nenpm::install(nenjo_nenpm::InstallOptions::new(project.path()))
            .expect("test lockfile should install");
        serde_yaml::to_string(&report.lockfile).unwrap()
    }
}
