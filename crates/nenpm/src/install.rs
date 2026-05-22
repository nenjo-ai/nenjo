use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::Result;
use anyhow::{Context, anyhow};
use nenjo_packages::{LocalPackageResolver, ResolvedPackage, ResolvedPackageGraph};
use rayon::prelude::*;

use crate::dependency::{DependencyManifest, LoadedDependencyManifest};
use crate::lockfile::{LockedSource, NenpmLock, lockfile_from_plan};
use crate::plan::InstallPlan;
use crate::registry::{
    PackageRegistry, RegistryPackageVersion, registry_record_manifest_path, verify_registry_package,
};
use crate::source::{
    DefaultPackageSourceFetcher, PackageSource, PackageSourceFetcher, normalize_source_paths,
    source_fetch_key,
};

mod integrity;
mod materialize;
mod registries;

use integrity::verify_lockfile_integrity;
use materialize::materialize_packages;
use registries::ConfiguredRegistries;
pub(crate) use registries::is_registry_manifest_path;

/// Options for installing a dependency manifest.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Directory containing `nenpm.yml` or `nenpm.yaml`.
    pub root: PathBuf,
    /// Package install directory. Defaults to `<root>/.nenjo/packages`.
    pub packages_dir: PathBuf,
    /// Write `nenpm.lock.yml` when false; only resolve when true.
    pub dry_run: bool,
    /// Re-resolve registry versions instead of preserving lockfile pins.
    pub update: bool,
    /// Require `nenpm.lock.yml` to exist and match the resolved dependency graph.
    pub locked: bool,
}

impl InstallOptions {
    /// Create install options for a project root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let packages_dir = root.join(".nenjo").join("packages");
        Self {
            root,
            packages_dir,
            dry_run: false,
            update: false,
            locked: false,
        }
    }

    /// Override the package install directory.
    pub fn packages_dir(mut self, packages_dir: impl Into<PathBuf>) -> Self {
        self.packages_dir = packages_dir.into();
        self
    }

    /// Resolve without writing a lockfile.
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Re-resolve registry versions instead of preserving lockfile pins.
    pub fn update(mut self, update: bool) -> Self {
        self.update = update;
        self
    }

    /// Require the install to match `nenpm.lock.yml`.
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }
}

/// Result of a dependency manifest install.
#[derive(Debug, Clone)]
pub struct InstallReport {
    /// Loaded dependency file path.
    pub manifest_path: PathBuf,
    /// Lockfile path.
    pub lockfile_path: PathBuf,
    /// Resolved install plan.
    pub plan: InstallPlan,
    /// Generated lockfile content.
    pub lockfile: NenpmLock,
    /// Whether the lockfile was written.
    pub wrote_lockfile: bool,
    /// Package materialization cache/install/prune summary.
    pub materialization: MaterializationReport,
}

/// Summary of package tree materialization work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationReport {
    /// Packages copied into the install tree.
    pub installed: usize,
    /// Packages already present and verified against the lockfile.
    pub reused: usize,
    /// Previously installed package directories removed because they are no longer locked.
    pub pruned: usize,
}

/// Install packages from `nenpm.yml` or `nenpm.yaml`.
pub fn install(options: InstallOptions) -> Result<InstallReport> {
    if options.locked && options.update {
        bail!("--locked cannot be combined with update");
    }
    let loaded = DependencyManifest::load_from_dir(&options.root)?;
    let lockfile_path = options.root.join("nenpm.lock.yml");
    let existing_lockfile = if options.update || !lockfile_path.exists() {
        None
    } else {
        Some(NenpmLock::load_file(&lockfile_path)?)
    };
    if options.locked && existing_lockfile.is_none() {
        bail!(
            "locked install requires {}; run nenpm install first",
            lockfile_path.display()
        );
    }
    let locked_versions = existing_lockfile
        .as_ref()
        .map(NenpmLock::versions_by_package)
        .unwrap_or_default();
    let resolved = resolve_dependency_manifest(&loaded, &locked_versions)?;
    let plan = InstallPlan::from_graph(resolved.graph)?;
    let lockfile = lockfile_from_plan(&plan, &resolved.sources)?;
    if let Some(existing_lockfile) = &existing_lockfile {
        verify_lockfile_integrity(existing_lockfile, &lockfile)?;
        if options.locked && existing_lockfile != &lockfile {
            bail!("nenpm.lock.yml is out of date; run nenpm install to update it");
        }
    }
    let (wrote_lockfile, materialization) = if options.dry_run {
        (false, MaterializationReport::default())
    } else {
        let materialization = materialize_packages(&options.root, &options.packages_dir, &lockfile)
            .context("failed to materialize package sources")?;
        let content =
            serde_yaml::to_string(&lockfile).context("failed to serialize nenpm lockfile")?;
        fs::write(&lockfile_path, content)
            .with_context(|| format!("failed to write {}", lockfile_path.display()))?;
        (true, materialization)
    };
    Ok(InstallReport {
        manifest_path: loaded.path,
        lockfile_path,
        plan,
        lockfile,
        wrote_lockfile,
        materialization,
    })
}

