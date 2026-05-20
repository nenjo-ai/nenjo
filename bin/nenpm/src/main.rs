use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use nenjo_nenpm::{
    AddOptions, InfoOptions, InstallOptions, ListOptions, PackageSpec, RemoveOptions, add, info,
    install, list, remove, update,
};

#[derive(Parser)]
#[command(name = "nenpm", about = "Nenjo package manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Install {
            root,
            include_dev,
            dry_run,
            max_concurrency,
        } => run_install(root, include_dev, dry_run, max_concurrency),
        Commands::Add {
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
            Ok(())
        }
        Commands::Remove {
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
            Ok(())
        }
        Commands::Update {
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
            Ok(())
        }
        Commands::List { root } => {
            let lockfile = list(ListOptions::new(root))?;
            for package in lockfile.packages {
                println!("{}@{}", package.name, package.version);
                for module in package.modules {
                    println!("  {} {} {}", module.kind.as_str(), module.name, module.path);
                }
            }
            Ok(())
        }
        Commands::Info { package, root } => {
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
            Ok(())
        }
    }
}

fn run_install(
    root: PathBuf,
    include_dev: bool,
    dry_run: bool,
    max_concurrency: usize,
) -> Result<()> {
    let report = install(
        InstallOptions::new(root)
            .include_dev(include_dev)
            .dry_run(dry_run)
            .max_concurrency(max_concurrency),
    )?;
    print_install_report(&report);
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

fn print_package(package: nenjo_nenpm::PlannedPackage<'_>) {
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
