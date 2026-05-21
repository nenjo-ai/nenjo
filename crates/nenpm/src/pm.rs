use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use anyhow::Context;

use crate::dependency::{DependencyManifest, LoadedDependencyManifest, RegistryReference};
use crate::install::{InstallOptions, InstallReport, install};
use crate::registry::{RegistryIndex, RegistryPackageVersion};
use crate::source::{
    DefaultPackageSourceFetcher, PackageSource, PackageSourceFetcher, package_source_scope,
};

/// Options for initializing a dependency manifest.
#[derive(Debug, Clone)]
pub struct InitOptions {
    pub root: PathBuf,
}

impl InitOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub manifest_path: PathBuf,
}

/// Create a starter `nenpm.yml`.
pub fn init(options: InitOptions) -> Result<InitReport> {
    fs::create_dir_all(&options.root)
        .with_context(|| format!("failed to create {}", options.root.display()))?;
    let yml = options.root.join("nenpm.yml");
    let yaml = options.root.join("nenpm.yaml");
    if yml.exists() || yaml.exists() {
        bail!(
            "dependency manifest already exists in {}",
            options.root.display()
        );
    }
    let manifest = DependencyManifest {
        schema: "nenjo.dependencies.v1".to_string(),
        dependencies: Default::default(),
        overrides: Default::default(),
        registries: Vec::new(),
    };
    let content = serde_yaml::to_string(&manifest)
        .context("failed to serialize starter dependency manifest")?;
    fs::write(&yml, content).with_context(|| format!("failed to write {}", yml.display()))?;
    Ok(InitReport { manifest_path: yml })
}

/// Parsed add spec for `nenpm add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    pub target: AddTarget,
}

/// Target selected by `nenpm add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddTarget {
    Registry {
        org: String,
    },
    Package {
        org: String,
        name: String,
        requirement: Option<String>,
    },
    RegistryPackages {
        org: String,
    },
}

impl PackageSpec {
    /// Parse an add spec.
    ///
    /// Supported forms:
    ///
    /// - `@org` adds the GitHub-backed `org/packages` registry.
    /// - `@org/name` adds one package, resolving the latest version.
    /// - `@org/name@^1.2.3` adds one package with an explicit requirement.
    /// - `@org/*` adds every package in the registry.
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        let Some(rest) = raw.strip_prefix('@') else {
            bail!("add spec must start with @org, @org/package, or @org/*");
        };
        if rest.is_empty() {
            bail!("add spec must include an org");
        }
        let Some((org, package_spec)) = rest.split_once('/') else {
            validate_org(rest)?;
            return Ok(Self {
                target: AddTarget::Registry {
                    org: rest.to_string(),
                },
            });
        };
        validate_org(org)?;
        if package_spec == "*" {
            return Ok(Self {
                target: AddTarget::RegistryPackages {
                    org: org.to_string(),
                },
            });
        }
        let (name, requirement) = match package_spec.rsplit_once('@') {
            Some((name, requirement)) => {
                if name.is_empty() || requirement.trim().is_empty() {
                    bail!("package spec version requirement cannot be empty");
                }
                (name, Some(requirement.to_string()))
            }
            None => (package_spec, None),
        };
        let package_name = format!("@{org}/{name}");
        nenjo_packages::validate_package_name(&package_name)
            .with_context(|| format!("invalid package name '{package_name}'"))?;
        Ok(Self {
            target: AddTarget::Package {
                org: org.to_string(),
                name: package_name,
                requirement,
            },
        })
    }
}

/// Options for adding a dependency.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub root: PathBuf,
    pub packages_dir: PathBuf,
    pub spec: PackageSpec,
    pub dry_run: bool,
    pub reference: String,
    pub manifest_path: String,
}

