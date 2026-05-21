use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use anyhow::Context;
use nenjo_packages::{ModuleImport, PackageKind, ResolvedModule};
use serde::{Deserialize, Serialize};

use crate::plan::InstallPlan;
use crate::source::PackageSource;

/// Generated nenpm lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NenpmLock {
    /// Lockfile schema.
    pub schema: String,
    /// Locked packages in dependency-first install order.
    pub packages: Vec<LockedPackage>,
}

/// Return the local install directory for a package version.
pub fn package_install_path(root: impl AsRef<Path>, name: &str, version: &str) -> PathBuf {
    package_install_path_in_packages_dir(
        root.as_ref().join(".nenjo").join("packages"),
        name,
        version,
    )
}

/// Return the local install directory for a package version under a packages dir.
pub fn package_install_path_in_packages_dir(
    packages_dir: impl AsRef<Path>,
    name: &str,
    version: &str,
) -> PathBuf {
    let packages_dir = packages_dir.as_ref();
    if let Some((scope, package)) = name.split_once('/') {
        packages_dir
            .join(sanitize_path_component(scope))
            .join(sanitize_path_component(&package_instance_key(
                package, version,
            )))
    } else {
        packages_dir.join(sanitize_path_component(&package_instance_key(
            name, version,
        )))
    }
}

/// Return the stable index key for a package instance.
pub fn package_instance_key(name: &str, version: &str) -> String {
    format!("{name}@{version}")
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '@' | '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Locked package record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackage {
    /// Package name.
    pub name: String,
    /// Resolved package version.
    pub version: String,
    /// Package manifest path inside the source root.
    pub manifest_path: String,
    /// Hash of the package manifest.
    pub hash: String,
    /// Source used to fetch the package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PackageSource>,
    /// Optional checksum supplied by the registry for this package version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    /// Resolved package dependencies.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Exact package versions used to satisfy dependencies.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolved_dependencies: BTreeMap<String, String>,
    /// Locked modules.
    pub modules: Vec<LockedModule>,
}

/// Generated index for the materialized package install tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInstallIndex {
    /// Index schema.
    pub schema: String,
    /// Materialized packages keyed by `name@version`.
    pub packages: BTreeMap<String, PackageInstallIndexEntry>,
}

/// Materialized package location metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInstallIndexEntry {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Package root relative to the project/global install root.
    pub root: String,
    /// Package manifest path relative to the materialized package root.
    pub manifest_path: String,
}

impl PackageInstallIndex {
    /// Load a materialized package index from disk.
    pub fn load_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let index: Self = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        index.validate()?;
        Ok(index)
    }

    /// Validate the package install index schema and package names.
    pub fn validate(&self) -> Result<()> {
        if self.schema != "nenjo.package-index.v1" {
            bail!("unsupported package install index schema '{}'", self.schema);
        }
        for package in self.packages.values() {
            nenjo_packages::validate_package_name(&package.name)
                .with_context(|| format!("invalid indexed package name '{}'", package.name))?;
        }
        Ok(())
    }

    /// Find a materialized package entry by name and version.
    pub fn get_package(&self, name: &str, version: &str) -> Option<&PackageInstallIndexEntry> {
        self.packages.get(&package_instance_key(name, version))
    }
}

/// Locked module resource record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedModule {
    /// Package-relative module path.
    pub path: String,
    /// Optional resource name selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Repository-relative source path.
    pub source_path: String,
    /// Module manifest schema.
    pub schema: String,
    /// Inferred module kind.
    pub kind: PackageKind,
    /// Inferred runtime resource name.
    pub name: String,
    /// Module content hash.
    pub hash: String,
    /// Structured runtime imports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<ModuleImport>,
}

impl NenpmLock {
    /// Load a lockfile from disk.
    pub fn load_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let lockfile: Self = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        lockfile.validate()?;
        Ok(lockfile)
    }

    /// Validate the lockfile schema and package names.
    pub fn validate(&self) -> Result<()> {
        if self.schema != "nenjo.lock.v1" {
            bail!("unsupported lockfile schema '{}'", self.schema);
        }
        for package in &self.packages {
            nenjo_packages::validate_package_name(&package.name)
                .with_context(|| format!("invalid locked package name '{}'", package.name))?;
        }
        Ok(())
    }

    pub(crate) fn versions_by_package(&self) -> BTreeMap<String, String> {
        self.packages
            .iter()
            .map(|package| (package.name.clone(), package.version.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LockedSource {
    pub source: PackageSource,
    pub checksum: Option<String>,
}

pub(crate) fn lockfile_from_plan(
    plan: &InstallPlan,
    sources: &BTreeMap<String, LockedSource>,
) -> Result<NenpmLock> {
    let mut packages = Vec::new();
    for package_name in &plan.package_order {
        let package = &plan.graph.packages[package_name];
        let mut modules: Vec<LockedModule> = package
            .modules
            .iter()
            .filter(|(key, module)| *key == &module.key())
            .map(|(_, module)| lock_module(module))
            .collect();
        modules.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.resource.cmp(&right.resource))
        });
        packages.push(LockedPackage {
            name: package.name.clone(),
            version: package.version.clone(),
            manifest_path: package.path.clone(),
            hash: package.hash.clone(),
            source: sources
                .get(package_name)
                .map(|source| source.source.clone()),
            checksum: sources
                .get(package_name)
                .and_then(|source| source.checksum.clone()),
            dependencies: package.dependencies().clone(),
            resolved_dependencies: package
                .dependencies()
                .keys()
                .filter_map(|dependency| {
                    plan.graph
                        .packages
                        .get(dependency)
                        .map(|resolved| (dependency.clone(), resolved.version.clone()))
                })
                .collect(),
            modules,
        });
    }
    Ok(NenpmLock {
        schema: "nenjo.lock.v1".to_string(),
        packages,
    })
}

fn lock_module(module: &ResolvedModule) -> LockedModule {
    LockedModule {
        path: module.path.clone(),
        resource: Some(module.name().to_string()),
        source_path: module.source_path.clone(),
        schema: module.schema().to_string(),
        kind: module.kind,
        name: module.name().to_string(),
        hash: module.hash.clone(),
        imports: module.imports.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{package_install_path, package_install_path_in_packages_dir};

    #[test]
    fn package_install_path_uses_name_version_instance_dir() {
        assert_eq!(
            package_install_path("/workspace", "@nenjo/nenji", "0.1.0")
                .to_string_lossy()
                .replace('\\', "/"),
            "/workspace/.nenjo/packages/@nenjo/nenji@0.1.0"
        );
        assert_eq!(
            package_install_path("/workspace", "agent", "0.1.0")
                .to_string_lossy()
                .replace('\\', "/"),
            "/workspace/.nenjo/packages/agent@0.1.0"
        );
    }

    #[test]
    fn custom_packages_dir_package_install_path_uses_same_layout() {
        assert_eq!(
            package_install_path_in_packages_dir("/home/me/.nenjo/packages", "@acme/core", "1.2.3")
                .to_string_lossy()
                .replace('\\', "/"),
            "/home/me/.nenjo/packages/@acme/core@1.2.3"
        );
    }
}
