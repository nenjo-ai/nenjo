use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use nenjo_packages::{LocalPackageResolver, ResolvedPackage, ResolvedPackageGraph};
use serde::{Deserialize, Serialize};

use crate::source::{
    DefaultPackageSourceFetcher, FetchedPackageSource, PackageSource, PackageSourceFetcher,
    fetch_bytes, normalize_fetch_url, normalize_source_paths, validate_package_source,
};

/// Registry metadata for one concrete package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryPackageVersion {
    /// Package name, for example `@nenjo/nenji`.
    pub name: String,
    /// Concrete package version.
    pub version: String,
    /// Package source.
    pub source: PackageSource,
    /// Dependency requirements indexed by package name.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Optional package checksum supplied by the registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

/// Minimal registry lookup contract used by package resolvers.
pub trait PackageRegistry {
    /// Return the best version matching a package requirement.
    fn resolve_version(&self, package: &str, requirement: &str) -> Result<RegistryPackageVersion>;
}

/// File-backed registry index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryIndex {
    /// Registry schema, currently `nenjo.registry.v1`.
    pub schema: String,
    /// Package versions keyed by package name.
    #[serde(default)]
    pub packages: BTreeMap<String, Vec<RegistryIndexVersion>>,
}

/// Package version entry in a registry index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryIndexVersion {
    /// Concrete package version.
    pub version: String,
    /// Package source.
    pub source: PackageSource,
    /// Dependency requirements indexed by package name.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Optional package checksum supplied by the registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

impl RegistryIndex {
    /// Parse a registry index from YAML.
    pub fn parse_yaml(content: &str) -> Result<Self> {
        let registry: Self =
            serde_yaml::from_str(content).context("failed to parse nenpm registry index")?;
        registry.validate()?;
        Ok(registry)
    }

    /// Load a registry index from a dependency-manifest registry value.
    pub fn load(reference: &str, base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref();
        let (content, source_base) = load_registry_content(reference, base_dir)?;
        let mut registry = Self::parse_yaml(&content)?;
        if let Some(source_base) = source_base {
            registry.normalize_relative_sources(&source_base);
        }
        Ok(registry)
    }

    /// Validate schema and package records.
    pub fn validate(&self) -> Result<()> {
        if self.schema != "nenjo.registry.v1" {
            bail!("unsupported registry schema '{}'", self.schema);
        }
        for (name, versions) in &self.packages {
            nenjo_packages::validate_package_name(name)
                .with_context(|| format!("invalid registry package name '{name}'"))?;
            if versions.is_empty() {
                bail!("registry package {name} has no versions");
            }
            for version in versions {
                for dependency in version.dependencies.keys() {
                    nenjo_packages::validate_package_name(dependency).with_context(|| {
                        format!("invalid dependency package name '{dependency}' for {name}")
                    })?;
                }
                validate_package_source(&version.source)
                    .with_context(|| format!("invalid source for {name}@{}", version.version))?;
            }
        }
        Ok(())
    }

    fn normalize_relative_sources(&mut self, base_dir: &Path) {
        for versions in self.packages.values_mut() {
            for version in versions {
                version.source = normalize_source_paths(version.source.clone(), base_dir);
                normalize_fetch_url(&mut version.source, base_dir);
            }
        }
    }
}

impl PackageRegistry for RegistryIndex {
    fn resolve_version(&self, package: &str, requirement: &str) -> Result<RegistryPackageVersion> {
        let versions = self
            .packages
            .get(package)
            .ok_or_else(|| anyhow!("registry has no package {package}"))?;
        versions
            .iter()
            .filter(|candidate| nenjo_packages::version_satisfies(&candidate.version, requirement))
            .max_by(|left, right| left.version.cmp(&right.version))
            .map(|candidate| RegistryPackageVersion {
                name: package.to_string(),
                version: candidate.version.clone(),
                source: candidate.source.clone(),
                dependencies: candidate.dependencies.clone(),
                checksum: candidate.checksum.clone(),
            })
            .ok_or_else(|| anyhow!("registry has no {package} version matching {requirement}"))
    }
}

/// In-memory registry implementation for tests and local development.
#[derive(Debug, Clone, Default)]
pub struct InMemoryRegistry {
    versions: BTreeMap<String, Vec<RegistryPackageVersion>>,
}

impl InMemoryRegistry {
    /// Create an empty in-memory registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a package version record.
    pub fn with_version(mut self, version: RegistryPackageVersion) -> Self {
        self.versions
            .entry(version.name.clone())
            .or_default()
            .push(version);
        self
    }
}

impl PackageRegistry for InMemoryRegistry {
    fn resolve_version(&self, package: &str, requirement: &str) -> Result<RegistryPackageVersion> {
        let versions = self
            .versions
            .get(package)
            .ok_or_else(|| anyhow!("registry has no package {package}"))?;
        versions
            .iter()
            .filter(|candidate| nenjo_packages::version_satisfies(&candidate.version, requirement))
            .max_by(|left, right| left.version.cmp(&right.version))
            .cloned()
            .ok_or_else(|| anyhow!("registry has no {package} version matching {requirement}"))
    }
}