fn resolve_dependency_manifest(
    loaded: &LoadedDependencyManifest,
    locked_versions: &BTreeMap<String, String>,
) -> Result<ResolvedInstall> {
    let manifest_dir = loaded
        .path
        .parent()
        .ok_or_else(|| anyhow!("dependency manifest has no parent directory"))?;
    let registries = ConfiguredRegistries::load(loaded)?;
    let mut packages = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut registry_records: BTreeMap<String, RegistryPackageVersion> = BTreeMap::new();
    let mut stack: Vec<(String, String)> = loaded
        .manifest
        .dependencies
        .iter()
        .map(|(name, requirement)| (name.clone(), requirement.clone()))
        .collect();
    let root_package = stack
        .first()
        .map(|(name, _)| name.clone())
        .unwrap_or_default();
    if stack.is_empty() {
        return Ok(ResolvedInstall {
            graph: ResolvedPackageGraph {
                root_package,
                packages,
            },
            sources,
        });
    }

    while let Some((name, requirement)) = stack.pop() {
        if loaded.manifest.overrides.contains_key(&name) {
            if let Some(existing) = packages.get(&name) {
                let existing: &ResolvedPackage = existing;
                if !nenjo_packages::version_satisfies(&existing.version, &requirement) {
                    bail!(
                        "{name} was already resolved to {}, which does not satisfy {requirement}",
                        existing.version
                    );
                }
                continue;
            }

            let override_source = loaded
                .manifest
                .overrides
                .get(&name)
                .expect("override existence was checked");
            let source = normalize_source_paths(override_source.to_package_source()?, manifest_dir);
            resolve_override_source(
                &mut packages,
                &mut sources,
                &mut stack,
                &name,
                &requirement,
                source,
            )?;
            continue;
        }

        if let Some(existing) = packages.get(&name) {
            let existing: &ResolvedPackage = existing;
            if !nenjo_packages::version_satisfies(&existing.version, &requirement) {
                bail!(
                    "{name} was already resolved to {}, which does not satisfy {requirement}",
                    existing.version
                );
            }
            continue;
        }

        if let Some(existing) = registry_records.get(&name) {
            if !nenjo_packages::version_satisfies(&existing.version, &requirement) {
                bail!(
                    "{name} was already resolved to {}, which does not satisfy {requirement}",
                    existing.version
                );
            }
            continue;
        }

        let registry = registries.for_package(&name)?;
        let registry_requirement = locked_versions
            .get(&name)
            .filter(|version| nenjo_packages::version_satisfies(version, &requirement))
            .unwrap_or(&requirement);
        let record = registry
            .resolve_version(&name, registry_requirement)
            .with_context(|| format!("failed to resolve {name} from registry"))?;
        for (dependency, requirement) in &record.dependencies {
            stack.push((dependency.clone(), requirement.clone()));
        }
        registry_records.insert(name, record);
    }

    let registry_resolved = resolve_registry_records_parallel(registry_records)?;
    merge_resolved_packages(&mut packages, registry_resolved.packages)?;
    sources.extend(registry_resolved.sources);

    let graph = ResolvedPackageGraph {
        root_package,
        packages,
    };
    graph.validate_versions()?;
    Ok(ResolvedInstall { graph, sources })
}

