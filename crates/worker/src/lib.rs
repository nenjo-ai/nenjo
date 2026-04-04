//! # worker
//!
//! Agent worker for the Nenjo platform.
//!
//! Boots the harness, connects to NATS via the event-bus transport layer,
//! and runs the agent event loop. This is the implementation behind `nenjo run`.
//!
//! The worker is resilient to backend and NATS outages: startup and the event
//! loop are wrapped in a retry loop with exponential backoff so the worker
//! automatically recovers when services come back online.

use std::time::Duration;

use anyhow::Result;
use clap::Args;
use harness::Harness;
use harness::config::Config;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Maximum backoff between connection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Initial backoff between connection attempts.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

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
}

/// Initialize tracing, load config, boot harness, connect NATS, and run.
pub async fn run(args: RunArgs) -> Result<()> {
    // Install rustls crypto provider before any TLS connections (ignore if already installed)
    let _ =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider());

    // Load .env file FIRST so RUST_LOG and other env vars are available
    dotenvy::dotenv().ok();

    // Initialize tracing — CLI arg takes priority over RUST_LOG env var
    let log_filter = args
        .log_level
        .clone()
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "info".into());

    // Build the env filter, suppressing noisy third-party crates at info level.
    // async_nats logs connection events at info which duplicates our own logs.
    let filter = if log_filter.contains("async_nats") {
        // User explicitly configured async_nats level — respect it.
        tracing_subscriber::EnvFilter::new(&log_filter)
    } else {
        tracing_subscriber::EnvFilter::new(format!("{log_filter},async_nats=warn"))
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(args.log_target))
        .init();

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

    info!(
        backend = %config.backend_api_url(),
        nats = %config.nats_url(),
        "Configuration loaded"
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
    let harness = Harness::new(config.clone()).await?;

    // Build the NATS transport.
    // The api_key_id is the stable worker identifier used for presence tracking.
    let api_key_id = harness.provider().manifest().api_key_id.ok_or_else(|| {
        anyhow::anyhow!(
            "Backend did not return api_key_id in manifest. \
             Ensure the backend is updated and the API key is valid."
        )
    })?;
    debug!(%api_key_id, "Using API key ID as stable worker identifier");

    let transport = nenjo_eventbus::nats::NatsTransport::builder()
        .url(config.nats_url())
        .token(&config.api_key)
        .worker_id(api_key_id)
        .build()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to NATS: {e}"))?;

    info!(nats_url = %config.nats_url(), "NATS transport connected");

    // Wire up the harness's own shutdown to the global one.
    let harness_shutdown = harness.shutdown_token();
    let global_shutdown = shutdown.clone();
    let link = tokio::spawn(async move {
        global_shutdown.cancelled().await;
        harness_shutdown.cancel();
    });

    // Run the event loop — blocks until the bus stream ends or shutdown.
    let result = harness.run(transport).await;

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
