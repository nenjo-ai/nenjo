//! # worker
//!
//! Agent worker for the Nenjo platform.
//!
//! Boots the harness, composes the raw event bus with the secure-envelope layer,
//! and runs the agent event loop. This is the implementation behind `nenjo run`.
//!
//! The worker is resilient to backend and NATS outages: startup and the event
//! loop are wrapped in a retry loop with exponential backoff so the worker
//! automatically recovers when services come back online.

pub mod crypto;
pub mod harness;

use std::time::Duration;

use crate::crypto::{EnrollmentStatus, WorkerAuthProvider};
use crate::harness::Harness;
use crate::harness::config::Config;
use anyhow::Result;
use clap::Args;
use nenjo_crypto_auth::EnrollmentBackedKeyProvider;
use nenjo_eventbus::EventBus;
use nenjo_secure_envelope::{SecureEnvelopeBus, SecureEnvelopeCodec};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Maximum backoff between connection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Initial backoff between connection attempts.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Poll interval while waiting for a worker enrollment to be approved.
const APPROVAL_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// CLI arguments for `nenjo run`.
#[derive(Args, Debug, Default)]
pub struct RunArgs {
    /// NATS server URL (e.g. tls://nats.nenjo.ai, only override for development reasons)
    #[arg(long, env = "NATS_URL")]
    pub nats_url: Option<String>,

    /// Backend API URL (e.g. https://api.nenjo.ai, only override for development reasons)
    #[arg(long, env = "NENJO_API_URL")]
    pub backend_url: Option<String>,

    /// Log level filter (e.g. info, debug, trace, info,nenjo=debug)
    #[arg(short, long, env = "RUST_LOG")]
    pub log_level: Option<String>,

    /// Worker capabilities (comma-separated). Default: all.
    /// Options: chat, task, cron, manifest, repo
    #[arg(long, env = "NENJO_CAPABILITIES", value_delimiter = ',')]
    pub capabilities: Option<Vec<String>>,

    /// Show the log target (module path) in log output.
    #[arg(long, env = "NENJO_LOG_TARGET")]
    pub log_target: bool,

    /// Override the .nenjo directory path (default: ~/.nenjo).
    #[arg(long, env = "NENJO_DIR")]
    pub nenjo_dir: Option<String>,

    /// Optional harness display name shown in the platform UI.
    #[arg(long, env = "NENJO_HARNESS_NAME")]
    pub harness_name: Option<String>,

    /// Optional harness labels shown in the platform UI.
    #[arg(long, env = "NENJO_HARNESS_LABELS", value_delimiter = ',')]
    pub harness_labels: Option<Vec<String>>,
}

/// Initialize tracing, load config, boot harness, connect NATS, and run.
pub async fn run(args: RunArgs) -> Result<()> {
    info!("Starting Nenjo worker...");

    // Load configuration (config.toml + env overrides + API key validation)
    let mut config = Config::load_or_init(args.nenjo_dir.as_deref())?;

    // CLI args override config + env values
    if let Some(ref url) = args.nats_url {
        config.nats_url = Some(url.clone());
    }
    if let Some(ref url) = args.backend_url {
        config.backend_api_url = Some(url.clone());
    }
    if let Some(ref caps) = args.capabilities {
        let mut parsed = Vec::new();
        for cap_str in caps {
            let cap: nenjo_events::Capability = cap_str
                .parse()
                .map_err(|e: String| anyhow::anyhow!("Invalid capability '{}': {}", cap_str, e))?;
            parsed.push(cap);
        }
        config.capabilities = parsed;
    }
    if let Some(ref name) = args.harness_name {
        config.harness_name = Some(name.clone());
    }
    if let Some(ref labels) = args.harness_labels {
        config.harness_labels = labels.clone();
    }

    info!(
        backend = %config.backend_api_url(),
        nats = %config.nats_url(),
        "Configuration loaded"
    );

    run_with_config(config).await
}

/// Run the worker using a fully constructed config.
///
/// This is the preferred entrypoint for embedded runtimes like tests, which
/// already control the config directory and do not need the CLI-oriented
/// `load_or_init` bootstrap path.
pub async fn run_with_config(config: Config) -> Result<()> {
    info!(
        backend = %config.backend_api_url(),
        nats = %config.nats_url(),
        "Starting worker runtime"
    );

    // Shutdown token shared across retry iterations — a signal here means
    // the user wants the process to stop, not just the current event loop.
    let shutdown = CancellationToken::new();
    let shutdown_for_signal = shutdown.clone();

    // Listen for OS signals in a dedicated task.
    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown_for_signal.cancel();
    });

    // Retry loop: (re-)creates the harness + NATS transport on each iteration.
    let mut backoff = INITIAL_BACKOFF;

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        match run_once(&config, &shutdown).await {
            Ok(()) => {
                // Clean shutdown (signal received) — exit.
                info!("Worker shut down");
                break;
            }
            Err(e) => {
                if shutdown.is_cancelled() {
                    info!("Worker shut down");
                    break;
                }
                warn!(
                    error = %e,
                    retry_in = ?backoff,
                    "Worker failed, retrying"
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.cancelled() => {
                        info!("Worker shut down");
                        break;
                    }
                }
                backoff = std::cmp::min(backoff.saturating_mul(2), MAX_BACKOFF);
            }
        }
    }

    Ok(())
}

