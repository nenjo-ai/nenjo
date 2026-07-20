use crate::Result;
use std::cmp::Ordering;
use std::path::Path;

use anyhow::{Context, anyhow};
use semver::Version;

use crate::dependency::DependencyManifest;
use crate::registry::{RegistryIndex, RegistryPackageVersion};
use crate::source::DefaultPackageSourceFetcher;

pub(super) struct ConfiguredRegistries {
    registries: Vec<RegistryIndex>,
}

impl ConfiguredRegistries {
    pub(super) fn load(
        manifest: &DependencyManifest,
        base_dir: &Path,
        source_fetcher: &DefaultPackageSourceFetcher,
    ) -> Result<Self> {
        let mut registries = Vec::new();
        for reference in &manifest.registries {
            let registry =
                RegistryIndex::load_reference_with_fetcher(reference, base_dir, source_fetcher)
                    .context("failed to load registry")?;
            registries.push(registry);
        }
        Ok(Self { registries })
    }

    pub(super) fn resolve_version_matching_all(
        &self,
        package: &str,
        requirements: &[String],
    ) -> Result<RegistryPackageVersion> {
        let matching_registries = self
            .registries
            .iter()
            .filter(|registry| registry.packages.contains_key(package))
            .collect::<Vec<_>>();
        if matching_registries.is_empty() {
            return Err(crate::NenpmError::Message(format!(
                "{package} requires registry resolution, but no configured registry contains it"
            )));
        }

        let mut best = None;
        for registry in matching_registries {
            let Ok(candidate) = registry.resolve_version_matching_all(package, requirements) else {
                continue;
            };
            let replace = best
                .as_ref()
                .is_none_or(|current: &RegistryPackageVersion| {
                    compare_versions(&candidate.version, &current.version) == Ordering::Greater
                });
            if replace {
                best = Some(candidate);
            }
        }
        best.ok_or_else(|| {
            anyhow!(
                "no configured registry has {package} matching {}",
                requirements.join(" and ")
            )
            .into()
        })
    }
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_semver = Version::parse(left.trim().trim_start_matches('v'));
    let right_semver = Version::parse(right.trim().trim_start_matches('v'));
    match (left_semver, right_semver) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Ok(_), Err(_)) => Ordering::Greater,
        (Err(_), Ok(_)) => Ordering::Less,
        (Err(_), Err(_)) => left.cmp(&right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegistryIndexVersion;
    use crate::source::PackageSource;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn registry(package: &str, version: &str) -> RegistryIndex {
        RegistryIndex {
            schema: "nenjo.registry.v1".to_string(),
            packages: BTreeMap::from([(
                package.to_string(),
                vec![RegistryIndexVersion {
                    version: version.to_string(),
                    source: PackageSource::Local {
                        root: PathBuf::from("."),
                        manifest_path: "packages/pkg/nenjo.package.yaml".to_string(),
                        scope: Some("@acme".to_string()),
                    },
                    dependencies: BTreeMap::new(),
                    checksum: None,
                }],
            )]),
        }
    }

    #[test]
    fn resolves_compatible_version_from_later_registry() {
        let registries = ConfiguredRegistries {
            registries: vec![
                registry("@acme/pkg", "1.1.0"),
                registry("@acme/pkg", "1.3.0"),
            ],
        };

        let resolved = registries
            .resolve_version_matching_all("@acme/pkg", &["^1.2.0".to_string()])
            .expect("later registry has a compatible version");

        assert_eq!(resolved.version, "1.3.0");
    }
}

pub(crate) fn is_registry_manifest_path(path: &str) -> bool {
    path == "packages.yaml"
        || path == "packages.yml"
        || path.ends_with(".registry.yaml")
        || path.ends_with(".registry.yml")
}
