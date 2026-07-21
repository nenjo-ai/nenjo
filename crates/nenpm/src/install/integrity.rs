use std::collections::BTreeMap;

use crate::Result;
use anyhow::anyhow;

use crate::lockfile::{LockedPackage, NenpmLock};
use crate::source::PackageSource;

pub(super) fn verify_lockfile_integrity(expected: &NenpmLock, actual: &NenpmLock) -> Result<()> {
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

fn verify_locked_modules(expected: &LockedPackage, actual: &LockedPackage) -> Result<()> {
    let actual_modules: BTreeMap<_, _> = actual
        .modules
        .iter()
        .map(|module| ((module.path.as_str(), module.resource.as_str()), module))
        .collect();
    for expected_module in &expected.modules {
        let key = (
            expected_module.path.as_str(),
            expected_module.resource.as_str(),
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
