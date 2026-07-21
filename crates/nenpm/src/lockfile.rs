use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use anyhow::Context;
use nenjo_packages::{
    GitHubRepositoryRef, ModuleImport, PackageKind, PackageResourceIdentity,
    PackageResourceInstanceKey, PackageResourceLogicalKey, PackageResourceLogicalRef,
    PackageResourcePath, PackageResourceSlug, ResolvedModule,
};
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
    /// Canonical GitHub repository that publishes this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<GitHubRepositoryRef>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockedModule {
    /// Package-relative module path.
    pub path: String,
    /// Authored resource selector within the module.
    pub resource: String,
    /// Repository-relative source path.
    pub source_path: String,
    /// Canonical authored resource path used for graph identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_path: Option<PackageResourcePath>,
    /// Authored stable package-local resource slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_slug: Option<PackageResourceSlug>,
    /// Stable package resource identity, independent of package version.
    #[serde(
        default,
        alias = "logical_key",
        skip_serializing_if = "Option::is_none"
    )]
    pub logical_ref: Option<PackageResourceLogicalRef>,
    /// Exact package resource identity for this locked version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_key: Option<PackageResourceInstanceKey>,
    /// Module manifest schema.
    pub schema: String,
    /// Inferred module kind.
    pub kind: PackageKind,
    /// Module content hash.
    pub hash: String,
    /// Structured runtime imports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<ModuleImport>,
    /// Additional package files required at runtime by this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<LockedPackageFile>,
}

/// Deserialization-only compatibility shape for lockfiles written before
/// `resource` became the single module selector.
#[derive(Deserialize)]
struct LockedModuleWire {
    path: String,
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    name: Option<String>,
    source_path: String,
    #[serde(default)]
    resource_path: Option<PackageResourcePath>,
    #[serde(default)]
    resource_slug: Option<PackageResourceSlug>,
    #[serde(default, alias = "logical_key")]
    logical_ref: Option<PackageResourceLogicalRef>,
    #[serde(default)]
    instance_key: Option<PackageResourceInstanceKey>,
    schema: String,
    kind: PackageKind,
    hash: String,
    #[serde(default)]
    imports: Vec<ModuleImport>,
    #[serde(default)]
    files: Vec<LockedPackageFile>,
}

impl<'de> Deserialize<'de> for LockedModule {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = LockedModuleWire::deserialize(deserializer)?;
        let resource = wire
            .resource
            .or(wire.name)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| serde::de::Error::missing_field("resource"))?;
        Ok(Self {
            path: wire.path,
            resource,
            source_path: wire.source_path,
            resource_path: wire.resource_path,
            resource_slug: wire.resource_slug,
            logical_ref: wire.logical_ref,
            instance_key: wire.instance_key,
            schema: wire.schema,
            kind: wire.kind,
            hash: wire.hash,
            imports: wire.imports,
            files: wire.files,
        })
    }
}

