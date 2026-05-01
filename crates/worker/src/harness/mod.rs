//! Harness — the main orchestration layer.
//!
//! Boots the system (bootstrap → Provider), listens for events on the event bus,
//! routes commands to the Provider, and streams results back. Manages active
//! execution handles for cancellation and lifecycle tracking.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use dashmap::DashMap;
use nenjo_sessions::{ScheduleState, SessionKind, SessionRecord, SessionStatus};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::crypto::WorkerAuthProvider;
use nenjo::Provider;
use nenjo_events::{Response, StreamEvent};
use nenjo_secure_envelope::{DecodingError, ReceivedInput, SecureEnvelopeBus};

pub mod agent;
pub mod config;
pub mod doc_sync;
pub mod execution_trace;
pub mod external_mcp;
pub mod handlers;
pub mod loader;
pub mod manifest;
pub mod preview;
pub mod prompt;
pub mod providers;
pub mod security;
pub mod session;
pub mod stream;
pub mod tools;

pub use nenjo::client as api_client;

use api_client::NenjoClient;
use config::Config;
use external_mcp::ExternalMcpPool;
use loader::FileSystemManifestLoader;
use providers::registry::ProviderRegistry;
use session::local_content::FileSessionContentStore;
use session::local_coordinator::LocalSessionCoordinator;
use session::local_store::FileSessionStore;
use session::reconcile_recoverable_session;
use tools::{HarnessToolFactory, NativeRuntime};

#[derive(Debug, Clone)]
struct RoutedResponse {
    user_id: Uuid,
    response: Response,
}

#[derive(Clone)]
pub struct ResponseSender {
    tx: tokio::sync::mpsc::UnboundedSender<RoutedResponse>,
    user_id: Uuid,
}

impl ResponseSender {
    fn new(tx: tokio::sync::mpsc::UnboundedSender<RoutedResponse>, user_id: Uuid) -> Self {
        Self { tx, user_id }
    }

    pub fn send(&self, response: Response) -> Result<(), ()> {
        self.tx
            .send(RoutedResponse {
                user_id: self.user_id,
                response,
            })
            .map_err(|_| ())
    }
}

fn response_for_decode_failure(failure: &DecodingError) -> Option<Response> {
    let session_id = failure.session_id?;
    Some(Response::AgentResponse {
        session_id: Some(session_id),
        payload: StreamEvent::Error {
            message: "Execution failed".to_string(),
            payload: Some(json!({
                "code": failure.code,
                "message": failure.message,
            })),
            encrypted_payload: None,
        },
    })
}

// ---------------------------------------------------------------------------
// Shared types used by handlers
// ---------------------------------------------------------------------------

/// What kind of execution this is (for targeted cancellation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionKind {
    Chat,
    Task,
    Cron,
    Heartbeat,
}

/// Tracks an active execution so it can be cancelled or paused.
pub struct ActiveExecution {
    pub kind: ExecutionKind,
    pub registry_token: uuid::Uuid,
    pub execution_run_id: Option<uuid::Uuid>,
    pub cancel: CancellationToken,
    pub pause: Option<nenjo::agents::runner::types::PauseToken>,
}

/// Thread-safe registry of active executions, keyed by a cancel key.
pub type ExecutionRegistry = Arc<DashMap<Uuid, ActiveExecution>>;

/// An active domain session — holds the domain-expanded runner and state.
pub struct DomainSession {
    pub runner: nenjo::AgentRunner,
    pub agent_id: Uuid,
    pub project_id: Uuid,
    pub domain_command: String,
    pub turn_number: u32,
}

/// Thread-safe registry of active domain sessions, keyed by `domain_session_id`.
pub type DomainRegistry = Arc<DashMap<Uuid, DomainSession>>;

/// Per-repo mutex to serialize git worktree operations (add/remove).
/// Git's `.git/config` lock does not support concurrent writes.
pub type GitLocks = Arc<DashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>>;