/// One full lifecycle: bootstrap → connect → event loop.
///
/// Returns `Ok(())` when the shutdown token is cancelled (graceful stop).
/// Returns `Err` on any transient failure so the caller can retry.
async fn run_once(config: &Config, shutdown: &CancellationToken) -> Result<()> {
    // Create harness — runs bootstrap, builds Provider, connects MCP servers
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(
        config.state_dir.join("crypto"),
    )?);
    let harness = Harness::new(config.clone(), auth_provider.clone()).await?;

    // The api_key_id is the stable worker identifier used for presence tracking.
    let auth = harness.provider().manifest().auth.clone().ok_or_else(|| {
        anyhow::anyhow!("Backend did not return auth in manifest. Ensure bootstrap is up to date.")
    })?;
    let api_key_id = auth.api_key_id.ok_or_else(|| {
        anyhow::anyhow!(
            "Backend did not return auth.api_key_id in manifest. \
             Ensure the backend is updated and the API key is valid."
        )
    })?;
    debug!(%api_key_id, "Using API key ID as stable worker identifier");

    let identity = auth_provider.identity();
    match auth_provider.enrollment_status().await {
        EnrollmentStatus::Pending => {
            info!(
                worker_crypto_id = %identity.worker_id,
                "Worker crypto identity loaded; enrollment pending"
            );
        }
        EnrollmentStatus::Active => {
            info!(
                worker_crypto_id = %identity.worker_id,
                "Worker crypto identity loaded; user-routed ACK available"
            );
        }
    }

    let user_id = auth.user_id;
    let org_id = auth.org_id;
    wait_for_enrollment_approval(
        harness.api().as_ref(),
        auth_provider.as_ref(),
        api_key_id,
        user_id,
        build_harness_metadata(config),
        shutdown,
    )
    .await?;

    let transport = nenjo_eventbus::nats::NatsTransport::builder()
        .url(config.nats_url())
        .token(&config.api_key)
        .worker_id(api_key_id)
        .build()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to NATS: {e}"))?;

    let codec = SecureEnvelopeCodec::new(
        EnrollmentBackedKeyProvider::new(auth_provider, harness.api(), api_key_id, user_id),
        org_id,
    );

    let bus = EventBus::builder()
        .transport(transport)
        .build()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to build event bus: {e}"))?;
    let secure_bus = SecureEnvelopeBus::new(bus, codec);

    info!(nats_url = %config.nats_url(), "Eventbus transport connected");

    // Wire up the harness's own shutdown to the global one.
    let harness_shutdown = harness.shutdown_token();
    let global_shutdown = shutdown.clone();
    let link = tokio::spawn(async move {
        global_shutdown.cancelled().await;
        harness_shutdown.cancel();
    });

    // Run the event loop — blocks until the bus stream ends or shutdown.
    let result = harness.run(secure_bus, user_id).await;

    link.abort();

    match result {
        Ok(()) if shutdown.is_cancelled() => Ok(()),
        Ok(()) => {
            // Event loop exited without a shutdown signal (e.g. NATS stream ended).
            // Treat as transient so the retry loop reconnects.
            Err(anyhow::anyhow!("Event loop exited unexpectedly"))
        }
        Err(e) => Err(e),
    }
}

async fn wait_for_enrollment_approval(
    api: &nenjo::client::NenjoClient,
    auth_provider: &WorkerAuthProvider,
    api_key_id: uuid::Uuid,
    bootstrap_user_id: uuid::Uuid,
    metadata: Option<serde_json::Value>,
    shutdown: &CancellationToken,
) -> Result<()> {
    auth_provider
        .sync_worker_enrollment(api, api_key_id, bootstrap_user_id, metadata.clone())
        .await
        .map_err(|error| anyhow::anyhow!("Failed to initialize worker enrollment: {error}"))?;

    if matches!(
        auth_provider.enrollment_status().await,
        EnrollmentStatus::Active
    ) {
        info!(%api_key_id, "Worker enrollment approved");
        return Ok(());
    }

    info!(
        %api_key_id,
        poll_every = ?APPROVAL_POLL_INTERVAL,
        "Waiting for worker enrollment approval"
    );

    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }

        match api.fetch_worker_enrollment_status(api_key_id).await {
            Ok(Some(status)) => match status.state {
                nenjo::client::WorkerEnrollmentState::Active => {
                    let _ = bootstrap_user_id;
                    auth_provider.apply_backend_enrollment(&status).await?;
                    info!(%api_key_id, "Worker enrollment approved");
                    return Ok(());
                }
                nenjo::client::WorkerEnrollmentState::Pending => {}
                nenjo::client::WorkerEnrollmentState::Revoked => {
                    return Err(anyhow::anyhow!(
                        "Worker enrollment was revoked before approval completed"
                    ));
                }
            },
            Ok(None) => {
                debug!(
                    %api_key_id,
                    "Worker enrollment status not found yet; continuing to wait"
                );
            }
            Err(error) => {
                debug!(
                    %api_key_id,
                    error = %error,
                    "Failed to fetch worker enrollment status; continuing to wait"
                );
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(APPROVAL_POLL_INTERVAL) => {}
            _ = shutdown.cancelled() => return Ok(()),
        }
    }
}

fn build_harness_metadata(config: &Config) -> Option<serde_json::Value> {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|value| !value.trim().is_empty());
    let version = Some(env!("CARGO_PKG_VERSION").to_string());
    let name = config
        .harness_name
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let labels: Vec<String> = config
        .harness_labels
        .iter()
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect();

    if name.is_none() && labels.is_empty() && host.is_none() && version.is_none() {
        return None;
    }

    Some(json!({
        "name": name,
        "labels": labels,
        "host": host,
        "version": version,
    }))
}

/// Wait for SIGINT or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(_) => {
                ctrl_c.await.ok();
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}
