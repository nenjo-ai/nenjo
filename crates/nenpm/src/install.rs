use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use anyhow::{Context, anyhow};
use nenjo_packages::{LocalPackageResolver, ResolvedPackage, ResolvedPackageGraph};
use rayon::prelude::*;

use crate::dependency::DependencyManifest;
use crate::lockfile::{LockedSource, NenpmLock, lockfile_from_plan};
use crate::plan::InstallPlan;
use crate::registry::{
    RegistryPackageVersion, registry_record_manifest_path, verify_registry_package,
};
use crate::source::{
    DefaultPackageSourceFetcher, FetchMode, PackageSource, PackageSourceFetcher,
    normalize_source_paths, package_source_scope, source_fetch_key,
};

mod integrity;
mod materialize;
mod registries;

use integrity::verify_lockfile_integrity;
use materialize::materialize_packages;
use registries::ConfiguredRegistries;
pub(crate) use registries::is_registry_manifest_path;

/// Major-version behavior for `nenpm upgrade`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradePolicy {
    /// Upgrade locked packages only within their current major version.
    Compatible,
    /// Allow packages to move to a new major version when requirements permit it.
    AllowMajor,
}

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
    /// Major-version policy used when `update` is true.
    pub upgrade_policy: UpgradePolicy,
    /// Require `nenpm.lock.yml` to exist and match the resolved dependency graph.
    pub locked: bool,
    /// Explicit source fetch mode for git-backed package sources.
    pub fetch_mode: Option<FetchMode>,
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
            upgrade_policy: UpgradePolicy::Compatible,
            locked: false,
            fetch_mode: None,
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

    /// Allow `update`/`upgrade` to move locked packages to a new major version.
    pub fn allow_major_updates(mut self) -> Self {
        self.upgrade_policy = UpgradePolicy::AllowMajor;
        self
    }

    /// Set the version upgrade policy used when `update` is true.
    pub fn upgrade_policy(mut self, upgrade_policy: UpgradePolicy) -> Self {
        self.upgrade_policy = upgrade_policy;
        self
    }

    /// Require the install to match `nenpm.lock.yml`.
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Override the source fetch mode for git-backed package sources.
    pub fn fetch_mode(mut self, fetch_mode: FetchMode) -> Self {
        self.fetch_mode = Some(fetch_mode);
        self
    }
}

/// Options for resolving a dependency manifest without materializing packages.
#[derive(Debug, Clone)]
pub struct ResolveOptions {
    /// Base directory used to resolve relative local registry/source references.
    pub base_dir: PathBuf,
    /// Dependency manifest to resolve.
    pub manifest: DependencyManifest,
    /// Existing lockfile used for locked checks or update pinning.
    pub existing_lockfile: Option<NenpmLock>,
    /// Re-resolve registry versions instead of preserving lockfile pins.
    pub update: bool,
    /// Major-version policy used when `update` is true.
    pub upgrade_policy: UpgradePolicy,
    /// Require the resolved graph to match the existing lockfile.
    pub locked: bool,
    /// Explicit source fetch mode for git-backed package sources.
    pub fetch_mode: Option<FetchMode>,
}

impl ResolveOptions {
    /// Create resolve options from a base directory and parsed dependency manifest.
    pub fn new(base_dir: impl Into<PathBuf>, manifest: DependencyManifest) -> Self {
        Self {
            base_dir: base_dir.into(),
            manifest,
            existing_lockfile: None,
            update: false,
            upgrade_policy: UpgradePolicy::Compatible,
            locked: false,
            fetch_mode: None,
        }
    }

    /// Provide an existing lockfile for locked checks or update pinning.
    pub fn existing_lockfile(mut self, existing_lockfile: Option<NenpmLock>) -> Self {
        self.existing_lockfile = existing_lockfile;
        self
    }

    /// Re-resolve registry versions instead of preserving lockfile pins.
    pub fn update(mut self, update: bool) -> Self {
        self.update = update;
        self
    }

    /// Allow `update`/`upgrade` to move locked packages to a new major version.
    pub fn allow_major_updates(mut self) -> Self {
        self.upgrade_policy = UpgradePolicy::AllowMajor;
        self
    }

    /// Set the version upgrade policy used when `update` is true.
    pub fn upgrade_policy(mut self, upgrade_policy: UpgradePolicy) -> Self {
        self.upgrade_policy = upgrade_policy;
        self
    }

