use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use nenjo_packages::{LocalPackageResolver, ResolvedPackage, ResolvedPackageGraph};
use rayon::prelude::*;

use crate::DEFAULT_MAX_CONCURRENCY;
use crate::dependency::{DependencyManifest, LoadedDependencyManifest};
use crate::lockfile::{LockedSource, NenpmLock, lockfile_from_plan};
use crate::plan::InstallPlan;
use crate::registry::{
    PackageRegistry, RegistryIndex, RegistryPackageVersion, registry_record_manifest_path,
    verify_registry_package,
};
use crate::source::{
    DefaultPackageSourceFetcher, PackageSource, PackageSourceFetcher, normalize_source_paths,
    source_fetch_key,
};

/// Options for installing a dependency manifest.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Directory containing `nenpm.yml` or `nenpm.yaml`.
    pub root: PathBuf,
    /// Include `dev_dependencies`.
    pub include_dev: bool,
    /// Write `nenpm.lock.yml` when false; only resolve when true.
    pub dry_run: bool,
    /// Maximum number of package source fetches to run at once.
    pub max_concurrency: usize,
    /// Re-resolve registry versions instead of preserving lockfile pins.
    pub update: bool,
}

impl InstallOptions {
    /// Create install options for a project root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            include_dev: false,
            dry_run: false,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            update: false,
        }
    }

    /// Include dev dependencies.
    pub fn include_dev(mut self, include_dev: bool) -> Self {
        self.include_dev = include_dev;
        self
    }

    /// Resolve without writing a lockfile.
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Limit concurrent package source fetches.
    pub fn max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency.max(1);
        self
    }

    /// Re-resolve registry versions instead of preserving lockfile pins.
    pub fn update(mut self, update: bool) -> Self {
        self.update = update;
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
}

/// Install packages from `nenpm.yml` or `nenpm.yaml`.
pub fn install(options: InstallOptions) -> Result<InstallReport> {
    let loaded = DependencyManifest::load_from_dir(&options.root)?;
    let lockfile_path = options.root.join("nenpm.lock.yml");
    let existing_lockfile = if options.update || !lockfile_path.exists() {
        None
    } else {
        Some(NenpmLock::load_file(&lockfile_path)?)
    };
    let locked_versions = existing_lockfile
        .as_ref()
        .map(NenpmLock::versions_by_package)
        .unwrap_or_default();
    let resolved = resolve_dependency_manifest(
        &loaded,
        options.include_dev,
        options.max_concurrency,
        &locked_versions,
    )?;
    let plan = InstallPlan::from_graph(resolved.graph)?;
    let lockfile = lockfile_from_plan(&plan, &resolved.sources)?;
    if let Some(existing_lockfile) = &existing_lockfile {
        verify_lockfile_integrity(existing_lockfile, &lockfile)?;
    }
    let wrote_lockfile = if options.dry_run {
        false
    } else {
        let content =
            serde_yaml::to_string(&lockfile).context("failed to serialize nenpm lockfile")?;
        fs::write(&lockfile_path, content)
            .with_context(|| format!("failed to write {}", lockfile_path.display()))?;
        true
    };
    Ok(InstallReport {
        manifest_path: loaded.path,
        lockfile_path,
        plan,
        lockfile,
        wrote_lockfile,
    })
}

