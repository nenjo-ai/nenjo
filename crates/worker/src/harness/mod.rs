//! Harness — the main orchestration layer.
//!
//! Boots the system (bootstrap → Provider), listens for events on the event bus,
//! routes commands to the Provider, and streams results back. Manages active
//! execution handles for cancellation and lifecycle tracking.

use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use nenjo::Provider;
use nenjo_eventbus::{EventBus, Transport};
use nenjo_events::Response;

pub mod agent;
pub mod chat_history;
pub mod config;
pub mod doc_sync;
pub mod domain_session_store;
pub mod execution_trace;
pub mod external_mcp;
pub mod handlers;
pub mod loader;
pub mod manifest;
pub mod prompt;
pub mod providers;
pub mod security;
pub mod stream;
pub mod tools;

pub use nenjo::client as api_client;

use api_client::NenjoClient;
use chat_history::ChatHistory;
use config::Config;
use domain_session_store::DomainSessionStore;
use external_mcp::ExternalMcpPool;
use loader::FileSystemManifestLoader;
use providers::registry::ProviderRegistry;
use tools::{HarnessToolFactory, NativeRuntime};

// ---------------------------------------------------------------------------
// Shared types used by handlers
// ---------------------------------------------------------------------------

/// What kind of execution this is (for targeted cancellation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionKind {
    Chat,
    Task,
    Cron,
}

/// Tracks an active execution so it can be cancelled or paused.
pub struct ActiveExecution {
    pub kind: ExecutionKind,
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
    pub response_tx: tokio::sync::mpsc::UnboundedSender<Response>,
    pub chat_history: Arc<ChatHistory>,
    pub domain_session_store: Arc<DomainSessionStore>,
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
    chat_history: Arc<ChatHistory>,
    domain_session_store: Arc<DomainSessionStore>,
    external_mcp: Arc<ExternalMcpPool>,
    executions: ExecutionRegistry,
    domains: DomainRegistry,
    git_locks: GitLocks,
    shutdown: CancellationToken,
}

impl Harness {
    /// Create a new Harness.
    pub async fn new(config: Config) -> Result<Self> {
        let api = Arc::new(NenjoClient::new(config.backend_api_url(), &config.api_key));

        manifest::sync(&api, &config.manifests_dir, &config.workspace_dir).await?;

        let loader = FileSystemManifestLoader::new(&config.manifests_dir);
        let manifest = nenjo::ManifestLoader::load(&loader).await?;

        let mcp_servers =
            override_platform_mcp_url(manifest.mcp_servers.clone(), config.backend_api_url());

        let external_mcp = Arc::new(ExternalMcpPool::new());
        external_mcp.reconcile(&mcp_servers).await;

        let provider_registry =
            ProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);
        let security = Arc::new(nenjo_tools::security::SecurityPolicy::with_workspace_dir(
            config.workspace_dir.clone(),
        ));
        let runtime: Arc<dyn nenjo_tools::runtime::RuntimeAdapter> = Arc::new(NativeRuntime);
        let platform_resolver: Arc<dyn nenjo::PlatformToolResolver> = Arc::new(
            nenjo::PlatformMcpResolver::new(config.backend_api_url(), &config.api_key),
        );
        let tool_factory = HarnessToolFactory::new(
            security,
            runtime,
            config.clone(),
            external_mcp.clone(),
            platform_resolver.clone(),
        );

        let memory_dir = config.state_dir.join("memory");
        let mem = nenjo::memory::MarkdownMemory::new(&memory_dir, &config.state_dir);

        let agent_config = config.agent.clone();

        let template_source: Arc<dyn nenjo::context::TemplateSource> = Arc::new(
            manifest::FileTemplateSource::new(config.manifests_dir.join("context_blocks")),
        );

        let provider = Provider::builder()
            .with_loader(loader)
            .with_model_factory(provider_registry)
            .with_tool_factory(tool_factory)
            .with_memory(mem)
            .with_agent_config(agent_config)
            .with_platform_resolver(platform_resolver)
            .with_template_source(template_source)
            .build()
            .await
            .context("Failed to build Provider")?;

        let provider = Arc::new(ArcSwap::from_pointee(provider));
        let chat_history = Arc::new(ChatHistory::new(&config.workspace_dir));
        let domain_session_store = Arc::new(DomainSessionStore::new(&config.workspace_dir));
        let executions = Arc::new(DashMap::new());
        let domains = Arc::new(DashMap::new());
        let git_locks = Arc::new(DashMap::new());
        let shutdown = CancellationToken::new();