/// Shared context passed to each command handler.
///
/// Handlers load the current Provider from `provider` (lock-free via ArcSwap).
/// Responses are sent via `response_tx` (never touch the bus directly).
pub struct CommandContext {
    pub provider: Arc<ArcSwap<Provider>>,
    pub actor_user_id: Uuid,
    pub response_tx: ResponseSender,
    pub auth_provider: Arc<WorkerAuthProvider>,
    pub worker_id: String,
    pub session_store: Arc<dyn nenjo_sessions::SessionStore>,
    pub session_content: Arc<dyn nenjo_sessions::SessionContentStore>,
    pub session_coordinator: Arc<dyn nenjo_sessions::SessionCoordinator>,
    pub api: Arc<NenjoClient>,
    pub config: Config,
    pub external_mcp: Arc<ExternalMcpPool>,
    pub executions: ExecutionRegistry,
    pub domains: DomainRegistry,
    pub git_locks: GitLocks,
}

impl CommandContext {
    /// Load the current Provider snapshot (lock-free).
    pub fn provider(&self) -> Arc<Provider> {
        self.provider.load_full()
    }

    /// Swap the Provider with a new one (for manifest changes).
    pub fn swap_provider(&self, new: Provider) {
        self.provider.store(Arc::new(new));
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The Nenjo agent harness.
pub struct Harness {
    provider: Arc<ArcSwap<Provider>>,
    config: Config,
    api: Arc<NenjoClient>,
    auth_provider: Arc<WorkerAuthProvider>,
    worker_id: String,
    session_store: Arc<dyn nenjo_sessions::SessionStore>,
    session_content: Arc<dyn nenjo_sessions::SessionContentStore>,
    session_coordinator: Arc<dyn nenjo_sessions::SessionCoordinator>,
    external_mcp: Arc<ExternalMcpPool>,
    executions: ExecutionRegistry,
    domains: DomainRegistry,
    git_locks: GitLocks,
    shutdown: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchedulerRestoreAction {
    Cron {
        session_id: Uuid,
        project_id: Option<Uuid>,
        schedule_expr: String,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    Heartbeat {
        session_id: Uuid,
        interval: Duration,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
        previous_output_ref: Option<String>,
        last_run_at: Option<chrono::DateTime<chrono::Utc>>,
        start_paused: bool,
    },
}

fn scheduler_restore_action(record: &SessionRecord) -> Option<SchedulerRestoreAction> {
    if !matches!(record.status, SessionStatus::Active | SessionStatus::Paused) {
        return None;
    }

    match record.scheduler.clone()? {
        ScheduleState::Cron(state) => {
            if record.status != SessionStatus::Active {
                return None;
            }
            Some(SchedulerRestoreAction::Cron {
                session_id: record.session_id,
                project_id: record.project_id,
                schedule_expr: state.schedule_expr,
                next_run_at: state.next_run_at,
            })
        }
        ScheduleState::Heartbeat(state) => Some(SchedulerRestoreAction::Heartbeat {
            session_id: record.session_id,
            interval: Duration::from_secs(state.interval_secs.max(1)),
            next_run_at: state.next_run_at,
            previous_output_ref: state.previous_output_ref,
            last_run_at: state.last_run_at,
            start_paused: record.status == SessionStatus::Paused,
        }),
    }
}

impl Harness {
    pub(crate) async fn rebuild_domain_session(
        provider: &Arc<ArcSwap<Provider>>,
        session_id: Uuid,
        agent_id: Uuid,
        project_id: Uuid,
        domain_command: &str,
        turn_number: u32,
    ) -> Result<DomainSession> {
        let provider_snapshot = provider.load_full();
        let base_runner = provider_snapshot
            .agent_by_id(agent_id)
            .await?
            .build()
            .await?;
        let domain_runner = base_runner.domain_expansion(domain_command).await?;

        let mut instance = domain_runner.instance().clone();
        if let Some(ref mut active_domain) = instance.prompt_context.active_domain {
            active_domain.session_id = session_id;
        }

        let runner = nenjo::AgentRunner::from_instance(
            instance,
            domain_runner.memory().cloned(),
            domain_runner.memory_scope().cloned(),
        );

        Ok(DomainSession {
            runner,
            agent_id,
            project_id,
            domain_command: domain_command.to_string(),
            turn_number,
        })
    }

    async fn restore_domain_sessions(
        provider: &Arc<ArcSwap<Provider>>,
        session_store: &Arc<dyn nenjo_sessions::SessionStore>,
        domains: &DomainRegistry,
    ) {
        for persisted in session_store
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|record| {
                record.kind == SessionKind::Domain
                    && matches!(record.status, SessionStatus::Active | SessionStatus::Paused)
            })
        {
            let Some(domain) = persisted.domain.clone() else {
                continue;
            };
            let Some(agent_id) = persisted.agent_id else {
                continue;
            };
            let Some(project_id) = persisted.project_id else {
                continue;
            };

            match Self::rebuild_domain_session(
                provider,
                persisted.session_id,
                agent_id,
                project_id,
                &domain.domain_command,
                domain.turn_number,
            )
            .await
            {
                Ok(session) => {
                    domains.insert(persisted.session_id, session);
                }
                Err(e) => {
                    warn!(session_id = %persisted.session_id, error = %e, "Failed to rebuild persisted domain session");
                    let _ = session_store.delete(persisted.session_id);
                }
            }
        }
    }

    fn reconcile_recoverable_sessions(
        session_store: &Arc<dyn nenjo_sessions::SessionStore>,
        session_content: &Arc<dyn nenjo_sessions::SessionContentStore>,
        session_coordinator: &Arc<dyn nenjo_sessions::SessionCoordinator>,
    ) {
        for persisted in session_store
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|record| {
                matches!(record.kind, SessionKind::Chat | SessionKind::Task)
                    && matches!(record.status, SessionStatus::Active | SessionStatus::Paused)
            })
        {
            if let Err(e) = reconcile_recoverable_session(
                &**session_store,
                &**session_content,
                &**session_coordinator,
                persisted.session_id,
            ) {
                warn!(
                    session_id = %persisted.session_id,
                    error = %e,
                    "Failed to reconcile recoverable session state"
                );
            }
        }
    }

    fn restore_context(&self, response_tx: ResponseSender) -> CommandContext {
        CommandContext {
            provider: self.provider.clone(),
            actor_user_id: Uuid::nil(),
            response_tx,
            auth_provider: self.auth_provider.clone(),
            worker_id: self.worker_id.clone(),
            session_store: self.session_store.clone(),
            session_content: self.session_content.clone(),
            session_coordinator: self.session_coordinator.clone(),
            api: self.api.clone(),
            config: self.config.clone(),
            external_mcp: self.external_mcp.clone(),
            executions: self.executions.clone(),
            domains: self.domains.clone(),
            git_locks: self.git_locks.clone(),
        }
    }

    async fn restore_scheduler_sessions(&self, restore_ctx: &CommandContext) {
        for persisted in self.session_store.list().unwrap_or_default().into_iter() {
            match scheduler_restore_action(&persisted) {
                Some(SchedulerRestoreAction::Cron {
                    session_id,
                    project_id,
                    schedule_expr,
                    next_run_at,
                }) => {
                    if let Err(e) = crate::harness::handlers::cron::handle_cron_enable(
                        restore_ctx,
                        session_id,
                        project_id,
                        &schedule_expr,
                        next_run_at,
                    )
                    .await
                    {
                        warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to restore cron schedule"
                        );
                    }
                }
                Some(SchedulerRestoreAction::Heartbeat {
                    session_id,
                    interval,
                    next_run_at,
                    previous_output_ref,
                    last_run_at,
                    start_paused,
                }) => {
                    if let Err(e) = crate::harness::handlers::heartbeat::restore_agent_heartbeat(
                        restore_ctx,
                        session_id,
                        interval,
                        next_run_at,
                        previous_output_ref,
                        last_run_at,
                        start_paused,
                    )
                    .await
                    {
                        warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to restore heartbeat schedule"
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Create a new Harness.
    pub async fn new(config: Config, auth_provider: Arc<WorkerAuthProvider>) -> Result<Self> {
        let api = Arc::new(NenjoClient::new(config.backend_api_url(), &config.api_key));

        manifest::sync(
            &api,
            &config.manifests_dir,
            &config.workspace_dir,
            &config.state_dir,
        )
        .await?;

        let loader = FileSystemManifestLoader::new(&config.manifests_dir);
        let manifest = nenjo::ManifestLoader::load(&loader).await?;

        let mcp_servers = manifest.mcp_servers.clone();

        let external_mcp = Arc::new(ExternalMcpPool::new());
        external_mcp.reconcile(&mcp_servers).await;

        let provider_registry =
            ProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);
        let security = Arc::new(nenjo_tools::security::SecurityPolicy::with_workspace_dir(
            config.workspace_dir.clone(),
        ));
        let runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter> = Arc::new(NativeRuntime);
        let tool_factory =
            HarnessToolFactory::new(security, runtime, config.clone(), external_mcp.clone());

        let memory_dir = config.state_dir.join("memory");
        let mem = nenjo::memory::MarkdownMemory::new(&memory_dir, &config.state_dir);

        let agent_config = config.agent.clone();

        let provider = Provider::builder()
            .with_loader(loader)
            .with_model_factory(provider_registry)
            .with_tool_factory(tool_factory)
            .with_memory(mem)
            .with_agent_config(agent_config)
            .build()
            .await
            .context("Failed to build Provider")?;

        let provider = Arc::new(ArcSwap::from_pointee(provider));
        let worker_id = provider
            .load_full()
            .manifest()
            .auth
            .as_ref()
            .and_then(|auth| auth.api_key_id)
            .map(|id| id.to_string())
            .unwrap_or_else(|| "local-worker".to_string());
        let session_store: Arc<dyn nenjo_sessions::SessionStore> =
            Arc::new(FileSessionStore::new(&config.state_dir.join("sessions")));
        let session_content: Arc<dyn nenjo_sessions::SessionContentStore> = Arc::new(
            FileSessionContentStore::new(&config.state_dir.join("session_content")),
        );
        let session_coordinator: Arc<dyn nenjo_sessions::SessionCoordinator> =
            Arc::new(LocalSessionCoordinator::new());
        let executions = Arc::new(DashMap::new());
        let domains = Arc::new(DashMap::new());
        let git_locks = Arc::new(DashMap::new());
        let shutdown = CancellationToken::new();

        Self::restore_domain_sessions(&provider, &session_store, &domains).await;
        Self::reconcile_recoverable_sessions(
            &session_store,
            &session_content,
            &session_coordinator,
        );

        Ok(Self {
            provider,
            config,
            api,
            auth_provider,
            worker_id,
            session_store,
            session_content,
            session_coordinator,
            external_mcp,
            executions,
            domains,
            git_locks,
            shutdown,
        })
    }

    /// Get the current Provider (lock-free).
    pub fn provider(&self) -> Arc<Provider> {
        self.provider.load_full()
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

    /// Resolve the effective capabilities for this worker.
    ///
    /// Empty config means all capabilities (full runner mode).
    fn resolved_capabilities(&self) -> Vec<nenjo_events::Capability> {
        if self.config.capabilities.is_empty() {
            nenjo_events::Capability::ALL.to_vec()
        } else {
            self.config.capabilities.clone()
        }
    }

    /// Run the event loop until shutdown.
    pub async fn run<T>(&self, mut bus: SecureEnvelopeBus<T>, bootstrap_user_id: Uuid) -> Result<()>
    where
        T: nenjo_eventbus::Transport + 'static,
    {
        let worker_id = bus.transport().worker_id();
        let capabilities = self.resolved_capabilities();

        info!(
            user_id = %bootstrap_user_id,
            %worker_id,
            ?capabilities,
            "Subscribing to eventbus"
        );

        let (response_tx, mut response_rx) =
            tokio::sync::mpsc::unbounded_channel::<RoutedResponse>();
        let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel::<ReceivedInput>();
        let system_response_tx = ResponseSender::new(response_tx.clone(), bootstrap_user_id);

        // Send initial worker registration + heartbeat.
        let app_version = Some(env!("CARGO_PKG_VERSION").to_string());
        let _ = system_response_tx.send(Response::WorkerRegistered {
            worker_id,
            capabilities: capabilities.clone(),
            version: app_version.clone(),
        });
        let _ = system_response_tx.send(Response::WorkerHeartbeat {
            worker_id,
            capabilities: capabilities.clone(),
            version: app_version.clone(),
        });

        let restore_ctx = self.restore_context(system_response_tx.clone());
        self.restore_scheduler_sessions(&restore_ctx).await;

        // Periodic heartbeat task.
        let heartbeat_tx = system_response_tx.clone();
        let heartbeat_shutdown = self.shutdown.clone();
        let heartbeat_caps = capabilities;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if heartbeat_tx.send(Response::WorkerHeartbeat {
                            worker_id,
                            capabilities: heartbeat_caps.clone(),
                            version: app_version.clone(),
                        }).is_err() {
                            break;
                        }
                    }
                    _ = heartbeat_shutdown.cancelled() => break,
                }
            }
        });

        // Bus I/O task: owns the bus, interleaves recv + send.
        let io_shutdown = self.shutdown.clone();
        let io_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    msg = response_rx.recv() => {
                        match msg {
                            Some(routed) => {
                                if let Err(e) = bus.send_response_for(routed.user_id, routed.response).await {
                                    warn!(error = %e, "Failed to send response");
                                }
                            }
                            None => break,
                        }
                    }
                    result = bus.recv_command() => {
                        match result {
                            Ok(Some(item)) => {
                                if command_tx.send(item).is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {
                                warn!("Event bus stream ended");
                                break;
                            }
                            Err(e) => {
                                warn!(error = %e, "Error receiving command");
                            }
                        }
                    }
                    _ = io_shutdown.cancelled() => break,
                }
            }
        });

        info!("Nenjo harness event loop started");

        // Main loop: dispatch commands to independent tasks.
        while let Some(received) = command_rx.recv().await {
            match received {
                ReceivedInput::Command(received) => {
                    let command = received.command.clone();
                    let actor_user_id = received.envelope.user_id;
                    if let Err(e) = received.ack().await {
                        warn!(error = %e, "Failed to ack command");
                    }

                    let ctx = CommandContext {
                        provider: self.provider.clone(),
                        actor_user_id,
                        response_tx: ResponseSender::new(response_tx.clone(), actor_user_id),
                        auth_provider: self.auth_provider.clone(),
                        worker_id: self.worker_id.clone(),
                        session_store: self.session_store.clone(),
                        session_content: self.session_content.clone(),
                        session_coordinator: self.session_coordinator.clone(),
                        api: self.api.clone(),
                        config: self.config.clone(),
                        external_mcp: self.external_mcp.clone(),
                        executions: self.executions.clone(),
                        domains: self.domains.clone(),
                        git_locks: self.git_locks.clone(),
                    };

                    tokio::spawn(async move {
                        if let Err(e) = handlers::route_command(command, ctx).await {
                            error!(error = %e, "Error handling command");
                        }
                    });
                }
                ReceivedInput::DecodeFailure(received) => {
                    let actor_user_id = received.envelope.user_id;
                    let failure = received.failure.clone();
                    if let Err(e) = received.ack().await {
                        warn!(error = %e, "Failed to ack decode failure");
                    }
                    if let Some(response) = response_for_decode_failure(&failure) {
                        let _ = response_tx.send(RoutedResponse {
                            user_id: actor_user_id,
                            response,
                        });
                    } else {
                        warn!(
                            user_id = %actor_user_id,
                            code = failure.code,
                            "Dropping user-facing decode failure without session context"
                        );
                    }
                }
            }
        }

        // Shutdown: cancel all active executions
        for entry in self.executions.iter() {
            entry.value().cancel.cancel();
        }
        self.executions.clear();
        drop(response_tx);
        let _ = io_handle.await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SchedulerRestoreAction, scheduler_restore_action};
    use chrono::Utc;
    use nenjo_sessions::{
        CronScheduleState, HeartbeatScheduleState, ScheduleState, SessionRecord, SessionRefs,
        SessionStatus, SessionSummary,
    };
    use uuid::Uuid;

    fn base_record(status: SessionStatus) -> SessionRecord {
        let now = Utc::now();
        SessionRecord {
            session_id: Uuid::new_v4(),
            kind: nenjo_sessions::SessionKind::CronSchedule,
            status,
            project_id: Some(Uuid::new_v4()),
            agent_id: None,
            task_id: None,
            routine_id: None,
            execution_run_id: None,
            parent_session_id: None,
            version: 0,
            refs: SessionRefs::default(),
            lease: Default::default(),
            scheduler: None,
            domain: None,
            summary: SessionSummary::default(),
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }

    #[test]
    fn scheduler_restore_plans_active_cron() {
        let next_run_at = Some(Utc::now());
        let mut record = base_record(SessionStatus::Active);
        record.scheduler = Some(ScheduleState::Cron(CronScheduleState {
            schedule_expr: "*/5 * * * *".to_string(),
            next_run_at,
            last_run_at: None,
            last_completion: None,
            paused: false,
        }));

        let action = scheduler_restore_action(&record);
        assert!(matches!(
            action,
            Some(SchedulerRestoreAction::Cron {
                session_id,
                project_id,
                schedule_expr,
                next_run_at: planned_next_run_at,
            }) if session_id == record.session_id
                && project_id == record.project_id
                && schedule_expr == "*/5 * * * *"
                && planned_next_run_at == next_run_at
        ));
    }

    #[test]
    fn scheduler_restore_skips_paused_cron() {
        let mut record = base_record(SessionStatus::Paused);
        record.scheduler = Some(ScheduleState::Cron(CronScheduleState {
            schedule_expr: "*/5 * * * *".to_string(),
            next_run_at: Some(Utc::now()),
            last_run_at: None,
            last_completion: None,
            paused: true,
        }));

        assert!(scheduler_restore_action(&record).is_none());
    }

    #[test]
    fn scheduler_restore_plans_paused_heartbeat() {
        let next_run_at = Some(Utc::now());
        let last_run_at = Some(Utc::now());
        let mut record = base_record(SessionStatus::Paused);
        record.kind = nenjo_sessions::SessionKind::HeartbeatSchedule;
        record.scheduler = Some(ScheduleState::Heartbeat(HeartbeatScheduleState {
            interval_secs: 0,
            next_run_at,
            last_run_at,
            previous_output_ref: Some("heartbeat/out.txt".to_string()),
            last_completion: None,
            run_in_progress: false,
            paused: true,
        }));

        let action = scheduler_restore_action(&record);
        assert!(matches!(
            action,
            Some(SchedulerRestoreAction::Heartbeat {
                session_id,
                interval,
                next_run_at: planned_next_run_at,
                previous_output_ref,
                last_run_at: planned_last_run_at,
                start_paused: true,
            }) if session_id == record.session_id
                && interval == std::time::Duration::from_secs(1)
                && planned_next_run_at == next_run_at
                && planned_last_run_at == last_run_at
                && previous_output_ref.as_deref() == Some("heartbeat/out.txt")
        ));
    }

    #[test]
    fn scheduler_restore_skips_non_scheduler_records() {
        let record = base_record(SessionStatus::Active);
        assert!(scheduler_restore_action(&record).is_none());
    }
}