fn resolve_dependency_manifest(
    loaded: &LoadedDependencyManifest,
    include_dev: bool,
    max_concurrency: usize,
    locked_versions: &BTreeMap<String, String>,
) -> Result<ResolvedInstall> {
    let manifest_dir = loaded
        .path
        .parent()
        .ok_or_else(|| anyhow!("dependency manifest has no parent directory"))?;
    let registry = load_default_registry(loaded)?;
    let mut packages = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut registry_records: BTreeMap<String, RegistryPackageVersion> = BTreeMap::new();
    let mut stack: Vec<(String, String)> = loaded
        .manifest
        .dependencies
        .iter()
        .map(|(name, requirement)| (name.clone(), requirement.clone()))
        .collect();
    if include_dev {
        stack.extend(
            loaded
                .manifest
                .dev_dependencies
                .iter()
                .map(|(name, requirement)| (name.clone(), requirement.clone())),
        );
    }
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

        let registry = registry
            .as_ref()
            .ok_or_else(|| anyhow!("{name} requires registry resolution, but no default registry was configured and no override was provided"))?;
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

    let registry_resolved =
        resolve_registry_records_parallel(registry_records, max_concurrency.max(1))?;
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

    if is_repository_manifest_path(&fetched.manifest_path) {
        let resolver =
            LocalPackageResolver::with_repository_path(&fetched.root, &fetched.manifest_path);
        let graph = resolver
            .resolve_package_graph(name)
            .with_context(|| format!("failed to resolve local repository package {name}"))?;
        let root = graph
            .packages
            .get(name)
            .ok_or_else(|| anyhow!("local repository graph did not include {name}"))?;
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
    max_concurrency: usize,
) -> Result<ResolvedPackages> {
    let mut groups: BTreeMap<String, Vec<RegistryPackageVersion>> = BTreeMap::new();
    for record in records.into_values() {
        groups
            .entry(source_fetch_key(&record.source))
            .or_default()
            .push(record);
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_concurrency.max(1))
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
        if is_repository_manifest_path(manifest_path) {
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

fn load_default_registry(loaded: &LoadedDependencyManifest) -> Result<Option<RegistryIndex>> {
    let Some(reference) = loaded.manifest.registries.get("default") else {
        return Ok(None);
    };
    let manifest_dir = loaded
        .path
        .parent()
        .ok_or_else(|| anyhow!("dependency manifest has no parent directory"))?;
    RegistryIndex::load(reference, manifest_dir)
        .map(Some)
        .with_context(|| format!("failed to load default registry {reference}"))
}

pub(crate) fn is_repository_manifest_path(path: &str) -> bool {
    path == "packages.yaml" || path.ends_with(".repository.yaml")
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

fn verify_lockfile_integrity(expected: &NenpmLock, actual: &NenpmLock) -> Result<()> {
    let actual_packages: BTreeMap<_, _> = actual
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();
    for expected_package in &expected.packages {
        let Some(source) = &expected_package.source else {
            continue;
        };
        if matches!(source, PackageSource::Local { .. }) {
            continue;
        }
        let Some(actual_package) = actual_packages.get(expected_package.name.as_str()) else {
            continue;
        };
        if actual_package.version != expected_package.version {
            continue;
        }
        if actual_package.hash != expected_package.hash {
            bail!(
                "locked package {}@{} manifest hash changed: expected {}, got {}",
                expected_package.name,
                expected_package.version,
                expected_package.hash,
                actual_package.hash
            );
        }
        if actual_package.dependencies != expected_package.dependencies {
            bail!(
                "locked package {}@{} dependencies changed",
                expected_package.name,
                expected_package.version
            );
        }
        verify_locked_modules(expected_package, actual_package)?;
    }
    Ok(())
}

fn verify_locked_modules(
    expected: &crate::lockfile::LockedPackage,
    actual: &crate::lockfile::LockedPackage,
) -> Result<()> {
    let actual_modules: BTreeMap<_, _> = actual
        .modules
        .iter()
        .map(|module| ((module.path.as_str(), module.resource.as_deref()), module))
        .collect();
    for expected_module in &expected.modules {
        let key = (
            expected_module.path.as_str(),
            expected_module.resource.as_deref(),
        );
        let actual_module = actual_modules.get(&key).ok_or_else(|| {
            anyhow!(
                "locked module {} {:?} is missing from {}@{}",
                expected_module.path,
                expected_module.resource,
                expected.name,
                expected.version
            )
        })?;
        if actual_module.hash != expected_module.hash {
            bail!(
                "locked module {} in {}@{} hash changed: expected {}, got {}",
                expected_module.path,
                expected.name,
                expected.version,
                expected_module.hash,
                actual_module.hash
            );
        }
    }
    Ok(())
}
