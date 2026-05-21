use std::path::PathBuf;

use clap::{Parser, Subcommand};
use console::style;
use eyre::{Result, WrapErr};
use nenjo_nenpm::{
    AddOptions, CleanOptions, InfoOptions, InitOptions, InstallOptions, ListOptions, PackageSource,
    PackageSpec, PrepareOptions, RemoveOptions, ValidateOptions, add, clean, info, init, install,
    list, prepare, remove, update, validate,
};

mod pm_ui;

#[derive(Parser)]
#[command(name = "nenpm", about = "Nenjo package manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a starter nenpm.yml.
    Init {
        /// Directory to initialize.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Install packages declared in nenpm.yml or nenpm.yaml.
    Install {
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Package install directory. Defaults to <root>/.nenjo/packages.
        #[arg(long)]
        packages_dir: Option<PathBuf>,
        /// Resolve and print without writing nenpm.lock.yml.
        #[arg(long)]
        dry_run: bool,
        /// Fail if nenpm.lock.yml is missing or out of date.
        #[arg(long)]
        locked: bool,
    },
    /// Add a registry or package dependency, then install when packages are added.
    Add {
        /// Add spec: @org, @org/package, @org/package@^1.2.3, or @org/*.
        spec: String,
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Package install directory. Defaults to <root>/.nenjo/packages.
        #[arg(long)]
        packages_dir: Option<PathBuf>,
        /// Resolve and print without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Git reference to use.
        #[arg(long = "ref", default_value = "main")]
        reference: String,
        /// Registry manifest path inside the repository.
        #[arg(long, default_value = "packages.yaml")]
        manifest_path: String,
    },
    /// Remove a dependency from nenpm.yml, then install.
    Remove {
        /// Package name.
        package: String,
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Package install directory. Defaults to <root>/.nenjo/packages.
        #[arg(long)]
        packages_dir: Option<PathBuf>,
        /// Resolve and print without writing files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-resolve registry versions and rewrite nenpm.lock.yml.
    Update {
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Package install directory. Defaults to <root>/.nenjo/packages.
        #[arg(long)]
        packages_dir: Option<PathBuf>,
        /// Resolve and print without writing nenpm.lock.yml.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove derived package install artifacts.
    Clean {
        /// Directory containing nenpm.yml or nenpm.yaml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Package install directory. Defaults to <root>/.nenjo/packages.
        #[arg(long)]
        packages_dir: Option<PathBuf>,
        /// Print what would be removed without deleting files.
        #[arg(long)]
        dry_run: bool,
    },
    /// List packages available from configured registries.
    List {
        /// Optional registry scope to list, for example @nenjo.
        registry: Option<String>,
        /// Directory containing nenpm.yml or nenpm.yaml.
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
    /// Validate a publisher-side package registry.
    Validate {
        /// Registry root directory.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Registry manifest path relative to root.
        #[arg(long)]
        registry: Option<String>,
    },
    /// Validate and compile publisher-side registry metadata.
    Prepare {
        /// Registry root directory.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Registry manifest path relative to root.
        #[arg(long)]
        registry: Option<String>,
        /// Output metadata path.
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { root } => {
            pm_ui::print_header("nenpm init");
            let report = pm_ui::run_phase("initializing dependency manifest", || {
                init(InitOptions::new(root)).wrap_err("failed to initialize nenpm.yml")
            })?;
            pm_ui::print_success(format!("created {}", report.manifest_path.display()));
            Ok(())
        }
        Commands::Install {
            root,
            packages_dir,
            dry_run,
            locked,
        } => run_install(root, packages_dir, dry_run, locked),
        Commands::Add {
            spec,
            root,
            packages_dir,
            dry_run,
            reference,
            manifest_path,
        } => {
            pm_ui::print_header("nenpm add");
            let report = pm_ui::run_phase("adding dependency and installing packages", || {
                let options = AddOptions::new(root, PackageSpec::parse(&spec)?)
                    .dry_run(dry_run)
                    .reference(reference)
                    .manifest_path(manifest_path);
                add(apply_add_packages_dir(options, packages_dir)).wrap_err("failed to add package")
            })?;
            print_add_report(&report);
            if let Some(install) = &report.install {
                print_install_report(install);
            }
            Ok(())
        }
        Commands::Remove {
            package,
            root,
            packages_dir,
            dry_run,
        } => {
            pm_ui::print_header("nenpm remove");
            let report = pm_ui::run_phase("removing dependency and reconciling packages", || {
                let options = RemoveOptions::new(root, package).dry_run(dry_run);
                remove(apply_remove_packages_dir(options, packages_dir))
                    .wrap_err("failed to remove package")
            })?;
            print_install_report(&report.install);
            Ok(())
        }
        Commands::Update {
            root,
            packages_dir,
            dry_run,
        } => {
            pm_ui::print_header("nenpm update");
            let report = pm_ui::run_phase("updating package graph", || {
                update(
                    apply_install_packages_dir(InstallOptions::new(root), packages_dir)
                        .dry_run(dry_run),
                )
                .wrap_err("failed to update packages")
            })?;
            print_install_report(&report);
            Ok(())
        }
        Commands::Clean {
            root,
            packages_dir,
            dry_run,
        } => {
            pm_ui::print_header("nenpm clean");
            let report = pm_ui::run_phase("cleaning derived package artifacts", || {
                let options = CleanOptions::new(root).dry_run(dry_run);
                clean(apply_clean_packages_dir(options, packages_dir))
                    .wrap_err("failed to clean packages")
            })?;
            print_clean_report(&report);
            Ok(())
        }
        Commands::List { registry, root } => {
            let mut options = ListOptions::new(root);
            if let Some(registry) = registry {
                options = options.registry(registry);
            }
            let packages = list(options).wrap_err("failed to list packages")?;
            for package in packages {
                println!("{}", style(package.name).cyan().bold());
                for version in package.versions {
                    println!("  {}", style(version).dim());
                }
            }
            Ok(())
        }
        Commands::Info { package, root } => {
            let info =
                info(InfoOptions::new(root, package)).wrap_err("failed to read package info")?;
            print_info_report(&info);
            Ok(())
        }
        Commands::Validate { root, registry } => {
            pm_ui::print_header("nenpm validate");
            let mut options = ValidateOptions::new(root);
            if let Some(registry) = registry {
                options = options.registry(registry);
            }
            let report = pm_ui::run_phase("checking package registry", || {
                validate(options).wrap_err("failed to validate package registry")
            })?;
            println!("{}", report.registry_path);
            for package in report.packages.values() {
                println!("{}@{}", package.name, package.version);
            }
            Ok(())
        }
        Commands::Prepare {
            root,
            registry,
            output,
        } => {
            pm_ui::print_header("nenpm prepare");
            let mut options = PrepareOptions::new(root);
            if let Some(registry) = registry {
                options = options.registry(registry);
            }
            if let Some(output) = output {
                options = options.output(output);
            }
            let report = pm_ui::run_phase("preparing package registry", || {
                prepare(options).wrap_err("failed to prepare package registry")
            })?;
            println!("wrote {}", report.output_path.display());
            Ok(())
        }
    }
}

fn run_install(
    root: PathBuf,
    packages_dir: Option<PathBuf>,
    dry_run: bool,
    locked: bool,
) -> Result<()> {
    pm_ui::print_header("nenpm install");
    let report = pm_ui::run_phase("resolving and installing packages", || {
        install(
            apply_install_packages_dir(InstallOptions::new(root), packages_dir)
                .dry_run(dry_run)
                .locked(locked),
        )
        .wrap_err("failed to install packages")
    })?;
    print_install_report(&report);
    Ok(())
}

fn apply_install_packages_dir(
    options: InstallOptions,
    packages_dir: Option<PathBuf>,
) -> InstallOptions {
    match packages_dir {
        Some(packages_dir) => options.packages_dir(packages_dir),
        None => options,
    }
}

fn apply_add_packages_dir(options: AddOptions, packages_dir: Option<PathBuf>) -> AddOptions {
    match packages_dir {
        Some(packages_dir) => options.packages_dir(packages_dir),
        None => options,
    }
}

fn apply_remove_packages_dir(
    options: RemoveOptions,
    packages_dir: Option<PathBuf>,
) -> RemoveOptions {
    match packages_dir {
        Some(packages_dir) => options.packages_dir(packages_dir),
        None => options,
    }
}

fn apply_clean_packages_dir(options: CleanOptions, packages_dir: Option<PathBuf>) -> CleanOptions {
    match packages_dir {
        Some(packages_dir) => options.packages_dir(packages_dir),
        None => options,
    }
}

fn print_add_report(report: &nenjo_nenpm::AddReport) {
    if report.registry_added {
        pm_ui::print_success(format!(
            "registered registry in {}",
            report.manifest_path.display()
        ));
    } else {
        pm_ui::print_note(format!(
            "registry already configured in {}",
            report.manifest_path.display()
        ));
    }
    if !report.dependencies_added.is_empty() {
        pm_ui::print_success(format!(
            "added {} dependencies",
            report.dependencies_added.len()
        ));
        for dependency in &report.dependencies_added {
            pm_ui::print_note(format!("dependency {dependency}"));
        }
    }
}

fn print_clean_report(report: &nenjo_nenpm::CleanReport) {
    if report.dry_run {
        pm_ui::print_note(format!(
            "dry run: would remove {} packages from {}",
            report.package_count,
            report.packages_dir.display()
        ));
    } else if report.removed {
        pm_ui::print_success(format!(
            "removed {} packages from {}",
            report.package_count,
            report.packages_dir.display()
        ));
    } else {
        pm_ui::print_note(format!(
            "nothing to clean at {}",
            report.packages_dir.display()
        ));
    }
}

fn print_info_report(info: &nenjo_nenpm::PackageInfo) {
    for version in &info.versions {
        println!(
            "{}{}",
            style(&version.name).cyan().bold(),
            style(format!("@{}", version.version)).dim()
        );
        if let Some(description) = &version.description {
            println!("  {}", description);
        }
        println!("  source {}", style(format_source(&version.source)).dim());
        if let Some(checksum) = &version.checksum {
            println!("  checksum {}", style(checksum).dim());
        }
        if !version.dependencies.is_empty() {
            println!("  dependencies");
            for (name, requirement) in &version.dependencies {
                println!("    {name}: {requirement}");
            }
        }
        if !version.modules.is_empty() {
            println!("  modules");
            for module in &version.modules {
                let description = module
                    .description
                    .as_deref()
                    .map(|description| format!(" - {description}"))
                    .unwrap_or_default();
                println!(
                    "    {} {} {} {}{}",
                    style(module.kind.as_str()).dim(),
                    style(&module.name).bold(),
                    module.path,
                    style(format!("({})", module.schema)).dim(),
                    description
                );
            }
        }
    }
}

fn print_install_report(report: &nenjo_nenpm::InstallReport) {
    let packages: Vec<_> = report.plan.packages().collect();
    let module_count = packages
        .iter()
        .map(|package| package.modules.len())
        .sum::<usize>();

    pm_ui::print_note(format!("manifest {}", report.manifest_path.display()));
    for package in &packages {
        print_package(package);
    }
    if report.wrote_lockfile {
        pm_ui::print_success(format!(
            "installed {} packages, reused {}, pruned {}, {} modules; wrote {}",
            report.materialization.installed,
            report.materialization.reused,
            report.materialization.pruned,
            module_count,
            report.lockfile_path.display()
        ));
    } else {
        pm_ui::print_note(format!(
            "dry run: resolved {} packages, {} modules; did not write {}",
            packages.len(),
            module_count,
            report.lockfile_path.display()
        ));
    }
}

fn format_source(source: &PackageSource) -> String {
    match source {
        PackageSource::Git {
            url,
            reference,
            manifest_path,
        } => format!("git {url}#{reference} {manifest_path}"),
        PackageSource::Artifact {
            url, manifest_path, ..
        } => format!("artifact {url} {manifest_path}"),
        PackageSource::Remote { url, .. } => format!("remote {url}"),
        PackageSource::Local {
            root,
            manifest_path,
            scope,
        } => match scope {
            Some(scope) => format!("local {} {} {}", root.display(), scope, manifest_path),
            None => format!("local {} {}", root.display(), manifest_path),
        },
    }
}

fn print_package(package: &nenjo_nenpm::PlannedPackage<'_>) {
    println!(
        "{}{}",
        style(&package.name).cyan().bold(),
        style(format!("@{}", package.version)).dim()
    );
    for module in &package.modules {
        println!(
            "  {} {} {} {}",
            style(module.kind.as_str()).dim(),
            style(module.name).bold(),
            module.path,
            style(format!("({})", module.schema)).dim()
        );
    }
}