/// Locked package sidecar file required by a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackageFile {
    /// Package-relative file path.
    pub path: String,
    /// File content hash.
    pub hash: String,
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
        let repository = sources
            .get(package_name)
            .and_then(|source| crate::package_source_github_repository(&source.source));
        let mut modules: Vec<LockedModule> = package
            .modules
            .iter()
            .filter(|(key, module)| *key == &module.key())
            .map(|(module_key, module)| {
                let identity_key = if package.modules.contains_key(&module.path) {
                    module.path.as_str()
                } else {
                    module_key.as_str()
                };
                let resource_path = PackageResourcePath::for_module(
                    identity_key,
                    &module.path,
                    &module.source_path,
                    module.name(),
                )?;
                lock_module(package, module, resource_path, repository.as_ref())
            })
            .collect::<Result<Vec<_>>>()?;
        modules.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.resource.cmp(&right.resource))
        });
        packages.push(LockedPackage {
            name: package.name.clone(),
            version: package.version.clone(),
            repository,
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

fn lock_module(
    package: &nenjo_packages::ResolvedPackage,
    module: &ResolvedModule,
    resource_path: PackageResourcePath,
    repository: Option<&GitHubRepositoryRef>,
) -> Result<LockedModule> {
    let resource_slug = module.manifest.resource_slug()?;
    let package_slug = package
        .name
        .trim_start_matches('@')
        .rsplit('/')
        .next()
        .unwrap_or(package.name.as_str());
    let canonical_identity = match (repository, resource_slug.as_ref()) {
        (Some(repository), Some(resource_slug)) => {
            Some(PackageResourceIdentity::from_resource_path(
                repository,
                package_slug,
                &package.version,
                module.kind,
                resource_slug,
                resource_path.clone(),
            )?)
        }
        _ => None,
    };
    let legacy_identity_name = resource_path.identity_name();
    let logical_ref = match canonical_identity.as_ref() {
        Some(identity) => identity.logical_ref().clone(),
        None => PackageResourceLogicalKey::legacy(
            &package.name,
            module.kind,
            &module.path,
            legacy_identity_name.as_str(),
        )?,
    };
    let instance_key = match canonical_identity.as_ref() {
        Some(identity) => identity.instance_key().clone(),
        None => PackageResourceInstanceKey::legacy(
            &package.name,
            &package.version,
            module.kind,
            &module.path,
            legacy_identity_name.as_str(),
        )?,
    };
    Ok(LockedModule {
        path: module.path.clone(),
        resource: module.name().to_string(),
        source_path: module.source_path.clone(),
        resource_path: Some(resource_path),
        resource_slug,
        logical_ref: Some(logical_ref),
        instance_key: Some(instance_key),
        schema: module.schema().to_string(),
        kind: module.kind,
        hash: module.hash.clone(),
        imports: module.imports.clone(),
        files: module
            .files
            .iter()
            .map(|file| LockedPackageFile {
                path: file.path.clone(),
                hash: file.hash.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nenjo_packages::{
        GitHubRepositoryRef, ModulePackageManifest, PackageKind, PackageResourcePath,
        ResolvedModule, ResolvedPackage, ResourceManifest,
    };

    use super::{lock_module, package_install_path, package_install_path_in_packages_dir};

    #[test]
    fn lockfile_reads_legacy_logical_key_and_writes_logical_ref() {
        let yaml = r#"
schema: nenjo.lock.v1
packages:
  - name: nenji
    version: 1.0.0
    manifest_path: nenji/package.yaml
    hash: package-hash
    dependencies: {}
    modules:
      - path: abilities/manage.yaml
        source_path: nenji/abilities/manage.yaml
        logical_key: "pkg:nenji:ability:abilities/manage.yaml#manage"
        schema: nenjo.ability.v1
        kind: ability
        name: manage
        hash: module-hash
"#;

        let lock: super::NenpmLock = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(lock.packages[0].modules[0].resource, "manage");
        let logical_ref = lock.packages[0].modules[0].logical_ref.as_ref().unwrap();
        assert_eq!(
            logical_ref.as_str(),
            "pkg:nenji:ability:abilities/manage.yaml#manage"
        );

        let serialized = serde_yaml::to_string(&lock).unwrap();
        assert!(serialized.contains("resource: manage"));
        assert!(!serialized.lines().any(|line| line.trim() == "name: manage"));
        assert!(serialized.contains("logical_ref:"));
        assert!(!serialized.contains("logical_key:"));
    }

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

    #[test]
    fn authored_slug_and_github_repository_produce_canonical_locked_logical_ref() {
        let package = ResolvedPackage {
            name: "@nenjo-ai/nenji".to_string(),
            path: "nenjo/nenji/package.yaml".to_string(),
            version: "1.2.0".to_string(),
            hash: "package-hash".to_string(),
            manifest: ModulePackageManifest {
                schema: "nenjo.package.v1".to_string(),
                name: "nenji".to_string(),
                version: "1.2.0".to_string(),
                description: None,
                dependencies: BTreeMap::new(),
                arguments: Vec::new(),
                modules: Vec::new(),
                metadata: serde_json::Value::Null,
            },
            modules: BTreeMap::new(),
        };
        let module = ResolvedModule {
            package_name: package.name.clone(),
            package_version: package.version.clone(),
            path: "capabilities/build/manage_tasks.yaml".to_string(),
            source_path: "nenjo/nenji/capabilities/build/manage_tasks.yaml".to_string(),
            hash: "module-hash".to_string(),
            kind: PackageKind::Ability,
            manifest: ResourceManifest {
                schema: "nenjo.ability.v1".to_string(),
                slug: Some("manage-tasks".to_string()),
                root_uri: None,
                selector: None,
                imports: BTreeMap::new(),
                manifest: serde_json::json!({ "name": "Manage Tasks" }),
            },
            imports: Vec::new(),
            files: Vec::new(),
        };
        let repository = GitHubRepositoryRef::parse("@nenjo-ai/packages").unwrap();
        let resource_path = PackageResourcePath::parse(&module.source_path).unwrap();

        let locked = lock_module(&package, &module, resource_path, Some(&repository)).unwrap();

        assert_eq!(locked.resource_slug.unwrap().as_str(), "manage-tasks");
        assert_eq!(
            locked.logical_ref.unwrap().as_str(),
            "pkg:@nenjo-ai/packages:nenji:ability:manage-tasks"
        );
        assert_eq!(
            locked.instance_key.unwrap().as_str(),
            "pkg:@nenjo-ai/packages:nenji@1.2.0:ability:manage-tasks"
        );
    }
}
