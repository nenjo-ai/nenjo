use anyhow::Result;
use clap::{Parser, Subcommand};
use nenjo_worker::RunArgs;

#[derive(Parser)]
#[command(name = "nenjo", about = "Nenjo platform agent CLI harness")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the agent worker (connect to NATS, process events)
    Run(RunArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run(args)) => nenjo_worker::run(args).await,
        None => nenjo_worker::run(RunArgs::default()).await,
    }
}