/// Registry-backed resolver that consumes registry metadata before fetching
/// package sources.
#[derive(Debug, Clone)]
pub struct RegistryPackageResolver<R> {
    registry: R,
}

impl<R> RegistryPackageResolver<R>
where
    R: PackageRegistry,
{
    /// Create a registry-backed package resolver.
    pub fn new(registry: R) -> Self {
        Self { registry }
    }

    /// Resolve a package graph by fetching registry source records.
    pub fn resolve_with_fetcher<F>(
        &self,
        package: &str,
        requirement: &str,
        fetcher: &F,
    ) -> Result<ResolvedPackageGraph>
    where
        F: PackageSourceFetcher,
    {
        let mut records = BTreeMap::new();
        let mut stack = vec![(package.to_string(), requirement.to_string())];

        while let Some((name, required)) = stack.pop() {
            if let Some(existing) = records.get(&name) {
                let existing: &RegistryPackageVersion = existing;
                if !nenjo_packages::version_satisfies(&existing.version, &required) {
                    bail!(
                        "{name} was already resolved to {}, which does not satisfy {required}",
                        existing.version
                    );
                }
                continue;
            }

            let record = self.registry.resolve_version(&name, &required)?;
            for (dependency, requirement) in &record.dependencies {
                stack.push((dependency.clone(), requirement.clone()));
            }
            records.insert(name, record);
        }

        let mut packages = BTreeMap::new();
        for record in records.into_values() {
            let fetched = fetcher
                .fetch(&record.source)
                .with_context(|| format!("failed to fetch {}@{}", record.name, record.version))?;
            let resolver = LocalPackageResolver::new(&fetched.root);
            let manifest_path = registry_record_manifest_path(&record, &fetched);
            if crate::install::is_repository_manifest_path(manifest_path) {
                bail!(
                    "registry source for {}@{} must point at a package manifest, not {}",
                    record.name,
                    record.version,
                    manifest_path
                );
            }
            let resolved = resolver
                .resolve_package_manifest(manifest_path)
                .with_context(|| {
                    format!(
                        "failed to resolve {}@{} from {}",
                        record.name, record.version, manifest_path
                    )
                })?;
            verify_registry_package(&record, &resolved)?;
            packages.insert(record.name, resolved);
        }

        let graph = ResolvedPackageGraph {
            root_package: package.to_string(),
            packages,
        };
        graph.validate_versions()?;
        Ok(graph)
    }

    /// Resolve a package graph using local source records from the registry.
    pub fn resolve_local_sources(
        &self,
        package: &str,
        requirement: &str,
    ) -> Result<ResolvedPackageGraph> {
        self.resolve_with_fetcher(package, requirement, &DefaultPackageSourceFetcher::new())
    }
}

pub(crate) fn registry_record_manifest_path<'a>(
    record: &'a RegistryPackageVersion,
    fetched: &'a FetchedPackageSource,
) -> &'a str {
    record
        .source
        .manifest_path()
        .unwrap_or(fetched.manifest_path.as_str())
}

pub(crate) fn verify_registry_package(
    record: &RegistryPackageVersion,
    package: &ResolvedPackage,
) -> Result<()> {
    if package.name != record.name {
        bail!(
            "registry resolved {}, but source manifest declares {}",
            record.name,
            package.name
        );
    }
    if package.version != record.version {
        bail!(
            "registry resolved {}@{}, but source manifest declares {}",
            record.name,
            record.version,
            package.version
        );
    }
    if package.dependencies() != &record.dependencies {
        bail!(
            "registry metadata for {}@{} does not match source manifest dependencies",
            record.name,
            record.version
        );
    }
    if let Some(expected) = &record.checksum
        && &package.hash != expected
    {
        bail!(
            "registry checksum for {}@{} does not match source manifest hash: expected {}, got {}",
            record.name,
            record.version,
            expected,
            package.hash
        );
    }
    Ok(())
}

fn load_registry_content(reference: &str, base_dir: &Path) -> Result<(String, Option<PathBuf>)> {
    let raw = reference.trim();
    if raw.is_empty() {
        bail!("registry reference cannot be empty");
    }

    if raw.starts_with("http://") || raw.starts_with("https://") {
        let bytes = fetch_bytes(raw)?;
        let content = String::from_utf8(bytes)
            .with_context(|| format!("registry {raw} did not contain valid UTF-8"))?;
        return Ok((content, None));
    }

    let path = if let Some(path) = raw.strip_prefix("file://") {
        PathBuf::from(path)
    } else if let Some(path) = raw.strip_prefix("file:") {
        PathBuf::from(path)
    } else {
        PathBuf::from(raw)
    };
    let path = if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    };
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read registry {}", path.display()))?;
    let source_base = path
        .parent()
        .ok_or_else(|| anyhow!("registry path has no parent directory"))?
        .to_path_buf();
    Ok((content, Some(source_base)))
}
