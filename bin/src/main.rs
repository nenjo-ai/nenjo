use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use nenjo_nenpm::{
    AddOptions, InfoOptions, InstallOptions, ListOptions, PackageSpec, PlannedPackage,
    RemoveOptions, add, info, install, list, remove, update,
};
use nenjo_worker::RunArgs;
use tracing::debug;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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
    /// Package manager commands.
    Pm {
        #[command(subcommand)]
        command: PmCommands,
    },
}

#[derive(Subcommand)]
enum PmCommands {
    /// Install packages declared in nenpm.yml or nenpm.yaml.
    Install {
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Include dev_dependencies.
        #[arg(long)]
        include_dev: bool,
        /// Resolve and print without writing nenpm.lock.yml.
        #[arg(long)]
        dry_run: bool,
        /// Maximum number of registry package sources fetched at once.
        #[arg(long, default_value_t = 8)]
        max_concurrency: usize,
    },
    /// Add a dependency to nenpm.yml, then install.
    Add {
        /// Package spec, for example @nenjo/nenji@^0.1.0.
        spec: String,
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Add to dev_dependencies.
        #[arg(long)]
        dev: bool,
        /// Resolve and print without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Maximum number of registry package sources fetched at once.
        #[arg(long, default_value_t = 8)]
        max_concurrency: usize,
    },
    /// Remove a dependency from nenpm.yml, then install.
    Remove {
        /// Package name.
        package: String,
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Resolve and print without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Maximum number of registry package sources fetched at once.
        #[arg(long, default_value_t = 8)]
        max_concurrency: usize,
    },
    /// Re-resolve registry versions and rewrite nenpm.lock.yml.
    Update {
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Include dev_dependencies.
        #[arg(long)]
        include_dev: bool,
        /// Resolve and print without writing nenpm.lock.yml.
        #[arg(long)]
        dry_run: bool,
        /// Maximum number of registry package sources fetched at once.
        #[arg(long, default_value_t = 8)]
        max_concurrency: usize,
    },
    /// List packages and modules from nenpm.lock.yml.
    List {
        /// Directory containing nenpm.lock.yml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Show package metadata from the default registry.
    Info {
        /// Package name.
        package: String,
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
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

            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().with_target(args.log_target))
                .try_init();

            nenjo_worker::run(args).await
        }
        Some(Commands::Pm { command }) => run_pm(command),
        None => anyhow::bail!("Command not provided"),
    }
}

fn run_pm(command: PmCommands) -> Result<()> {
    match command {
        PmCommands::Install {
            root,
            include_dev,
            dry_run,
            max_concurrency,
        } => {
            let report = install(
                InstallOptions::new(root)
                    .include_dev(include_dev)
                    .dry_run(dry_run)
                    .max_concurrency(max_concurrency),
            )?;
            print_install_report(&report);
        }
        PmCommands::Add {
            spec,
            root,
            dev,
            dry_run,
            max_concurrency,
        } => {
            let report = add(AddOptions::new(root, PackageSpec::parse(&spec)?)
                .dev(dev)
                .dry_run(dry_run)
                .max_concurrency(max_concurrency))?;
            print_install_report(&report.install);
        }
        PmCommands::Remove {
            package,
            root,
            dry_run,
            max_concurrency,
        } => {
            let report = remove(
                RemoveOptions::new(root, package)
                    .dry_run(dry_run)
                    .max_concurrency(max_concurrency),
            )?;
            print_install_report(&report.install);
        }
        PmCommands::Update {
            root,
            include_dev,
            dry_run,
            max_concurrency,
        } => {
            let report = update(
                InstallOptions::new(root)
                    .include_dev(include_dev)
                    .dry_run(dry_run)
                    .max_concurrency(max_concurrency),
            )?;
            print_install_report(&report);
        }
        PmCommands::List { root } => {
            let lockfile = list(ListOptions::new(root))?;
            for package in lockfile.packages {
                println!("{}@{}", package.name, package.version);
                for module in package.modules {
                    println!("  {} {} {}", module.kind.as_str(), module.name, module.path);
                }
            }
        }
        PmCommands::Info { package, root } => {
            let info = info(InfoOptions::new(root, package))?;
            for version in info.versions {
                println!("{}@{}", version.name, version.version);
                if !version.dependencies.is_empty() {
                    println!("  dependencies:");
                    for (name, requirement) in version.dependencies {
                        println!("    {name}: {requirement}");
                    }
                }
            }
        }
    }
    Ok(())
}

fn print_install_report(report: &nenjo_nenpm::InstallReport) {
    println!("{}", report.manifest_path.display());
    for package in report.plan.packages() {
        print_package(package);
    }
    if report.wrote_lockfile {
        println!("wrote {}", report.lockfile_path.display());
    } else {
        println!("dry run: did not write {}", report.lockfile_path.display());
    }
}

fn print_package(package: PlannedPackage<'_>) {
    println!("{}@{}", package.name, package.version);
    for module in package.modules {
        println!(
            "  {} {} {} ({})",
            module.kind.as_str(),
            module.name,
            module.path,
            module.schema
        );
    }
}
