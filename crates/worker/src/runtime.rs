//! Worker runtime — the process orchestration layer.
//!
//! Boots the system (bootstrap → Provider), listens for events on the event bus,
//! routes commands to the Provider, and streams results back. Manages active
//! execution handles for cancellation and lifecycle tracking.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use nenjo_crypto_auth::WrappedAccountContentKey as AuthWrappedAccountContentKey;
use nenjo_events::WrappedAccountContentKey as EventWrappedAccountContentKey;
use nenjo_secure_envelope::SecureEnvelopeBus;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::api_client::ApiClient;
use crate::assembly::{WorkerAssembly, WorkerHarness, WorkerProvider};
use crate::bootstrap::WorkerManifestCache;
use crate::config::{Config, SessionConfig};
use crate::crypto::WorkerAuthProvider;
use crate::event_loop::{self, ResponseSender, SeenMessageIds, WorkerEventLoopContext};
use crate::external_mcp::ExternalMcpPool;
use crate::handlers::cron::WorkerCronHarnessExt;
use crate::handlers::crypto::AccountKeyStore;
use crate::handlers::heartbeat::{HeartbeatRestoreRequest, WorkerHeartbeatHarnessExt};
use crate::providers::ModelProviderRegistry;
use crate::sessions::{
    CronSessionRecovery, DomainSessionRecovery, HeartbeatSessionRecovery,
    WorkerSessionRecoveryHandler, WorkerSessionRuntime, WorkerSessionStores,
};
use crate::skills::SkillRegistry;

pub(crate) struct WorkerAccountKeyStore {
    pub auth_provider: Arc<WorkerAuthProvider>,
}

