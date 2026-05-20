use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::dependency::{DependencyManifest, LoadedDependencyManifest};
use crate::install::{InstallOptions, InstallReport, install};
use crate::lockfile::NenpmLock;
use crate::registry::{RegistryIndex, RegistryPackageVersion};

/// Parsed package spec for `nenpm add`, such as `@nenjo/nenji@^0.1.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    /// Package name.
    pub name: String,
    /// Version requirement.
    pub requirement: String,
}

impl PackageSpec {
    /// Parse a package spec. Scoped packages must include a version separator
    /// after the package name, for example `@nenjo/nenji@^0.1.0`.
    pub fn parse(raw: &str) -> Result<Self> {
        let Some(index) = raw.rfind('@') else {
            bail!("package spec must include a version requirement, like @nenjo/nenji@^0.1.0");
        };
        if index == 0 {
            bail!("package spec must include a version requirement, like @nenjo/nenji@^0.1.0");
        }
        let (name, requirement) = raw.split_at(index);
        let requirement = &requirement[1..];
        if requirement.trim().is_empty() {
            bail!("package spec version requirement cannot be empty");
        }
        nenjo_packages::validate_package_name(name)
            .with_context(|| format!("invalid package name '{name}'"))?;
        Ok(Self {
            name: name.to_string(),
            requirement: requirement.to_string(),
        })
    }
}

/// Options for adding a dependency.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub root: PathBuf,
    pub spec: PackageSpec,
    pub dev: bool,
    pub dry_run: bool,
    pub max_concurrency: usize,
}

impl AddOptions {
    pub fn new(root: impl Into<PathBuf>, spec: PackageSpec) -> Self {
        Self {
            root: root.into(),
            spec,
            dev: false,
            dry_run: false,
            max_concurrency: 8,
        }
    }

    pub fn dev(mut self, dev: bool) -> Self {
        self.dev = dev;
        self
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency.max(1);
        self
    }
}

#[derive(Debug, Clone)]
pub struct AddReport {
    pub install: InstallReport,
}

/// Add a dependency to `nenpm.yml` or `nenpm.yaml`, then install.
pub fn add(options: AddOptions) -> Result<AddReport> {
    let mut loaded = DependencyManifest::load_from_dir(&options.root)?;
    if options.dev {
        loaded
            .manifest
            .dev_dependencies
            .insert(options.spec.name, options.spec.requirement);
    } else {
        loaded
            .manifest
            .dependencies
            .insert(options.spec.name, options.spec.requirement);
    }
    let install = install_with_manifest(
        &loaded,
        &options.root,
        options.dry_run,
        InstallOptions::new(&options.root)
            .include_dev(options.dev)
            .dry_run(options.dry_run)
            .max_concurrency(options.max_concurrency),
    )?;
    Ok(AddReport { install })
}

/// Options for removing a dependency.
#[derive(Debug, Clone)]
pub struct RemoveOptions {
    pub root: PathBuf,
    pub package: String,
    pub dry_run: bool,
    pub max_concurrency: usize,
}

impl RemoveOptions {
    pub fn new(root: impl Into<PathBuf>, package: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            package: package.into(),
            dry_run: false,
            max_concurrency: 8,
        }
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency.max(1);
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
    let removed_dev = loaded.manifest.dev_dependencies.remove(&options.package);
    if removed_runtime.is_none() && removed_dev.is_none() {
        bail!(
            "{} is not declared in dependencies or dev_dependencies",
            options.package
        );
    }
    let install = install_with_manifest(
        &loaded,
        &options.root,
        options.dry_run,
        InstallOptions::new(&options.root)
            .dry_run(options.dry_run)
            .max_concurrency(options.max_concurrency),
    )?;
    Ok(RemoveReport { install })
}

/// Re-resolve versions from the registry and rewrite the lockfile.
pub fn update(options: InstallOptions) -> Result<InstallReport> {
    install(options.update(true))
}

/// Options for listing locked packages.
#[derive(Debug, Clone)]
pub struct ListOptions {
    pub root: PathBuf,
}

impl ListOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// Load the current lockfile.
pub fn list(options: ListOptions) -> Result<NenpmLock> {
    NenpmLock::load_file(options.root.join("nenpm.lock.yml"))
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
    pub versions: Vec<RegistryPackageVersion>,
}

/// Load package metadata from the configured default registry.
pub fn info(options: InfoOptions) -> Result<PackageInfo> {
    nenjo_packages::validate_package_name(&options.package)
        .with_context(|| format!("invalid package name '{}'", options.package))?;
    let loaded = DependencyManifest::load_from_dir(&options.root)?;
    let reference = loaded
        .manifest
        .registries
        .get("default")
        .with_context(|| "missing registries.default")?;
    let manifest_dir = loaded
        .path
        .parent()
        .with_context(|| "dependency manifest has no parent directory")?;
    let registry = RegistryIndex::load(reference, manifest_dir)?;
    let versions = registry
        .packages
        .get(&options.package)
        .with_context(|| format!("registry has no package {}", options.package))?
        .iter()
        .map(|version| RegistryPackageVersion {
            name: options.package.clone(),
            version: version.version.clone(),
            source: version.source.clone(),
            dependencies: version.dependencies.clone(),
            checksum: version.checksum.clone(),
        })
        .collect();
    Ok(PackageInfo { versions })
}

fn write_dependency_manifest(loaded: &LoadedDependencyManifest, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    let content = serde_yaml::to_string(&loaded.manifest)
        .context("failed to serialize nenpm dependency manifest")?;
    fs::write(&loaded.path, content)
        .with_context(|| format!("failed to write {}", loaded.path.display()))
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

    let original = fs::read_to_string(&loaded.path)
        .with_context(|| format!("failed to read {}", loaded.path.display()))?;
    write_dependency_manifest(loaded, false)?;
    let result = install(install_options);
    fs::write(&loaded.path, original)
        .with_context(|| format!("failed to restore {}", loaded.path.display()))?;
    result.with_context(|| format!("failed to resolve dry-run manifest at {}", root.display()))
}