impl AddOptions {
    pub fn new(root: impl Into<PathBuf>, spec: PackageSpec) -> Self {
        let root = root.into();
        let packages_dir = root.join(".nenjo").join("packages");
        Self {
            root,
            packages_dir,
            spec,
            dry_run: false,
            reference: "main".to_string(),
            manifest_path: "packages.yaml".to_string(),
        }
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn packages_dir(mut self, packages_dir: impl Into<PathBuf>) -> Self {
        self.packages_dir = packages_dir.into();
        self
    }

    pub fn reference(mut self, reference: impl Into<String>) -> Self {
        self.reference = reference.into();
        self
    }

    pub fn manifest_path(mut self, manifest_path: impl Into<String>) -> Self {
        self.manifest_path = manifest_path.into();
        self
    }
}

#[derive(Debug, Clone)]
pub struct AddReport {
    pub manifest_path: PathBuf,
    pub registry: RegistryReference,
    pub registry_added: bool,
    pub dependencies_added: Vec<String>,
    pub install: Option<InstallReport>,
}

/// Add a registry and optionally package dependencies to `nenpm.yml` or
/// `nenpm.yaml`, then install when package dependencies were added.
pub fn add(options: AddOptions) -> Result<AddReport> {
    let manifest_path = dependency_manifest_path_for_write(&options.root);
    let mut loaded = load_or_create_dependency_manifest(&manifest_path)?;
    let (org, requested_packages) = match &options.spec.target {
        AddTarget::Registry { org } => (org.as_str(), RequestedPackages::None),
        AddTarget::Package {
            org,
            name,
            requirement,
        } => (
            org.as_str(),
            RequestedPackages::One {
                name: name.clone(),
                requirement: requirement.clone(),
            },
        ),
        AddTarget::RegistryPackages { org } => (org.as_str(), RequestedPackages::All),
    };
    let (registry, registry_added) = ensure_registry_for_org(
        &mut loaded.manifest,
        org,
        &options.reference,
        &options.manifest_path,
    )?;
    let mut dependencies_added = Vec::new();

    match requested_packages {
        RequestedPackages::None => {
            write_dependency_manifest(&loaded, options.dry_run)?;
            Ok(AddReport {
                manifest_path,
                registry,
                registry_added,
                dependencies_added,
                install: None,
            })
        }
        RequestedPackages::One { name, requirement } => {
            let registry_index = load_add_registry(&registry, &manifest_path)?;
            let requirement = match requirement {
                Some(requirement) => requirement,
                None => latest_caret_requirement(&registry_index, &name)?,
            };
            add_dependency(&mut loaded.manifest, name.clone(), requirement)?;
            dependencies_added.push(name);
            let install = install_with_manifest(
                &loaded,
                &options.root,
                options.dry_run,
                InstallOptions::new(&options.root)
                    .packages_dir(options.packages_dir)
                    .dry_run(options.dry_run),
            )?;
            Ok(AddReport {
                manifest_path,
                registry,
                registry_added,
                dependencies_added,
                install: Some(install),
            })
        }
        RequestedPackages::All => {
            let registry_index = load_add_registry(&registry, &manifest_path)?;
            for name in registry_index.packages.keys() {
                let requirement = latest_caret_requirement(&registry_index, name)?;
                add_dependency(&mut loaded.manifest, name.clone(), requirement)?;
                dependencies_added.push(name.clone());
            }
            let install = install_with_manifest(
                &loaded,
                &options.root,
                options.dry_run,
                InstallOptions::new(&options.root)
                    .packages_dir(options.packages_dir)
                    .dry_run(options.dry_run),
            )?;
            Ok(AddReport {
                manifest_path,
                registry,
                registry_added,
                dependencies_added,
                install: Some(install),
            })
        }
    }
}

enum RequestedPackages {
    None,
    One {
        name: String,
        requirement: Option<String>,
    },
    All,
}

fn validate_org(org: &str) -> Result<()> {
    if org.is_empty() || org.contains('/') {
        bail!("org must not be empty or contain /");
    }
    let scope = format!("@{org}/package");
    nenjo_packages::validate_package_name(&scope)
        .with_context(|| format!("invalid org '{org}'"))?;
    Ok(())
}

fn ensure_registry_for_org(
    manifest: &mut DependencyManifest,
    org: &str,
    reference: &str,
    manifest_path: &str,
) -> Result<(RegistryReference, bool)> {
    let scope = format!("@{org}");
    if let Some(existing) = manifest
        .registries
        .iter()
        .find(|registry| registry_matches_scope(registry, &scope))
        .cloned()
    {
        return Ok((existing, false));
    }
    let registry = github_registry_reference(org, reference, manifest_path)?;
    manifest.registries.push(registry.clone());
    Ok((registry, true))
}

fn registry_matches_scope(reference: &RegistryReference, scope: &str) -> bool {
    match reference {
        RegistryReference::Source(source) => package_source_scope(source)
            .as_deref()
            .is_some_and(|value| value == scope),
        RegistryReference::Index(_) => false,
    }
}

fn github_registry_reference(
    org: &str,
    reference: &str,
    manifest_path: &str,
) -> Result<RegistryReference> {
    Ok(RegistryReference::Source(PackageSource::Git {
        url: format!("https://github.com/{org}/packages.git"),
        reference: reference.to_string(),
        manifest_path: nenjo_packages::validate_source_path(manifest_path)
            .context("registry manifest path is invalid")?,
    }))
}

fn load_add_registry(
    registry: &RegistryReference,
    dependency_manifest_path: &Path,
) -> Result<RegistryIndex> {
    let manifest_dir = dependency_manifest_path
        .parent()
        .with_context(|| "dependency manifest has no parent directory")?;
    Ok(RegistryIndex::load_reference(registry, manifest_dir).context("failed to load registry")?)
}

fn latest_caret_requirement(registry: &RegistryIndex, name: &str) -> Result<String> {
    let versions = registry
        .packages
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("registry has no package {name}"))?;
    let version = versions
        .iter()
        .map(|version| version.version.as_str())
        .max()
        .ok_or_else(|| anyhow::anyhow!("registry package {name} has no versions"))?;
    Ok(format!("^{version}"))
}

