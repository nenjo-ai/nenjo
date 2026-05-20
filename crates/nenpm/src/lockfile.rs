use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
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
    /// Locked modules.
    pub modules: Vec<LockedModule>,
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