        for persisted in domain_session_store.load_all().unwrap_or_default() {
            let provider_snapshot = provider.load_full();
            let base_runner = match provider_snapshot.agent_by_id(persisted.agent_id).await {
                Ok(builder) => match builder.build().await {
                    Ok(runner) => runner,
                    Err(e) => {
                        warn!(session_id = %persisted.session_id, error = %e, "Failed to rebuild persisted domain base runner");
                        let _ = domain_session_store.delete(persisted.session_id);
                        continue;
                    }
                },
                Err(e) => {
                    warn!(session_id = %persisted.session_id, error = %e, "Failed to find agent for persisted domain session");
                    let _ = domain_session_store.delete(persisted.session_id);
                    continue;
                }
            };

            let domain_runner = match base_runner
                .domain_expansion(&persisted.domain_command)
                .await
            {
                Ok(runner) => runner,
                Err(e) => {
                    warn!(session_id = %persisted.session_id, error = %e, "Failed to rebuild persisted domain session");
                    let _ = domain_session_store.delete(persisted.session_id);
                    continue;
                }
            };

            let mut instance = domain_runner.instance().clone();
            if let Some(ref mut active_domain) = instance.prompt_context.active_domain {
                active_domain.session_id = persisted.session_id;
                active_domain.turn_number = persisted.turn_number;
            }
            let restored_runner = nenjo::AgentRunner::from_instance(
                instance,
                domain_runner.memory().cloned(),
                domain_runner.memory_scope().cloned(),
            );
            domains.insert(
                persisted.session_id,
                DomainSession {
                    runner: restored_runner,
                    agent_id: persisted.agent_id,
                    project_id: persisted.project_id,
                    domain_command: persisted.domain_command,
                    turn_number: persisted.turn_number,
                },
            );
        }

        Ok(Self {
            provider,
            config,
            api,
            chat_history,
            domain_session_store,
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
    pub async fn run<T: Transport + 'static>(&self, transport: T) -> Result<()> {
        let user_id = self.provider.load().manifest().user_id;
        let worker_id = transport.worker_id();
        let capabilities = self.resolved_capabilities();

        info!(
            %user_id,
            %worker_id,
            ?capabilities,
            "Subscribing to NATS events"
        );

        let mut bus = EventBus::builder()
            .transport(transport)
            .user_id(user_id)
            .build()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to build event bus: {e}"))?;

        let (response_tx, mut response_rx) = tokio::sync::mpsc::unbounded_channel::<Response>();
        let (command_tx, mut command_rx) =
            tokio::sync::mpsc::unbounded_channel::<nenjo_eventbus::ReceivedCommand>();

        // Send initial worker registration + heartbeat.
        let app_version = Some(env!("CARGO_PKG_VERSION").to_string());
        let _ = response_tx.send(Response::WorkerRegistered {
            worker_id,
            capabilities: capabilities.clone(),
            version: app_version.clone(),
        });
        let _ = response_tx.send(Response::WorkerHeartbeat {
            worker_id,
            capabilities: capabilities.clone(),
            version: app_version.clone(),
        });

        // Periodic heartbeat task.
        let heartbeat_tx = response_tx.clone();
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
                            Some(response) => {
                                if let Err(e) = bus.send_response(response).await {
                                    warn!(error = %e, "Failed to send response");
                                }
                            }
                            None => break,
                        }
                    }
                    result = bus.recv_command() => {
                        match result {
                            Ok(Some(cmd)) => {
                                if command_tx.send(cmd).is_err() {
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
            let command = received.command.clone();
            if let Err(e) = received.ack().await {
                warn!(error = %e, "Failed to ack command");
            }

            let ctx = CommandContext {
                provider: self.provider.clone(),
                response_tx: response_tx.clone(),
                chat_history: self.chat_history.clone(),
                domain_session_store: self.domain_session_store.clone(),
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

/// Override the platform MCP server URL to match the configured backend.
///
/// The manifest may contain a hardcoded production URL for the platform server,
/// but the worker might be running against a different backend (e.g. localhost).
pub fn override_platform_mcp_url(
    mut servers: Vec<nenjo::manifest::McpServerManifest>,
    backend_api_url: &str,
) -> Vec<nenjo::manifest::McpServerManifest> {
    let backend_mcp = format!("{}/mcp", backend_api_url.trim_end_matches('/'));
    for server in &mut servers {
        if server.name == external_mcp::PLATFORM_SERVER_NAME
            && server.url.as_deref() != Some(&backend_mcp)
        {
            debug!(
                old_url = ?server.url,
                new_url = %backend_mcp,
                "Overriding platform MCP server URL to match backend"
            );
            server.url = Some(backend_mcp);
            break;
        }
    }
    servers
}
