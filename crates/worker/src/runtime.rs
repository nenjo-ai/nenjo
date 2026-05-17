//! Worker runtime — the process orchestration layer.
//!
//! Boots the system (bootstrap → Provider), listens for events on the event bus,
//! routes commands to the Provider, and streams results back. Manages active
//! execution handles for cancellation and lifecycle tracking.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use nenjo_crypto_auth::WrappedAccountContentKey as AuthWrappedAccountContentKey;
use nenjo_events::WrappedAccountContentKey as EventWrappedAccountContentKey;
use nenjo_harness::handlers::crypto::AccountKeyStore;
use nenjo_harness::handlers::heartbeat::HeartbeatRestoreRequest;
use nenjo_secure_envelope::SecureEnvelopeBus;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::api_client::NenjoClient;
use crate::assembly::{WorkerAssembly, WorkerHarness, WorkerProvider};
use crate::config::{Config, SessionConfig};
use crate::crypto::WorkerAuthProvider;
use crate::event_loop::{self, ResponseSender, SeenMessageIds, WorkerEventLoopContext};
use crate::sessions::{
    CronSessionRecovery, DomainSessionRecovery, HeartbeatSessionRecovery,
    WorkerSessionRecoveryHandler, WorkerSessionRuntime, WorkerSessionStores,
};

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

pub use nenjo_harness::{
    ActiveExecution, DomainRegistry, DomainSession, ExecutionKind, ExecutionRegistry, GitLocks,
};

/// Shared context passed to each command handler.
///
/// Handlers use `harness` for provider-backed command execution.
/// Responses are sent via `response_tx` (never touch the bus directly).
pub struct CommandContext {
    pub harness: WorkerHarness,
    pub actor_user_id: Uuid,
    pub response_tx: ResponseSender,
    pub auth_provider: Arc<WorkerAuthProvider>,
    pub worker_name: String,
    pub config: Config,
    pub domains: DomainRegistry<WorkerProvider>,
    pub git_locks: GitLocks,
}

// ---------------------------------------------------------------------------
// Worker runtime
// ---------------------------------------------------------------------------

/// Worker process runtime around a `nenjo_harness::Harness`.
pub struct WorkerRuntime {
    harness: WorkerHarness,
    config: Config,
    api: Arc<NenjoClient>,
    auth_provider: Arc<WorkerAuthProvider>,
    worker_name: String,
    session_runtime: WorkerSessionRuntime,
    seen_message_ids: SeenMessageIds,
    shutdown: CancellationToken,
}

struct RuntimeSessionRecoveryHandler {
    ctx: CommandContext,
}

#[async_trait::async_trait]
impl WorkerSessionRecoveryHandler for RuntimeSessionRecoveryHandler {
    async fn restore_domain_session(&self, request: DomainSessionRecovery) -> Result<()> {
        let session = self
            .ctx
            .harness
            .rebuild_domain_session(
                request.session_id,
                request.agent_id,
                request.project_id,
                &request.domain_command,
            )
            .await?;
        self.ctx.domains.insert(request.session_id, session);
        Ok(())
    }

    async fn restore_cron_session(&self, request: CronSessionRecovery) -> Result<()> {
        self.ctx
            .harness
            .handle_cron_enable(
                &self.ctx.cron_context(),
                request.session_id,
                request.project_id,
                &request.schedule_expr,
                request.timezone.as_deref(),
                request.next_run_at,
            )
            .await?;
        Ok(())
    }

    async fn restore_heartbeat_session(&self, request: HeartbeatSessionRecovery) -> Result<()> {
        self.ctx
            .harness
            .restore_agent_heartbeat(
                &self.ctx.heartbeat_context(),
                HeartbeatRestoreRequest {
                    agent_id: request.session_id,
                    interval: request.interval,
                    timezone: request.timezone,
                    start_at: request.next_run_at,
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
        let worker_name = assembly.session_runtime.worker_name().to_string();
        configure_session_cleanup(
            config.sessions.clone(),
            assembly.session_stores.clone(),
            shutdown.clone(),
        );

        Ok(Self {
            harness: assembly.harness,
            config,
            api: Arc::new(assembly.api),
            auth_provider: assembly.auth_provider,
            worker_name,
            session_runtime: assembly.session_runtime,
            seen_message_ids,
            shutdown,
        })
    }

    pub(crate) fn command_context(
        &self,
        actor_user_id: Uuid,
        response_tx: ResponseSender,
    ) -> CommandContext {
        CommandContext {
            harness: self.harness.clone(),
            actor_user_id,
            response_tx,
            auth_provider: self.auth_provider.clone(),
            worker_name: self.worker_name.clone(),
            config: self.config.clone(),
            domains: self.harness.domains(),
            git_locks: self.harness.git_locks(),
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

    pub fn api(&self) -> Arc<NenjoClient> {
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
    use crate::assembly::{WorkerAssembly, build_provider};
    use crate::bootstrap::WorkerManifestCache;
    use crate::config::Config;
    use crate::crypto::WorkerAuthProvider;
    use crate::external_mcp::ExternalMcpPool;
    use crate::sessions::{LocalSessionCoordinator, WorkerSessionRuntime, WorkerSessionStores};
    use nenjo::LocalManifestStore;
    use nenjo::client::NenjoClient;
    use std::sync::Arc;
    use tempfile::tempdir;

    use super::WorkerRuntime;

    #[tokio::test]
    async fn worker_runtime_constructs_from_assembly() {
        let temp = tempdir().unwrap();
        let config = Config {
            workspace_dir: temp.path().join("workspace"),
            state_dir: temp.path().join("state"),
            manifests_dir: temp.path().join("manifests"),
            backend_api_url: Some("http://localhost:3001".to_string()),
            api_key: "test-api-key".to_string(),
            ..Default::default()
        };
        let auth_provider =
            Arc::new(WorkerAuthProvider::load_or_create(temp.path().join("crypto")).unwrap());
        let api = NenjoClient::new(config.backend_api_url(), &config.api_key);
        let external_mcp = Arc::new(ExternalMcpPool::new());
        let provider = build_provider(
            &config,
            LocalManifestStore::new(&config.manifests_dir),
            auth_provider.clone(),
            external_mcp.clone(),
        )
        .await
        .unwrap();
        let session_stores = WorkerSessionStores::new(&config.state_dir);
        let session_runtime = WorkerSessionRuntime::new(
            session_stores.clone(),
            LocalSessionCoordinator::new(),
            "embedded-worker",
        );
        let harness = nenjo_harness::Harness::builder(provider)
            .with_session_runtime(session_runtime.clone())
            .with_manifest_client(api.clone())
            .with_manifest_store(WorkerManifestCache {
                manifests_dir: config.manifests_dir.clone(),
                workspace_dir: config.workspace_dir.clone(),
                state_dir: config.state_dir.clone(),
                config_dir: config.config_dir.clone(),
            })
            .with_mcp_runtime(external_mcp.clone())
            .build();

        let runtime = WorkerRuntime::from_assembly(
            WorkerAssembly {
                harness,
                api,
                auth_provider,
                session_runtime,
                session_stores,
                external_mcp,
            },
            config,
        )
        .await
        .unwrap();

        assert_eq!(runtime.worker_name, "embedded-worker");
        assert!(runtime.provider().manifest().agents.is_empty());
    }
}
