use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use anyhow::{Context, anyhow};
use nenjo_packages::{LocalPackageResolver, ResolvedPackage, ResolvedPackageGraph};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::dependency::RegistryReference;
use crate::source::{
    DefaultPackageSourceFetcher, FetchedPackageSource, PackageSource, PackageSourceFetcher,
    fetch_bytes, normalize_fetch_url, normalize_source_paths, package_source_scope,
    validate_package_source,
};

/// Registry metadata for one concrete package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryPackageVersion {
    /// Fully resolved package name, for example `@nenjo-ai/nenji`.
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
    /// Package versions keyed by package name. Repo-backed registry indexes use
    /// unscoped keys; scoped consumer names are derived from the registry
    /// source.
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
        let mut registry = match Self::parse_yaml(&content) {
            Ok(registry) => registry,
            Err(error) => {
                let Some(source_base) = source_base.as_ref() else {
                    return Err(error);
                };
                let source = PackageSource::Local {
                    root: source_base.root.clone(),
                    manifest_path: source_base.manifest_path.clone(),
                    scope: None,
                };
                Self::load_registry_source(&source, base_dir)
                    .with_context(|| "failed to load repository-style registry source")?
            }
        };
        if let Some(source_base) = source_base {
            registry.normalize_relative_sources(&source_base.root);
        }
        Ok(registry)
    }

    /// Load a registry from a typed registry reference.
    pub fn load_reference(
        reference: &RegistryReference,
        base_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let base_dir = base_dir.as_ref();
        match reference {
            RegistryReference::Index(reference) => Self::load(reference, base_dir),
            RegistryReference::Source(source) => Self::load_registry_source(source, base_dir),
        }
    }

    fn load_registry_source(source: &PackageSource, base_dir: &Path) -> Result<Self> {
        let source = normalize_source_paths(source.clone(), base_dir);
        let fetched = DefaultPackageSourceFetcher::new()
            .fetch(&source)
            .context("failed to fetch package registry source")?;
        if !crate::install::is_registry_manifest_path(&fetched.manifest_path) {
            bail!(
                "package registry source must point at packages.yaml, *.registry.yaml, or *.registry.yml, got {}",
                fetched.manifest_path
            );
        }
        if matches!(source, PackageSource::Local { scope: None, .. }) {
            bail!("local registry source must declare scope");
        }
        let resolver =
            LocalPackageResolver::with_registry_path(&fetched.root, &fetched.manifest_path);
        let registry = resolver.load_registry()?;
        let mut packages = BTreeMap::new();
        let repo_scope = package_source_scope(&source);
        for (name, manifest_path) in registry.packages {
            let resolved = resolver
                .resolve_package_manifest(&manifest_path)
                .with_context(|| format!("failed to resolve registry package {name}"))?;
            if resolved.name != name {
                bail!(
                    "registry maps {name} to {manifest_path}, but package manifest declares {}",
                    resolved.name
                );
            }
            let registry_name = scoped_package_name(repo_scope.as_deref(), &name);
            packages
                .entry(registry_name)
                .or_insert_with(Vec::new)
                .push(RegistryIndexVersion {
                    version: resolved.version.clone(),
                    source: source_with_manifest_path(&source, manifest_path),
                    dependencies: scoped_dependencies(
                        repo_scope.as_deref(),
                        resolved.dependencies(),
                    ),
                    checksum: None,
                });
        }
        Ok(Self {
            schema: "nenjo.registry.v1".to_string(),
            packages,
        })
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

fn scoped_package_name(scope: Option<&str>, name: &str) -> String {
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

fn source_with_manifest_path(source: &PackageSource, manifest_path: String) -> PackageSource {
    match source {
        PackageSource::Git { url, reference, .. } => PackageSource::Git {
            url: url.clone(),
            reference: reference.clone(),
            manifest_path,
        },
        PackageSource::Artifact { url, checksum, .. } => PackageSource::Artifact {
            url: url.clone(),
            checksum: checksum.clone(),
            manifest_path,
        },
        PackageSource::Local { root, scope, .. } => PackageSource::Local {
            root: root.clone(),
            manifest_path,
            scope: scope.clone(),
        },
        PackageSource::Remote { url, checksum } => PackageSource::Remote {
            url: url.clone(),
            checksum: checksum.clone(),
        },
    }
}

impl PackageRegistry for RegistryIndex {
    fn resolve_version(&self, package: &str, requirement: &str) -> Result<RegistryPackageVersion> {
        self.resolve_version_matching_all(package, &[requirement.to_string()])
    }
}

impl RegistryIndex {
    pub(crate) fn resolve_version_matching_all(
        &self,
        package: &str,
        requirements: &[String],
    ) -> Result<RegistryPackageVersion> {
        let versions = self
            .packages
            .get(package)
            .ok_or_else(|| anyhow!("registry has no package {package}"))?;
        Ok(versions
            .iter()
            .filter(|candidate| {
                requirements.iter().all(|requirement| {
                    nenjo_packages::version_satisfies(&candidate.version, requirement)
                })
            })
            .max_by(|left, right| compare_versions(&left.version, &right.version))
            .map(|candidate| RegistryPackageVersion {
                name: package.to_string(),
                version: candidate.version.clone(),
                source: candidate.source.clone(),
                dependencies: candidate.dependencies.clone(),
                checksum: candidate.checksum.clone(),
            })
            .ok_or_else(|| {
                anyhow!(
                    "registry has no {package} version matching {}",
                    requirements.join(" and ")
                )
            })?)
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
        Ok(versions
            .iter()
            .filter(|candidate| nenjo_packages::version_satisfies(&candidate.version, requirement))
            .max_by(|left, right| compare_versions(&left.version, &right.version))
            .cloned()
            .ok_or_else(|| anyhow!("registry has no {package} version matching {requirement}"))?)
    }
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_semver = Version::parse(left.trim().trim_start_matches('v'));
    let right_semver = Version::parse(right.trim().trim_start_matches('v'));
    match (left_semver, right_semver) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
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
            if crate::install::is_registry_manifest_path(manifest_path) {
                bail!(
                    "registry source for {}@{} must point at a package manifest, not registry index {}",
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
    let source_scope = package_source_scope(&record.source);
    let source_name = scoped_package_name(source_scope.as_deref(), &package.name);
    if source_name != record.name {
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
    let source_dependencies = scoped_dependencies(source_scope.as_deref(), package.dependencies());
    if source_dependencies != record.dependencies {
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

#[derive(Debug, Clone)]
struct RegistryContentSource {
    root: PathBuf,
    manifest_path: String,
}

fn load_registry_content(
    reference: &str,
    base_dir: &Path,
) -> Result<(String, Option<RegistryContentSource>)> {
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
    let root = path
        .parent()
        .ok_or_else(|| anyhow!("registry path has no parent directory"))?
        .to_path_buf();
    let manifest_path = path
        .file_name()
        .and_then(|file| file.to_str())
        .ok_or_else(|| anyhow!("registry path has no filename"))?
        .to_string();
    Ok((
        content,
        Some(RegistryContentSource {
            root,
            manifest_path,
        }),
    ))
}