    /// Require the resolved graph to match the existing lockfile.
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Override the source fetch mode for git-backed package sources.
    pub fn fetch_mode(mut self, fetch_mode: FetchMode) -> Self {
        self.fetch_mode = Some(fetch_mode);
        self
    }
}

/// Result of resolving a dependency manifest.
#[derive(Debug, Clone)]
pub struct ResolveReport {
    /// Resolved install plan.
    pub plan: InstallPlan,
    /// Generated lockfile content.
    pub lockfile: NenpmLock,
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

/// Resolve packages from an already-parsed dependency manifest.
pub fn resolve(options: ResolveOptions) -> Result<ResolveReport> {
    if options.locked && options.update {
        bail!("--locked cannot be combined with update");
    }
    options
        .manifest
        .validate()
        .context("dependency manifest is invalid")?;
    let existing_lockfile = if options.update {
        None
    } else {
        options.existing_lockfile.as_ref()
    };
    if options.locked && existing_lockfile.is_none() {
        bail!("locked resolve requires an existing lockfile");
    }
    let locked_version_policy = locked_version_policy(options.update, options.upgrade_policy);
    let locked_versions = match locked_version_policy {
        LockedVersionPolicy::Ignore => BTreeMap::new(),
        LockedVersionPolicy::Exact | LockedVersionPolicy::SameMajor => options
            .existing_lockfile
            .as_ref()
            .map(NenpmLock::versions_by_package)
            .unwrap_or_default(),
    };
    let source_fetcher = match options.fetch_mode {
        Some(fetch_mode) => DefaultPackageSourceFetcher::with_fetch_mode(fetch_mode),
        None => DefaultPackageSourceFetcher::new(),
    };
    let resolved = resolve_dependency_manifest(
        &options.manifest,
        &options.base_dir,
        &locked_versions,
        locked_version_policy,
        &source_fetcher,
    )?;
    let plan = InstallPlan::from_graph(resolved.graph)?;
    let lockfile = lockfile_from_plan(&plan, &resolved.sources)?;
    if let Some(existing_lockfile) = existing_lockfile {
        verify_lockfile_integrity(existing_lockfile, &lockfile)?;
        if options.locked && existing_lockfile != &lockfile {
            bail!("nenpm.lock.yml is out of date; run nenpm install to update it");
        }
    }
    Ok(ResolveReport { plan, lockfile })
}

/// Install packages from `nenpm.yml` or `nenpm.yaml`.
pub fn install(options: InstallOptions) -> Result<InstallReport> {
    if options.locked && options.update {
        bail!("--locked cannot be combined with update");
    }
    let loaded = DependencyManifest::load_from_dir(&options.root)?;
    let lockfile_path = options.root.join("nenpm.lock.yml");
    let loaded_lockfile = if lockfile_path.exists() {
        Some(NenpmLock::load_file(&lockfile_path)?)
    } else {
        None
    };
    let existing_lockfile = if options.update {
        None
    } else {
        loaded_lockfile.as_ref()
    };
    if options.locked && existing_lockfile.is_none() {
        bail!(
            "locked install requires {}; run nenpm install first",
            lockfile_path.display()
        );
    }
    let source_fetcher = match options.fetch_mode {
        Some(fetch_mode) => DefaultPackageSourceFetcher::with_fetch_mode(fetch_mode),
        None => DefaultPackageSourceFetcher::new(),
    };
    let (plan, lockfile) = if options.locked && !options.update {
        let lockfile = loaded_lockfile
            .clone()
            .expect("locked install requires a lockfile");
        verify_locked_dependency_manifest(&loaded.manifest, &lockfile)?;
        (InstallPlan::from_lockfile(&lockfile)?, lockfile)
    } else {
        let manifest_dir = loaded
            .path
            .parent()
            .ok_or_else(|| anyhow!("dependency manifest has no parent directory"))?
            .to_path_buf();
        let resolved = resolve(
            ResolveOptions::new(manifest_dir, loaded.manifest.clone())
                .existing_lockfile(loaded_lockfile.clone())
                .update(options.update)
                .upgrade_policy(options.upgrade_policy)
                .locked(options.locked)
                .fetch_mode(source_fetcher.fetch_mode()?),
        )?;
        (resolved.plan, resolved.lockfile)
    };
    let (wrote_lockfile, materialization) = if options.dry_run {
        (false, MaterializationReport::default())
    } else {
        let materialization = materialize_packages(
            &options.root,
            &options.packages_dir,
            &lockfile,
            &source_fetcher,
        )
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockedVersionPolicy {
    Exact,
    SameMajor,
    Ignore,
}

fn verify_locked_dependency_manifest(
    manifest: &DependencyManifest,
    lockfile: &NenpmLock,
) -> Result<()> {
    let locked = lockfile
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let manifest_roots = manifest
        .dependencies
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let locked_dependency_names = lockfile
        .packages
        .iter()
        .flat_map(|package| package.dependencies.keys().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let locked_roots = lockfile
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .filter(|name| !locked_dependency_names.contains(name))
        .collect::<BTreeSet<_>>();
    if !locked_roots.is_subset(&manifest_roots) {
        bail!("nenpm.lock.yml is out of date; run nenpm install to update it");
    }

    for (name, requirement) in &manifest.dependencies {
        let Some(package) = locked.get(name.as_str()) else {
            bail!(
                "locked dependency manifest requires {name}, but it is missing from nenpm.lock.yml"
            );
        };
        if !nenjo_packages::version_satisfies(&package.version, requirement) {
            bail!(
                "locked dependency manifest requires {name} {requirement}, but nenpm.lock.yml has {}",
                package.version
            );
        }
    }

    for package in &lockfile.packages {
        for (dependency, requirement) in &package.dependencies {
            let Some(locked_dependency) = locked.get(dependency.as_str()) else {
                bail!(
                    "locked package {} depends on {dependency}, but it is missing from nenpm.lock.yml",
                    package.name
                );
            };
            if !nenjo_packages::version_satisfies(&locked_dependency.version, requirement) {
                bail!(
                    "locked package {} requires {dependency} {requirement}, but nenpm.lock.yml has {}",
                    package.name,
                    locked_dependency.version
                );
            }
        }
        for (dependency, version) in &package.resolved_dependencies {
            let Some(locked_dependency) = locked.get(dependency.as_str()) else {
                bail!(
                    "locked package {} resolved {dependency}, but it is missing from nenpm.lock.yml",
                    package.name
                );
            };
            if &locked_dependency.version != version {
                bail!(
                    "locked package {} resolved {dependency} {version}, but nenpm.lock.yml has {}",
                    package.name,
                    locked_dependency.version
                );
            }
        }
    }

    Ok(())
}

fn locked_version_policy(update: bool, upgrade_policy: UpgradePolicy) -> LockedVersionPolicy {
    if !update {
        return LockedVersionPolicy::Exact;
    }
    match upgrade_policy {
        UpgradePolicy::Compatible => LockedVersionPolicy::SameMajor,
        UpgradePolicy::AllowMajor => LockedVersionPolicy::Ignore,
    }
}

fn resolve_dependency_manifest(
    manifest: &DependencyManifest,
    manifest_dir: &Path,
    locked_versions: &BTreeMap<String, String>,
    locked_version_policy: LockedVersionPolicy,
    source_fetcher: &DefaultPackageSourceFetcher,
) -> Result<ResolvedInstall> {
    let registries = ConfiguredRegistries::load(manifest, manifest_dir, source_fetcher)?;
    let mut packages = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut registry_records: BTreeMap<String, RegistryPackageVersion> = BTreeMap::new();
    let mut registry_requirements_by_package: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut stack: Vec<(String, String)> = manifest
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
        if manifest.overrides.contains_key(&name) {
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

            let override_source = manifest
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
                source_fetcher,
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

        let requirements = registry_requirements_by_package
            .entry(name.clone())
            .or_default();
        if !requirements.contains(&requirement) {
            requirements.push(requirement);
        }

        let requirements = registry_requirements(
            requirements,
            locked_versions.get(&name).map(String::as_str),
            locked_version_policy,
        )?;
        let record = registries
            .resolve_version_matching_all(&name, &requirements)
            .with_context(|| format!("failed to resolve {name} from registry"))?;
        if registry_records.get(&name) == Some(&record) {
            continue;
        }
        for (dependency, requirement) in &record.dependencies {
            stack.push((dependency.clone(), requirement.clone()));
        }
        registry_records.insert(name, record);
    }

    let registry_resolved = resolve_registry_records_parallel(registry_records, source_fetcher)?;
    merge_resolved_packages(&mut packages, registry_resolved.packages)?;
    sources.extend(registry_resolved.sources);

    let graph = ResolvedPackageGraph {
        root_package,
        packages,
    };
    graph.validate_versions()?;
    Ok(ResolvedInstall { graph, sources })
}

fn registry_requirements(
    requirements: &[String],
    locked_version: Option<&str>,
    locked_version_policy: LockedVersionPolicy,
) -> Result<Vec<String>> {
    match (locked_version_policy, locked_version) {
        (LockedVersionPolicy::Ignore, _) | (_, None) => Ok(requirements.to_vec()),
        (LockedVersionPolicy::Exact, Some(version))
            if requirements
                .iter()
                .all(|requirement| nenjo_packages::version_satisfies(version, requirement)) =>
        {
            Ok(vec![version.to_string()])
        }
        (LockedVersionPolicy::Exact, Some(_)) => Ok(requirements.to_vec()),
        (LockedVersionPolicy::SameMajor, Some(version)) => {
            let compatibility = same_major_requirement(version)?;
            let mut requirements = requirements.to_vec();
            requirements.push(compatibility);
            Ok(requirements)
        }
    }
}

fn same_major_requirement(version: &str) -> Result<String> {
    let normalized = version.trim().trim_start_matches('v');
    let version = semver::Version::parse(normalized)
        .with_context(|| format!("locked package version {version} is not semantic"))?;
    let next_major = version
        .major
        .checked_add(1)
        .ok_or_else(|| anyhow!("locked package version {version} has no valid next major"))?;
    Ok(format!(">={version},<{next_major}.0.0"))
}

fn resolve_override_source(
    packages: &mut BTreeMap<String, ResolvedPackage>,
    sources: &mut BTreeMap<String, LockedSource>,
    stack: &mut Vec<(String, String)>,
    name: &str,
    requirement: &str,
    source: PackageSource,
    source_fetcher: &DefaultPackageSourceFetcher,
) -> Result<()> {
    let fetched = source_fetcher
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
        let package = apply_source_scope_to_override_package(&source, name, package)?;
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

fn apply_source_scope_to_override_package(
    source: &PackageSource,
    name: &str,
    mut package: ResolvedPackage,
) -> Result<ResolvedPackage> {
    if package.name == name {
        return Ok(package);
    }
    let source_scope = package_source_scope(source);
    let source_scoped_name = scoped_package_name(source_scope.as_deref(), &package.name);
    if source_scoped_name != name {
        bail!("override for {name} resolved package {}", package.name);
    }

    package.name = source_scoped_name.clone();
    package.manifest.name = source_scoped_name.clone();
    package.manifest.dependencies =
        scoped_dependencies(source_scope.as_deref(), package.dependencies());
    for module in package.modules.values_mut() {
        module.package_name = source_scoped_name.clone();
    }
    Ok(package)
}

fn scoped_package_name(scope: Option<&str>, name: &str) -> String {
    if name.starts_with('@') {
        return name.to_string();
    }
    match scope {
        Some(scope) => format!("{scope}/{name}"),
        None => name.to_string(),
    }
}

fn scoped_dependencies(
    scope: Option<&str>,
    dependencies: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    dependencies
        .iter()
        .map(|(name, requirement)| (scoped_package_name(scope, name), requirement.clone()))
        .collect()
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

pub(crate) fn resolve_registry_records_parallel(
    records: BTreeMap<String, RegistryPackageVersion>,
    source_fetcher: &DefaultPackageSourceFetcher,
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
            .map(|records| resolve_registry_record_group(records, source_fetcher))
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
    source_fetcher: &DefaultPackageSourceFetcher,
) -> Result<Vec<(String, ResolvedPackage, LockedSource)>> {
    let source = records
        .first()
        .ok_or_else(|| anyhow!("registry source group was empty"))?
        .source
        .clone();
    let fetched = source_fetcher.fetch(&source).with_context(|| {
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
        let package = apply_registry_record_to_package(&record, package);
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

fn apply_registry_record_to_package(
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
pub(crate) struct ResolvedPackages {
    pub(crate) packages: BTreeMap<String, ResolvedPackage>,
    pub(crate) sources: BTreeMap<String, LockedSource>,
}
