use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use rayon::prelude::*;

use super::MaterializationReport;
use super::host_parallelism;
use crate::lockfile::{
    LockedPackage, NenpmLock, PackageInstallIndex, PackageInstallIndexEntry,
    package_install_path_in_packages_dir, package_instance_key,
};
use crate::source::{DefaultPackageSourceFetcher, PackageSourceFetcher};

pub(super) fn materialize_packages(
    root: &Path,
    packages_dir: &Path,
    lockfile: &NenpmLock,
    source_fetcher: &DefaultPackageSourceFetcher,
) -> Result<MaterializationReport> {
    let previous_index = load_previous_index(packages_dir)?;
    let pruned = prune_stale_package_installs(root, packages_dir, &previous_index, lockfile)?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(host_parallelism())
        .build()
        .context("failed to create package materialization worker pool")?;
    let actions: Vec<MaterializeAction> = pool.install(|| {
        lockfile
            .packages
            .par_iter()
            .map(|package| {
                materialize_package(
                    root,
                    packages_dir,
                    package,
                    previous_index.as_ref(),
                    source_fetcher,
                )
            })
            .collect::<Result<Vec<_>>>()
    })?;
    write_package_install_index(root, packages_dir, lockfile)?;
    Ok(MaterializationReport {
        installed: actions
            .iter()
            .filter(|action| matches!(action, MaterializeAction::Installed))
            .count(),
        reused: actions
            .iter()
            .filter(|action| matches!(action, MaterializeAction::Reused))
            .count(),
        pruned,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaterializeAction {
    Installed,
    Reused,
    Skipped,
}

fn load_previous_index(packages_dir: &Path) -> Result<Option<PackageInstallIndex>> {
    let index_path = package_install_index_path(packages_dir);
    if !index_path.exists() {
        return Ok(None);
    }
    Ok(Some(PackageInstallIndex::load_file(&index_path)?))
}

fn prune_stale_package_installs(
    root: &Path,
    packages_dir: &Path,
    previous_index: &Option<PackageInstallIndex>,
    lockfile: &NenpmLock,
) -> Result<usize> {
    let Some(index) = previous_index else {
        return Ok(0);
    };
    let desired: BTreeSet<_> = lockfile
        .packages
        .iter()
        .map(|package| package_instance_key(&package.name, &package.version))
        .collect();
    let mut pruned = 0;
    for entry in index.packages.values() {
        if desired.contains(&package_instance_key(&entry.name, &entry.version)) {
            continue;
        }
        let package_root = package_root_from_index(root, packages_dir, &entry.root);
        if package_root.exists() {
            fs::remove_dir_all(&package_root)
                .with_context(|| format!("failed to remove {}", package_root.display()))?;
            pruned += 1;
        }
    }
    Ok(pruned)
}

fn materialize_package(
    root: &Path,
    packages_dir: &Path,
    package: &LockedPackage,
    previous_index: Option<&PackageInstallIndex>,
    source_fetcher: &DefaultPackageSourceFetcher,
) -> Result<MaterializeAction> {
    let Some(source) = &package.source else {
        return Ok(MaterializeAction::Skipped);
    };
    let target =
        package_install_path_in_packages_dir(packages_dir, &package.name, &package.version);
    if package_install_can_be_reused(root, packages_dir, package, previous_index)? {
        return Ok(MaterializeAction::Reused);
    }
    let fetched = source_fetcher.fetch(source).with_context(|| {
        format!(
            "failed to fetch source for {}@{}",
            package.name, package.version
        )
    })?;

    let package_source = package_source_root(&fetched.root, &package.manifest_path)?;
    if same_path(&package_source, &target) {
        return Ok(MaterializeAction::Reused);
    }
    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("failed to replace {}", target.display()))?;
    }
    copy_dir_all(&package_source, &target).with_context(|| {
        format!(
            "failed to install {}@{} into {}",
            package.name,
            package.version,
            target.display()
        )
    })?;
    Ok(MaterializeAction::Installed)
}

fn package_install_can_be_reused(
    root: &Path,
    packages_dir: &Path,
    package: &LockedPackage,
    previous_index: Option<&PackageInstallIndex>,
) -> Result<bool> {
    let Some(index) = previous_index else {
        return Ok(false);
    };
    let Some(entry) = index.get_package(&package.name, &package.version) else {
        return Ok(false);
    };
    let expected_manifest_path = materialized_manifest_path(&package.manifest_path)?;
    if entry.manifest_path != expected_manifest_path {
        return Ok(false);
    }
    let package_root = package_root_from_index(root, packages_dir, &entry.root);
    if !package_root.is_dir() {
        return Ok(false);
    }
    if !file_hash_matches(&package_root.join(&entry.manifest_path), &package.hash)? {
        return Ok(false);
    }
    for module in &package.modules {
        if !file_hash_matches(&package_root.join(&module.path), &module.hash)? {
            return Ok(false);
        }
        for file in &module.files {
            if !file_hash_matches(&package_root.join(&file.path), &file.hash)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn file_hash_matches(path: &Path, expected: &str) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(nenjo_packages::sha256_hex(&bytes) == expected)
}

fn package_source_root(source_root: &Path, manifest_path: &str) -> Result<PathBuf> {
    let manifest_path = Path::new(manifest_path);
    let package_dir = manifest_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(""));
    let package_root = source_root.join(package_dir);
    if !package_root.exists() {
        return Err(anyhow!(
            "package source directory {} does not exist",
            package_root.display()
        ));
    }
    Ok(package_root)
}

fn copy_dir_all(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&source, &target)?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::copy(&source, &target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn write_package_install_index(
    root: &Path,
    packages_dir: &Path,
    lockfile: &NenpmLock,
) -> Result<()> {
    let mut packages = BTreeMap::new();
    for package in &lockfile.packages {
        let install_path =
            package_install_path_in_packages_dir(packages_dir, &package.name, &package.version);
        let root_path = relative_path(root, &install_path)?;
        packages.insert(
            package_instance_key(&package.name, &package.version),
            PackageInstallIndexEntry {
                name: package.name.clone(),
                version: package.version.clone(),
                root: root_path,
                manifest_path: materialized_manifest_path(&package.manifest_path)?,
            },
        );
    }
    let index = PackageInstallIndex {
        schema: "nenjo.package-index.v1".to_string(),
        packages,
    };
    let index_path = package_install_index_path(packages_dir);
    if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(&index)
        .context("failed to serialize package install index")?;
    fs::write(&index_path, content)
        .with_context(|| format!("failed to write {}", index_path.display()))?;
    Ok(())
}

fn package_install_index_path(packages_dir: &Path) -> PathBuf {
    packages_dir.join(".nenpm-index.json")
}

fn package_root_from_index(root: &Path, packages_dir: &Path, indexed_root: &str) -> PathBuf {
    let indexed = Path::new(indexed_root);
    if indexed.is_absolute() {
        indexed.to_path_buf()
    } else if let Ok(relative_to_packages) = indexed.strip_prefix(".nenjo/packages") {
        packages_dir.join(relative_to_packages)
    } else {
        root.join(indexed)
    }
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canonical_path = path.canonicalize().unwrap_or(path);
    let indexed = canonical_path
        .strip_prefix(&canonical_root)
        .unwrap_or(&canonical_path);
    Ok(indexed.to_string_lossy().replace('\\', "/"))
}

fn materialized_manifest_path(manifest_path: &str) -> Result<String> {
    let path = Path::new(manifest_path);
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("package manifest path '{manifest_path}' has no file name"))?;
    Ok(file_name.to_string_lossy().replace('\\', "/"))
}