#[async_trait::async_trait]
impl AccountKeyStore for WorkerAccountKeyStore {
    async fn store_user_ack(
        &self,
        user_id: Uuid,
        wrapped_ack: EventWrappedAccountContentKey,
    ) -> anyhow::Result<()> {
        self.auth_provider
            .store_user_ack(
                user_id,
                AuthWrappedAccountContentKey {
                    key_version: wrapped_ack.key_version,
                    algorithm: wrapped_ack.algorithm,
                    ephemeral_public_key: wrapped_ack.ephemeral_public_key,
                    nonce: wrapped_ack.nonce,
                    ciphertext: wrapped_ack.ciphertext,
                    created_at: wrapped_ack.created_at,
                },
            )
            .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared types used by handlers
// ---------------------------------------------------------------------------

pub use nenjo_harness::domain::{DomainRegistry, DomainSession};
pub use nenjo_harness::registry::{ActiveExecution, ExecutionKind, ExecutionRegistry};

/// Per-repo mutexes used to serialize worker git operations.
pub type GitLocks = Arc<DashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>>;

/// Shared context passed to each command handler.
///
/// Handlers use `harness` for provider-backed command execution.
/// Responses are sent via `response_tx` (never touch the bus directly).
pub struct CommandContext {
    pub harness: WorkerHarness,
    pub api: Arc<ApiClient>,
    pub provider_registry: Arc<ModelProviderRegistry>,
    pub actor_user_id: Uuid,
    pub response_tx: ResponseSender,
    pub org_response_tx: ResponseSender,
    pub auth_provider: Arc<WorkerAuthProvider>,
    pub external_mcp: Arc<ExternalMcpPool>,
    pub skill_registry: Arc<SkillRegistry>,
    pub worker_name: String,
    pub config: Config,
    pub domains: DomainRegistry<WorkerProvider>,
    pub git_locks: GitLocks,
    pub manifest_cache: Arc<WorkerManifestCache>,
    pub manifest_change_lock: Arc<tokio::sync::Mutex<()>>,
}

// ---------------------------------------------------------------------------
// Worker runtime
// ---------------------------------------------------------------------------

/// Worker process runtime around a `nenjo_harness::Harness`.
pub struct WorkerRuntime {
    harness: WorkerHarness,
    config: Config,
    api: Arc<ApiClient>,
    provider_registry: Arc<ModelProviderRegistry>,
    auth_provider: Arc<WorkerAuthProvider>,
    external_mcp: Arc<ExternalMcpPool>,
    skill_registry: Arc<SkillRegistry>,
    worker_name: String,
    session_runtime: WorkerSessionRuntime,
    git_locks: GitLocks,
    manifest_cache: Arc<WorkerManifestCache>,
    manifest_change_lock: Arc<tokio::sync::Mutex<()>>,
    seen_message_ids: SeenMessageIds,
    shutdown: CancellationToken,
}

struct RuntimeSessionRecoveryHandler {
    ctx: CommandContext,
}

#[async_trait::async_trait]
impl WorkerSessionRecoveryHandler for RuntimeSessionRecoveryHandler {
    async fn restore_domain_session(&self, request: DomainSessionRecovery) -> Result<()> {
        let agent = nenjo::Slug::parse(&request.agent)?;
        let project = request
            .project
            .as_deref()
            .map(nenjo::Slug::parse)
            .transpose()?;
        let session = self
            .ctx
            .harness
            .rebuild_domain_session(request.session_id, agent, project, &request.domain_command)
            .await?;
        self.ctx.domains.insert(request.session_id, session);
        Ok(())
    }

    async fn restore_cron_session(&self, request: CronSessionRecovery) -> Result<()> {
        let Some(routine) = request.routine.as_deref() else {
            return Ok(());
        };
        let routine = nenjo::Slug::parse(routine)?;
        let project = request
            .project
            .as_deref()
            .map(nenjo::Slug::parse)
            .transpose()?;
        let project = project.as_ref().map(|slug| slug.as_str());
        let task: Option<nenjo_events::CronTaskContent> =
            request.task.map(serde_json::from_value).transpose()?;
        self.ctx
            .harness
            .handle_cron_enable(
                &self.ctx.cron_context(),
                crate::handlers::cron::CronEnableRequest {
                    routine: routine.as_str(),
                    project,
                    schedule: &request.schedule_expr,
                    timezone: request.timezone.as_deref(),
                    task_content: task,
                    start_at: request.next_run_at,
                },
            )
            .await?;
        Ok(())
    }

    async fn restore_heartbeat_session(&self, request: HeartbeatSessionRecovery) -> Result<()> {
        let agent = nenjo::Slug::parse(&request.agent)?;
        let manifest = self.ctx.harness.provider().manifest_snapshot();
        let agent_id = manifest
            .agents
            .iter()
            .any(|item| item.slug == agent)
            .then(|| crate::resource_resolver::stable_resource_id("agent", &agent))
            .ok_or_else(|| anyhow::anyhow!("agent not found: {agent}"))?;
        self.ctx
            .harness
            .restore_agent_heartbeat(
                &self.ctx.heartbeat_context(),
                HeartbeatRestoreRequest {
                    agent_id,
                    interval: request.interval,
                    timezone: request.timezone,
                    start_at: request.next_run_at,
                    instructions: request.instructions,
                    previous_output_ref: request.previous_output_ref,
                    last_run_at: request.last_run_at,
                    start_paused: request.start_paused,
                },
            )
            .await?;
        Ok(())
    }
}

impl WorkerRuntime {
    /// Construct a worker runtime from already assembled dependencies.
    pub(crate) async fn from_assembly(assembly: WorkerAssembly, config: Config) -> Result<Self> {
        let seen_message_ids = event_loop::new_seen_message_ids();
        let shutdown = CancellationToken::new();
        let worker_name = assembly.session_runtime.host_id().to_string();
        configure_session_cleanup(
            config.sessions.clone(),
            assembly.session_stores.clone(),
            shutdown.clone(),
        );

        Ok(Self {
            harness: assembly.harness,
            config,
            api: Arc::new(assembly.api),
            provider_registry: assembly.provider_registry,
            auth_provider: assembly.auth_provider,
            external_mcp: assembly.external_mcp,
            skill_registry: assembly.skill_registry,
            worker_name,
            session_runtime: assembly.session_runtime,
            git_locks: Arc::new(DashMap::new()),
            manifest_cache: assembly.manifest_cache,
            manifest_change_lock: assembly.manifest_change_lock,
            seen_message_ids,
            shutdown,
        })
    }

    pub(crate) fn command_context(
        &self,
        actor_user_id: Uuid,
        response_tx: ResponseSender,
        org_response_tx: ResponseSender,
    ) -> CommandContext {
        CommandContext {
            harness: self.harness.clone(),
            api: self.api.clone(),
            provider_registry: self.provider_registry.clone(),
            actor_user_id,
            response_tx,
            org_response_tx,
            auth_provider: self.auth_provider.clone(),
            external_mcp: self.external_mcp.clone(),
            skill_registry: self.skill_registry.clone(),
            worker_name: self.worker_name.clone(),
            config: self.config.clone(),
            domains: self.harness.domains(),
            git_locks: self.git_locks.clone(),
            manifest_cache: self.manifest_cache.clone(),
            manifest_change_lock: self.manifest_change_lock.clone(),
        }
    }

    pub(crate) async fn recover_reconcilable_sessions(&self, restore_ctx: CommandContext) {
        let handler = RuntimeSessionRecoveryHandler { ctx: restore_ctx };
        if let Err(error) = self
            .session_runtime
            .recover_reconcilable_sessions(&handler)
            .await
        {
            warn!(%error, "Failed to recover reconcilable sessions");
        }
    }

    /// Get the current Provider (lock-free).
    pub fn provider(&self) -> Arc<WorkerProvider> {
        self.harness.provider()
    }

    pub fn api(&self) -> Arc<ApiClient> {
        self.api.clone()
    }

    /// Get a handle that can trigger shutdown from another task.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Signal all active executions to stop and shut down the event loop.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Run the event loop until shutdown.
    pub async fn run<T>(&self, bus: SecureEnvelopeBus<T>, ctx: WorkerEventLoopContext) -> Result<()>
    where
        T: nenjo_eventbus::Transport + 'static,
    {
        event_loop::run(self, bus, ctx).await
    }

    pub(crate) fn seen_message_ids(&self) -> SeenMessageIds {
        self.seen_message_ids.clone()
    }

    pub(crate) fn cancel_active_executions(&self) {
        let executions = self.harness.executions();
        for entry in executions.iter() {
            entry.value().cancel.cancel();
        }
        executions.clear();
    }
}

fn configure_session_cleanup(
    config: SessionConfig,
    stores: WorkerSessionStores,
    shutdown: CancellationToken,
) {
    if !config.cleanup_enabled {
        return;
    }

    if config.cleanup_on_startup {
        run_session_cleanup(&stores, config.retention_days, "startup");
    }

    if config.cleanup_interval_hours == 0 {
        return;
    }

    tokio::spawn(async move {
        let interval_duration = Duration::from_secs(
            config
                .cleanup_interval_hours
                .saturating_mul(60)
                .saturating_mul(60),
        );
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(interval_duration) => {
                    run_session_cleanup(&stores, config.retention_days, "periodic");
                }
            }
        }
    });
}

