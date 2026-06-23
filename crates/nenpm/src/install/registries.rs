use crate::Result;
use anyhow::{Context, anyhow};

use crate::dependency::LoadedDependencyManifest;
use crate::registry::RegistryIndex;
use crate::source::DefaultPackageSourceFetcher;

pub(super) struct ConfiguredRegistries {
    registries: Vec<RegistryIndex>,
}

impl ConfiguredRegistries {
    pub(super) fn load(
        loaded: &LoadedDependencyManifest,
        source_fetcher: &DefaultPackageSourceFetcher,
    ) -> Result<Self> {
        let manifest_dir = loaded
            .path
            .parent()
            .ok_or_else(|| anyhow!("dependency manifest has no parent directory"))?;
        let mut registries = Vec::new();
        for reference in &loaded.manifest.registries {
            let registry =
                RegistryIndex::load_reference_with_fetcher(reference, manifest_dir, source_fetcher)
                    .context("failed to load registry")?;
            registries.push(registry);
        }
        Ok(Self { registries })
    }

    pub(super) fn for_package(&self, package: &str) -> Result<&RegistryIndex> {
        self.registries
            .iter()
            .find(|registry| registry.packages.contains_key(package))
            .ok_or_else(|| {
                crate::NenpmError::Message(format!(
                    "{package} requires registry resolution, but no configured registry contains it"
                ))
            })
    }
}

pub(crate) fn is_registry_manifest_path(path: &str) -> bool {
    path == "packages.yaml"
        || path == "packages.yml"
        || path.ends_with(".registry.yaml")
        || path.ends_with(".registry.yml")
}
