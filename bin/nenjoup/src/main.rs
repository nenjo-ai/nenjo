use std::path::PathBuf;

use clap::{Parser, Subcommand};
use eyre::{Result, WrapErr};
use nenjo_updater::{CacheMode, UpdateOptions, check_for_update, update_bundle};

#[derive(Parser)]
#[command(name = "nenjoup", version, about = "Nenjo binary bundle updater")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Update the installed nenjo, nenpm, and nenjoup binaries.
    Update {
        /// Install a specific release tag or version, for example v0.12.0.
        #[arg(long)]
        version: Option<String>,
        /// Override the install directory. Defaults to this nenjoup binary's directory.
        #[arg(long)]
        install_dir: Option<PathBuf>,
    },
    /// Check whether a newer Nenjo release is available.
    Check,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Update {
            version,
            install_dir,
        } => {
            let mut options = UpdateOptions::new();
            if let Some(version) = version {
                options = options.version(version);
            }
            if let Some(install_dir) = install_dir {
                options = options.install_dir(install_dir);
            }

            let report = update_bundle(options).wrap_err("failed to update Nenjo binaries")?;
            println!(
                "Installed {} for {} to {}",
                report.version_tag,
                report.target,
                report.install_dir.display()
            );
            for binary in report.installed_binaries {
                println!("  {binary}");
            }
            Ok(())
        }
        Commands::Check => {
            match check_for_update(
                env!("CARGO_PKG_VERSION"),
                "nenjoup update",
                CacheMode::Refresh,
            )
            .wrap_err("failed to check for updates")?
            {
                Some(notice) => println!("{}", notice.render()),
                None => println!("Nenjo is up to date."),
            }
            Ok(())
        }
    }
}