fn resolve_override_source(
    packages: &mut BTreeMap<String, ResolvedPackage>,
    sources: &mut BTreeMap<String, LockedSource>,
    stack: &mut Vec<(String, String)>,
    name: &str,
    requirement: &str,
    source: PackageSource,
) -> Result<()> {
    let fetched = DefaultPackageSourceFetcher::new()
        .fetch(&source)
        .with_context(|| format!("failed to fetch source for {name}"))?;

    if is_registry_manifest_path(&fetched.manifest_path) {
        let resolver =
            LocalPackageResolver::with_registry_path(&fetched.root, &fetched.manifest_path);
        let graph = resolver
            .resolve_package_graph(name)
            .with_context(|| format!("failed to resolve local registry package {name}"))?;
        let root = graph
            .packages
            .get(name)
            .ok_or_else(|| anyhow!("local registry graph did not include {name}"))?;
        if !nenjo_packages::version_satisfies(&root.version, requirement) {
            bail!(
                "{name} resolved to {}, which does not satisfy {requirement}",
                root.version
            );
        }
        for package_name in graph.packages.keys() {
            sources.insert(
                package_name.clone(),
                LockedSource {
                    source: source.clone(),
                    checksum: None,
                },
            );
        }
        merge_resolved_packages(packages, graph.packages)?;
    } else {
        let resolver = LocalPackageResolver::new(&fetched.root);
        let package = resolver
            .resolve_package_manifest(&fetched.manifest_path)
            .with_context(|| format!("failed to resolve override package {name}"))?;
        if package.name != name {
            bail!("override for {name} resolved package {}", package.name);
        }
        if !nenjo_packages::version_satisfies(&package.version, requirement) {
            bail!(
                "{name} resolved to {}, which does not satisfy {requirement}",
                package.version
            );
        }
        for (dependency, requirement) in package.dependencies() {
            stack.push((dependency.clone(), requirement.clone()));
        }
        sources.insert(
            name.to_string(),
            LockedSource {
                source,
                checksum: None,
            },
        );
        packages.insert(name.to_string(), package);
    }
    Ok(())
}

fn merge_resolved_packages(
    target: &mut BTreeMap<String, ResolvedPackage>,
    source: BTreeMap<String, ResolvedPackage>,
) -> Result<()> {
    for (name, package) in source {
        if let Some(existing) = target.get(&name) {
            if existing.version != package.version {
                bail!(
                    "{name} resolved to both {} and {}",
                    existing.version,
                    package.version
                );
            }
            continue;
        }
        target.insert(name, package);
    }
    Ok(())
}

fn resolve_registry_records_parallel(
    records: BTreeMap<String, RegistryPackageVersion>,
) -> Result<ResolvedPackages> {
    let mut groups: BTreeMap<String, Vec<RegistryPackageVersion>> = BTreeMap::new();
    for record in records.into_values() {
        groups
            .entry(source_fetch_key(&record.source))
            .or_default()
            .push(record);
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(host_parallelism())
        .build()
        .context("failed to create registry fetch worker pool")?;

    let resolved: Result<Vec<Vec<(String, ResolvedPackage, LockedSource)>>> = pool.install(|| {
        groups
            .into_values()
            .collect::<Vec<_>>()
            .into_par_iter()
            .map(resolve_registry_record_group)
            .collect()
    });

    let mut packages = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for group in resolved? {
        for (name, package, source) in group {
            if packages.insert(name.clone(), package).is_some() {
                bail!("registry resolved duplicate package {name}");
            }
            sources.insert(name, source);
        }
    }

    Ok(ResolvedPackages { packages, sources })
}

pub(crate) fn host_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

fn resolve_registry_record_group(
    records: Vec<RegistryPackageVersion>,
) -> Result<Vec<(String, ResolvedPackage, LockedSource)>> {
    let source = records
        .first()
        .ok_or_else(|| anyhow!("registry source group was empty"))?
        .source
        .clone();
    let fetched = DefaultPackageSourceFetcher::new()
        .fetch(&source)
        .with_context(|| {
            format!(
                "failed to fetch registry source {}",
                source_fetch_key(&source)
            )
        })?;
    let resolver = LocalPackageResolver::new(&fetched.root);
    let mut packages = Vec::with_capacity(records.len());

    for record in records {
        let manifest_path = registry_record_manifest_path(&record, &fetched);
        if is_registry_manifest_path(manifest_path) {
            bail!(
                "registry source for {}@{} must point at a package manifest, not {}",
                record.name,
                record.version,
                manifest_path
            );
        }
        let package = resolver
            .resolve_package_manifest(manifest_path)
            .with_context(|| {
                format!(
                    "failed to resolve registry package {}@{} from {}",
                    record.name, record.version, manifest_path
                )
            })?;
        verify_registry_package(&record, &package)?;
        let package = project_registry_package(&record, package);
        packages.push((
            record.name,
            package,
            LockedSource {
                source: record.source,
                checksum: record.checksum,
            },
        ));
    }

    Ok(packages)
}

fn project_registry_package(
    record: &RegistryPackageVersion,
    mut package: ResolvedPackage,
) -> ResolvedPackage {
    package.name = record.name.clone();
    package.manifest.name = record.name.clone();
    package.manifest.dependencies = record.dependencies.clone();
    for module in package.modules.values_mut() {
        module.package_name = record.name.clone();
    }
    package
}

#[derive(Debug)]
struct ResolvedInstall {
    graph: ResolvedPackageGraph,
    sources: BTreeMap<String, LockedSource>,
}

#[derive(Debug)]
struct ResolvedPackages {
    packages: BTreeMap<String, ResolvedPackage>,
    sources: BTreeMap<String, LockedSource>,
}