fn add_dependency(
    manifest: &mut DependencyManifest,
    name: String,
    requirement: String,
) -> Result<()> {
    if requirement.trim().is_empty() {
        bail!("package requirement cannot be empty");
    }
    manifest.dependencies.insert(name, requirement);
    Ok(())
}

fn count_installed_packages(packages_dir: &Path) -> Result<usize> {
    let index_path = packages_dir.join(".nenpm-index.json");
    if index_path.exists() {
        let index = crate::PackageInstallIndex::load_file(&index_path)?;
        return Ok(index.packages.len());
    }
    if !packages_dir.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in fs::read_dir(packages_dir)
        .with_context(|| format!("failed to read {}", packages_dir.display()))?
    {
        let entry = entry?;
        if entry.file_name() == ".nenpm-index.json" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            count += count_package_dirs(entry.path())?;
        }
    }
    Ok(count)
}

fn count_package_dirs(path: PathBuf) -> Result<usize> {
    if path.join("nenjo.package.yaml").exists() || path.join("package.yaml").exists() {
        return Ok(1);
    }
    let mut count = 0;
    for entry in
        fs::read_dir(&path).with_context(|| format!("failed to read {}", path.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += count_package_dirs(entry.path())?;
        }
    }
    Ok(count)
}

/// Options for removing a dependency.
#[derive(Debug, Clone)]
pub struct RemoveOptions {
    pub root: PathBuf,
    pub packages_dir: PathBuf,
    pub package: String,
    pub dry_run: bool,
}

