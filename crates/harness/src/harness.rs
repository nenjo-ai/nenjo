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

use crate::api_client::NenjoClient;
use crate::chat_history::ChatHistory;
use crate::config::Config;
use crate::external_mcp::ExternalMcpPool;
use crate::handlers;
use crate::loader::FileSystemManifestLoader;
use crate::providers::registry::ProviderRegistry;
use crate::tools::{HarnessToolFactory, NativeRuntime};

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
    pub turn_number: u32,
}

/// Thread-safe registry of active domain sessions, keyed by `domain_session_id`.
pub type DomainRegistry = Arc<DashMap<Uuid, DomainSession>>;

/// Shared context passed to each command handler.
///
/// Handlers load the current Provider from `provider` (lock-free via ArcSwap).
/// Responses are sent via `response_tx` (never touch the bus directly).
pub struct CommandContext {
    pub provider: Arc<ArcSwap<Provider>>,
    pub response_tx: tokio::sync::mpsc::UnboundedSender<Response>,
    pub chat_history: Arc<ChatHistory>,
    pub api: Arc<NenjoClient>,
    pub config: Config,
    pub external_mcp: Arc<ExternalMcpPool>,
    pub executions: ExecutionRegistry,
    pub domains: DomainRegistry,
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
    external_mcp: Arc<ExternalMcpPool>,
    executions: ExecutionRegistry,
    domains: DomainRegistry,
    shutdown: CancellationToken,
}

impl Harness {
    /// Create a new Harness.
    pub async fn new(config: Config) -> Result<Self> {
        let api = Arc::new(NenjoClient::new(config.backend_api_url(), &config.api_key));

        crate::manifest::sync(&api, &config.data_dir, &config.workspace_dir).await?;

        let loader = FileSystemManifestLoader::new(&config.data_dir);
        let manifest = nenjo::ManifestLoader::load(&loader).await?;

        let mcp_servers =
            override_platform_mcp_url(manifest.mcp_servers.clone(), config.backend_api_url());

        let external_mcp = Arc::new(ExternalMcpPool::new());
        external_mcp.reconcile(&mcp_servers).await;

        let provider_registry =
            ProviderRegistry::new(&config.model_provider_api_keys, &config.reliability);
        let security = Arc::new(nenjo_tools::security::SecurityPolicy::default());
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

        let home = directories::UserDirs::new()
            .map(|u| u.home_dir().to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let memory_dir = home.join(".nenjo").join("memory");
        let mem = nenjo::memory::MarkdownMemory::new(&memory_dir);

        let agent_config = config.agent.clone();

        let provider = Provider::builder()
            .with_loader(loader)
            .with_model_factory(provider_registry)
            .with_tool_factory(tool_factory)
            .with_memory(mem)
            .with_agent_config(agent_config)
            .with_platform_resolver(platform_resolver)
            .build()
            .await
            .context("Failed to build Provider")?;

        let provider = Arc::new(ArcSwap::from_pointee(provider));
        let chat_history = Arc::new(ChatHistory::new(&config.workspace_dir));
        let executions = Arc::new(DashMap::new());
        let domains = Arc::new(DashMap::new());
        let shutdown = CancellationToken::new();

        Ok(Self {
            provider,
            config,
            api,
            chat_history,
            external_mcp,
            executions,
            domains,
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

        info!("Harness event loop started");

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
                api: self.api.clone(),
                config: self.config.clone(),
                external_mcp: self.external_mcp.clone(),
                executions: self.executions.clone(),
                domains: self.domains.clone(),
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
        if server.name == crate::external_mcp::PLATFORM_SERVER_NAME
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
