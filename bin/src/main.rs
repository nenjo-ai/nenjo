use std::process;

use clap::{Parser, Subcommand};
use eyre::{Result, eyre};
use nenjo_updater::{maybe_update_notice, run_nenjoup_update};
use nenjo_worker::RunArgs;
use tracing::debug;
use tracing_subscriber::{filter::LevelFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "nenjo", version, about = "Nenjo platform agent CLI harness")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the agent worker (connect to NATS, process events)
    Run(RunArgs),
    /// Update the installed Nenjo binary bundle.
    Update {
        /// Install a specific release tag or version, for example v0.12.0.
        #[arg(long)]
        version: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Install rustls crypto provider before any TLS connections
    if let Err(err) =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider())
    {
        debug!("Crypto provider installation failed (likely already installed): {err:?}");
    }

    // Load .env file FIRST so RUST_LOG and other env vars are available
    dotenvy::dotenv().ok();

    match cli.command {
        Some(Commands::Run(args)) => {
            print_update_notice("nenjo update");

            // Initialize tracing — CLI arg takes priority over RUST_LOG env var
            let log_filter = args
                .log_level
                .clone()
                .or_else(|| std::env::var("RUST_LOG").ok())
                .unwrap_or_else(|| "info".into());

            // Build the env filter, suppressing noisy third-party crates at info level.
            // async_nats logs connection events at info which duplicates our own logs.
            let base_filter = tracing_subscriber::EnvFilter::new(&log_filter);
            let filter = if base_filter.to_string().contains("async_nats") {
                // User explicitly configured async_nats level — respect it.
                base_filter
            } else {
                base_filter.add_directive("async_nats=warn".parse().expect("valid directive"))
            };
            let show_log_target = filter
                .max_level_hint()
                .is_some_and(|level| level >= LevelFilter::DEBUG);

            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().with_target(show_log_target))
                .try_init();

            nenjo_worker::run(args).await.map_err(|error| eyre!(error))
        }
        Some(Commands::Update { version }) => run_bundle_update(version),
        None => Err(eyre!("Command not provided")),
    }
}

fn print_update_notice(update_command: &str) {
    if let Some(notice) = maybe_update_notice(env!("CARGO_PKG_VERSION"), update_command) {
        eprintln!("{}", notice.render());
    }
}

fn run_bundle_update(version: Option<String>) -> Result<()> {
    let status = run_nenjoup_update(version.as_deref())?;
    if status.success() {
        Ok(())
    } else {
        process::exit(status.code().unwrap_or(1));
    }
}