impl RemoveOptions {
    pub fn new(root: impl Into<PathBuf>, package: impl Into<String>) -> Self {
        let root = root.into();
        let packages_dir = root.join(".nenjo").join("packages");
        Self {
            root,
            packages_dir,
            package: package.into(),
            dry_run: false,
        }
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn packages_dir(mut self, packages_dir: impl Into<PathBuf>) -> Self {
        self.packages_dir = packages_dir.into();
        self
    }
}

#[derive(Debug, Clone)]
pub struct RemoveReport {
    pub install: InstallReport,
}

/// Remove a dependency from `nenpm.yml` or `nenpm.yaml`, then install.
pub fn remove(options: RemoveOptions) -> Result<RemoveReport> {
    nenjo_packages::validate_package_name(&options.package)
        .with_context(|| format!("invalid package name '{}'", options.package))?;
    let mut loaded = DependencyManifest::load_from_dir(&options.root)?;
    let removed_runtime = loaded.manifest.dependencies.remove(&options.package);
    if removed_runtime.is_none() {
        bail!("{} is not declared in dependencies", options.package);
    }
    let install = install_with_manifest(
        &loaded,
        &options.root,
        options.dry_run,
        InstallOptions::new(&options.root)
            .packages_dir(options.packages_dir)
            .dry_run(options.dry_run),
    )?;
    Ok(RemoveReport { install })
}

/// Re-resolve versions from the registry and rewrite the lockfile.
pub fn update(options: InstallOptions) -> Result<InstallReport> {
    install(options.update(true))
}

/// Options for cleaning derived package install artifacts.
#[derive(Debug, Clone)]
pub struct CleanOptions {
    pub root: PathBuf,
    pub packages_dir: PathBuf,
    pub dry_run: bool,
}

impl CleanOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let packages_dir = root.join(".nenjo").join("packages");
        Self {
            root,
            packages_dir,
            dry_run: false,
        }
    }

    pub fn packages_dir(mut self, packages_dir: impl Into<PathBuf>) -> Self {
        self.packages_dir = packages_dir.into();
        self
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanReport {
    pub packages_dir: PathBuf,
    pub package_count: usize,
    pub removed: bool,
    pub dry_run: bool,
}

/// Remove derived package install artifacts without touching dependency or lock files.
pub fn clean(options: CleanOptions) -> Result<CleanReport> {
    let packages_dir = options.packages_dir;
    let package_count = count_installed_packages(&packages_dir)?;
    let removed = packages_dir.exists() && !options.dry_run;
    if removed {
        fs::remove_dir_all(&packages_dir)
            .with_context(|| format!("failed to remove {}", packages_dir.display()))?;
    }
    Ok(CleanReport {
        packages_dir,
        package_count,
        removed,
        dry_run: options.dry_run,
    })
}

/// Options for listing configured registry packages.
#[derive(Debug, Clone)]
pub struct ListOptions {
    pub root: PathBuf,
    pub registry: Option<String>,
}

impl ListOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            registry: None,
        }
    }

    pub fn registry(mut self, registry: impl Into<String>) -> Self {
        self.registry = Some(registry.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRegistryPackage {
    pub name: String,
    pub versions: Vec<String>,
}

/// List packages available from configured registries.
pub fn list(options: ListOptions) -> Result<Vec<ListedRegistryPackage>> {
    let loaded = DependencyManifest::load_from_dir(&options.root)?;
    let manifest_dir = loaded
        .path
        .parent()
        .with_context(|| "dependency manifest has no parent directory")?;
    let mut packages = std::collections::BTreeMap::<String, Vec<String>>::new();
    for reference in &loaded.manifest.registries {
        if let Some(selector) = &options.registry
            && !registry_matches_selector(reference, selector)?
        {
            continue;
        }
        let registry = RegistryIndex::load_reference(reference, manifest_dir)
            .context("failed to load registry")?;
        for (name, versions) in registry.packages {
            packages
                .entry(name)
                .or_default()
                .extend(versions.into_iter().map(|version| version.version));
        }
    }
    if packages.is_empty()
        && let Some(selector) = &options.registry
    {
        bail!("no configured registry matches {selector}");
    }
    Ok(packages
        .into_iter()
        .map(|(name, mut versions)| {
            versions.sort();
            versions.dedup();
            ListedRegistryPackage { name, versions }
        })
        .collect())
}

fn registry_matches_selector(reference: &RegistryReference, selector: &str) -> Result<bool> {
    let selector = normalize_registry_selector(selector)?;
    Ok(match reference {
        RegistryReference::Source(source) => package_source_scope(source)
            .as_deref()
            .is_some_and(|scope| scope == selector),
        RegistryReference::Index(_) => false,
    })
}

fn normalize_registry_selector(selector: &str) -> Result<&str> {
    let selector = selector.trim();
    if !selector.starts_with('@') || selector.contains('/') {
        bail!("registry selector must look like @scope");
    }
    Ok(selector)
}

/// Options for reading registry package metadata.
#[derive(Debug, Clone)]
pub struct InfoOptions {
    pub root: PathBuf,
    pub package: String,
}

impl InfoOptions {
    pub fn new(root: impl Into<PathBuf>, package: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            package: package.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub versions: Vec<PackageInfoVersion>,
}

#[derive(Debug, Clone)]
pub struct PackageInfoVersion {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub source: PackageSource,
    pub dependencies: std::collections::BTreeMap<String, String>,
    pub checksum: Option<String>,
    pub modules: Vec<PackageInfoModule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfoModule {
    pub kind: nenjo_packages::PackageKind,
    pub name: String,
    pub path: String,
    pub schema: String,
    pub description: Option<String>,
}

/// Load package metadata from configured registries.
pub fn info(options: InfoOptions) -> Result<PackageInfo> {
    nenjo_packages::validate_package_name(&options.package)
        .with_context(|| format!("invalid package name '{}'", options.package))?;
    let loaded = DependencyManifest::load_from_dir(&options.root)?;
    let manifest_dir = loaded
        .path
        .parent()
        .with_context(|| "dependency manifest has no parent directory")?;
    for reference in &loaded.manifest.registries {
        let registry = RegistryIndex::load_reference(reference, manifest_dir)
            .context("failed to load registry")?;
        if let Some(versions) = registry.packages.get(&options.package) {
            let versions = versions
                .iter()
                .map(|version| {
                    let record = RegistryPackageVersion {
                        name: options.package.clone(),
                        version: version.version.clone(),
                        source: version.source.clone(),
                        dependencies: version.dependencies.clone(),
                        checksum: version.checksum.clone(),
                    };
                    package_info_version(record)
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(PackageInfo { versions });
        }
    }
    bail!("no configured registry contains {}", options.package);
}

fn package_info_version(record: RegistryPackageVersion) -> Result<PackageInfoVersion> {
    let fetched = DefaultPackageSourceFetcher::new()
        .fetch(&record.source)
        .with_context(|| format!("failed to fetch {}@{}", record.name, record.version))?;
    let resolver = nenjo_packages::LocalPackageResolver::new(&fetched.root);
    let manifest_path = crate::registry::registry_record_manifest_path(&record, &fetched);
    let package = resolver
        .resolve_package_manifest(manifest_path)
        .with_context(|| {
            format!(
                "failed to resolve {}@{} from {}",
                record.name, record.version, manifest_path
            )
        })?;
    crate::registry::verify_registry_package(&record, &package)?;

    let mut modules: Vec<_> = package
        .modules
        .iter()
        .filter(|(key, module)| *key == &module.key())
        .map(|(_, module)| module)
        .map(|module| PackageInfoModule {
            kind: module.kind,
            name: module.name().to_string(),
            path: module.path.clone(),
            schema: module.schema().to_string(),
            description: module_manifest_string(module, "description"),
        })
        .collect();
    modules.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.name.cmp(&right.name))
    });

    Ok(PackageInfoVersion {
        name: record.name,
        version: record.version,
        description: package.manifest.description.clone(),
        source: record.source,
        dependencies: record.dependencies,
        checksum: record.checksum,
        modules,
    })
}

fn module_manifest_string(module: &nenjo_packages::ResolvedModule, key: &str) -> Option<String> {
    module
        .manifest
        .manifest
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn write_dependency_manifest(loaded: &LoadedDependencyManifest, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    let content = serde_yaml::to_string(&loaded.manifest)
        .context("failed to serialize nenpm dependency manifest")?;
    Ok(fs::write(&loaded.path, content)
        .with_context(|| format!("failed to write {}", loaded.path.display()))?)
}

fn dependency_manifest_path_for_write(root: &Path) -> PathBuf {
    let yml = root.join("nenpm.yml");
    if yml.exists() {
        yml
    } else {
        root.join("nenpm.yml")
    }
}

fn load_or_create_dependency_manifest(path: &Path) -> Result<LoadedDependencyManifest> {
    if path.exists() {
        DependencyManifest::load_file(path)
    } else {
        Ok(LoadedDependencyManifest {
            path: path.to_path_buf(),
            manifest: DependencyManifest {
                schema: "nenjo.dependencies.v1".to_string(),
                dependencies: Default::default(),
                overrides: Default::default(),
                registries: Vec::new(),
            },
        })
    }
}

fn install_with_manifest(
    loaded: &LoadedDependencyManifest,
    root: &Path,
    dry_run: bool,
    install_options: InstallOptions,
) -> Result<InstallReport> {
    if !dry_run {
        write_dependency_manifest(loaded, false)?;
        return install(install_options);
    }

    let original = if loaded.path.exists() {
        Some(
            fs::read_to_string(&loaded.path)
                .with_context(|| format!("failed to read {}", loaded.path.display()))?,
        )
    } else {
        None
    };
    write_dependency_manifest(loaded, false)?;
    let result = install(install_options);
    match original {
        Some(original) => fs::write(&loaded.path, original)
            .with_context(|| format!("failed to restore {}", loaded.path.display()))?,
        None => {
            if loaded.path.exists() {
                fs::remove_file(&loaded.path)
                    .with_context(|| format!("failed to remove {}", loaded.path.display()))?;
            }
        }
    }
    Ok(result
        .with_context(|| format!("failed to resolve dry-run manifest at {}", root.display()))?)
}