fn run_session_cleanup(stores: &WorkerSessionStores, retention_days: u64, reason: &str) {
    match stores.prune_terminal_sessions(retention_days) {
        Ok(report) => info!(
            reason,
            scanned = report.scanned,
            deleted = report.deleted,
            retained = report.retained,
            retention_days,
            "Cleaned file-backed session state"
        ),
        Err(error) => warn!(
            reason,
            error = %error,
            retention_days,
            "Failed to clean file-backed session state"
        ),
    }
}

#[cfg(test)]
mod tests {
    use crate::assembly::{ProviderBuildContext, WorkerAssembly, build_provider};
    use crate::bootstrap::ManifestRefreshHandle;
    use crate::config::Config;
    use crate::crypto::WorkerAuthProvider;
    use crate::external_mcp::ExternalMcpPool;
    use crate::providers::ModelProviderRegistry;
    use crate::sessions::{WorkerSessionRuntime, WorkerSessionStores};
    use crate::skills::SkillRegistry;
    use nenjo::LocalManifestStore;
    use nenjo_platform::api_client::ApiClient;
    use std::sync::Arc;
    use tempfile::tempdir;

    use super::{WorkerManifestCache, WorkerRuntime};

    #[tokio::test]
    async fn worker_runtime_constructs_from_assembly() {
        let temp = tempdir().unwrap();
        let config = Config {
            config_dir: temp.path().join("config"),
            workspace_dir: temp.path().join("workspace"),
            state_dir: temp.path().join("state"),
            manifests_dir: temp.path().join("manifests"),
            backend_api_url: Some("http://localhost:3001".to_string()),
            api_key: "test-api-key".to_string(),
            ..Default::default()
        };
        let auth_provider =
            Arc::new(WorkerAuthProvider::load_or_create(temp.path().join("crypto")).unwrap());
        let api = ApiClient::new(config.backend_api_url(), &config.api_key);
        let external_mcp = Arc::new(ExternalMcpPool::new());
        let skill_registry = Arc::new(SkillRegistry::default());
        let provider_registry = Arc::new(ModelProviderRegistry::new(
            &config.model_provider_api_keys,
            &config.reliability,
        ));
        let manifest_cache = Arc::new(WorkerManifestCache {
            manifests_dir: config.manifests_dir.clone(),
            workspace_dir: config.workspace_dir.clone(),
            state_dir: config.state_dir.clone(),
            config_dir: config.config_dir.clone(),
        });
        let manifest_change_lock = Arc::new(tokio::sync::Mutex::new(()));
        let expected_manifest_cache = manifest_cache.clone();
        let expected_manifest_change_lock = manifest_change_lock.clone();
        let provider = build_provider(ProviderBuildContext {
            config: &config,
            loader: LocalManifestStore::new(&config.manifests_dir),
            auth_provider: auth_provider.clone(),
            external_mcp: external_mcp.clone(),
            skill_registry: skill_registry.clone(),
            provider_registry: provider_registry.clone(),
            manifest_cache: manifest_cache.clone(),
            manifest_refresh: ManifestRefreshHandle::default(),
        })
        .await
        .unwrap();
        let session_stores = WorkerSessionStores::new(&config.state_dir);
        let session_runtime =
            WorkerSessionRuntime::with_host(session_stores.clone(), "embedded-worker");
        let harness = nenjo_harness::Harness::builder(provider)
            .with_session_runtime(session_runtime.clone())
            .build();

        let runtime = WorkerRuntime::from_assembly(
            WorkerAssembly {
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
            },
            config,
        )
        .await
        .unwrap();

        assert_eq!(runtime.worker_name, "embedded-worker");
        assert!(runtime.provider().manifest().agents.is_empty());
        assert!(Arc::ptr_eq(
            &runtime.manifest_cache,
            &expected_manifest_cache
        ));
        assert!(Arc::ptr_eq(
            &runtime.manifest_change_lock,
            &expected_manifest_change_lock
        ));
    }
}
