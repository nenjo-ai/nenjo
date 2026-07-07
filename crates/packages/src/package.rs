use std::collections::{BTreeMap, BTreeSet};

use crate::Result;
use anyhow::Context;
use nenjo::arguments::PackageArgumentDefinition;
use serde::{Deserialize, Serialize};

use crate::schema::parse_package_file_schema;
use crate::{
    ManifestSchemaVersion, PackageFileSchema, PackageKind, normalize_module_reference,
    validate_package_name, validate_package_slug, validate_source_path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageCatalog {
    /// Catalog schema string, for example `nenjo.packages.v1`.
    pub schema: String,
    /// Optional human-readable catalog name.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional catalog description.
    #[serde(default)]
    pub description: Option<String>,
    /// Package entries advertised by the catalog.
    #[serde(default)]
    pub packages: Vec<PackageEntry>,
}

impl PackageCatalog {
    /// Return the validated catalog schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        Ok(PackageFileSchema::parse_catalog(&self.schema)?.version())
    }

    /// Validate the catalog schema and all package entry paths.
    pub fn validate(&self) -> Result<()> {
        self.schema_version()?;
        for package in &self.packages {
            validate_source_path(&package.path).with_context(|| {
                format!("package catalog entry '{}' has invalid path", package.slug)
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Single package entry inside a [`PackageCatalog`].
pub struct PackageEntry {
    /// Resource kind installed by this package.
    #[serde(rename = "type", alias = "kind")]
    pub kind: PackageKind,
    /// Stable package slug within the catalog.
    pub slug: String,
    /// Optional display name for catalog UIs.
    #[serde(default)]
    pub name: Option<String>,
    /// Repository-relative path to the package descriptor.
    pub path: String,
    /// Adapter-specific or UI-specific package metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Package descriptor for one installable resource and its dependencies.
pub struct PackageDescriptor {
    /// Descriptor schema string, for example `nenjo.package.v1`.
    pub schema: String,
    /// Resource kind installed by this descriptor.
    #[serde(rename = "type", alias = "kind")]
    pub kind: PackageKind,
    /// Stable package slug within the catalog.
    pub slug: String,
    /// Human-readable package name.
    pub name: String,
    /// Optional semantic version published by the package author.
    #[serde(default)]
    pub version: Option<String>,
    /// Descriptor-relative filename for the resource manifest.
    pub entry: String,
    /// Repository-relative package descriptors this package depends on.
    #[serde(default)]
    pub depends_on: Vec<ResourceDependency>,
    /// Adapter-specific or UI-specific package metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Dependency on another package resource.
pub struct ResourceDependency {
    /// Repository-relative path to the dependency descriptor.
    pub path: String,
    /// Optional version requirement. Exact versions and `^major.minor.patch` are supported.
    #[serde(default)]
    pub version: Option<String>,
}

impl PackageDescriptor {
    /// Return the validated descriptor schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        Ok(PackageFileSchema::parse_descriptor(&self.schema)?.version())
    }

    /// Validate the descriptor schema and entry path.
    pub fn validate(&self, path: &str) -> Result<()> {
        self.schema_version()
            .with_context(|| format!("{path} has unsupported package schema"))?;
        validate_source_path(&self.entry)
            .with_context(|| format!("{path} has invalid package entry"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Registry manifest listing packages available from one package source.
pub struct PackageRegistryManifest {
    /// Registry schema string, for example `nenjo.registry.v1`.
    pub schema: String,
    /// Optional human-readable registry name.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional registry description.
    #[serde(default)]
    pub description: Option<String>,
    /// External package registries this registry depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registries: Vec<PackageRegistryReference>,
    /// Package names mapped to registry-relative package manifest paths.
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
}

impl PackageRegistryManifest {
    /// Return the validated registry schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        parse_package_file_schema(&self.schema, "registry")
    }

    /// Validate the registry schema, package names, and manifest paths.
    pub fn validate(&self) -> Result<()> {
        self.schema_version()?;
        for (index, registry) in self.registries.iter().enumerate() {
            registry
                .validate()
                .with_context(|| format!("external registry reference {} is invalid", index + 1))?;
        }
        for (name, path) in &self.packages {
            validate_package_slug(name)
                .with_context(|| format!("registry package '{name}' is invalid"))?;
            validate_source_path(path)
                .with_context(|| format!("registry package '{name}' has invalid path"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
/// Registry dependency reference advertised by a publisher registry.
pub enum PackageRegistryReference {
    /// Legacy registry index reference, such as `registry.yml` or an HTTPS URL.
    Index(String),
    /// Repository-style registry source, usually a git source pointing at `packages.yaml`.
    Source(PackageRegistrySource),
}

impl PackageRegistryReference {
    /// Validate that this registry reference is well-formed.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Index(reference) => {
                if reference.trim().is_empty() {
                    bail!("registry reference cannot be empty");
                }
            }
            Self::Source(source) => source.validate()?,
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Fetchable registry source descriptor advertised by a publisher registry.
pub enum PackageRegistrySource {
    /// Package registry content comes from a git repository.
    Git {
        /// Git remote URL.
        url: String,
        /// Branch, tag, or commit reference.
        reference: String,
        /// Repository-relative registry manifest path.
        manifest_path: String,
    },
    /// Package registry content comes from an immutable package artifact.
    Artifact {
        /// Artifact URL.
        url: String,
        /// Expected artifact checksum.
        checksum: String,
        /// Artifact-relative registry manifest path.
        manifest_path: String,
    },
    /// Package registry content comes from a local repository checkout.
    Local {
        /// Local repository root.
        root: String,
        /// Repository-relative registry manifest path.
        manifest_path: String,
        /// Package scope used when the local source points at a registry manifest.
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

impl PackageRegistrySource {
    /// Return the registry manifest path for this source.
    pub fn manifest_path(&self) -> &str {
        match self {
            Self::Git { manifest_path, .. }
            | Self::Artifact { manifest_path, .. }
            | Self::Local { manifest_path, .. } => manifest_path,
        }
    }

    /// Validate that this source descriptor is well-formed.
    pub fn validate(&self) -> Result<()> {
        validate_source_path(self.manifest_path())
            .context("registry source manifest path is invalid")?;
        match self {
            Self::Git { url, reference, .. } => {
                if url.trim().is_empty() {
                    bail!("git registry source url cannot be empty");
                }
                if reference.trim().is_empty() {
                    bail!("git registry source reference cannot be empty");
                }
            }
            Self::Artifact { url, checksum, .. } => {
                if url.trim().is_empty() {
                    bail!("artifact registry source url cannot be empty");
                }
                if checksum.trim().is_empty() {
                    bail!("artifact registry source checksum cannot be empty");
                }
            }
            Self::Local { root, scope, .. } => {
                if root.trim().is_empty() {
                    bail!("local registry source root cannot be empty");
                }
                if let Some(scope) = scope {
                    validate_registry_scope(scope)?;
                }
            }
        }
        Ok(())
    }
}

fn validate_registry_scope(scope: &str) -> Result<()> {
    if !scope.starts_with('@') || scope.contains('/') {
        bail!("local registry scope must look like @scope");
    }
    validate_package_name(&format!("{scope}/package"))
        .context("local registry scope is invalid")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Multi-module package manifest used by the nenpm package model.
pub struct ModulePackageManifest {
    /// Package schema string, for example `nenjo.package.v1`.
    pub schema: String,
    /// Package name. Repo-backed registry manifests author unscoped names such
    /// as `nenji`; registry consumers may see a scoped name such as
    /// `@nenjo-ai/nenji`.
    pub name: String,
    /// Semantic version published by the package author.
    pub version: String,
    /// Optional human-readable package description.
    #[serde(default)]
    pub description: Option<String>,
    /// Package-level dependency requirements keyed by package name.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Package-local runtime argument contracts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<PackageArgumentDefinition>,
    /// Manifest modules included by this package.
    #[serde(default)]
    pub modules: Vec<PackageModule>,
    /// Adapter-specific or UI-specific package metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ModulePackageManifest {
    /// Return the validated package schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        Ok(PackageFileSchema::parse_descriptor(&self.schema)?.version())
    }

    /// Validate the package manifest and package-relative module paths.
    pub fn validate(&self, path: &str) -> Result<()> {
        self.schema_version()
            .with_context(|| format!("{path} has unsupported package schema"))?;
        validate_package_name(&self.name)
            .with_context(|| format!("{path} has invalid package name"))?;
        if self.version.trim().is_empty() {
            bail!("{path} has empty package version");
        }
        for name in self.dependencies.keys() {
            validate_package_name(name)
                .with_context(|| format!("{path} has invalid dependency '{name}'"))?;
        }
        validate_package_arguments(path, &self.arguments)?;
        let mut module_paths = BTreeSet::new();
        for module in &self.modules {
            validate_source_path(&module.path)
                .with_context(|| format!("{path} has invalid module path '{}'", module.path))?;
            if !module_paths.insert(normalize_module_reference(&module.path)?) {
                bail!("{path} declares duplicate module path '{}'", module.path);
            }
        }
        Ok(())
    }
}

fn validate_package_arguments(path: &str, arguments: &[PackageArgumentDefinition]) -> Result<()> {
    let mut names = BTreeSet::new();
    let mut selectors = BTreeSet::new();
    for argument in arguments {
        if !names.insert(argument.name.clone()) {
            bail!(
                "{path} declares duplicate argument name '{}'",
                argument.name
            );
        }
        if !selectors.insert(argument.selector.clone()) {
            bail!(
                "{path} declares duplicate argument selector '{}'",
                argument.selector
            );
        }
        if let Some(default) = &argument.default {
            argument
                .value_type
                .coerce_render_value(default)
                .with_context(|| {
                    format!(
                        "{path} argument '{}' has invalid default value",
                        argument.name
                    )
                })?;
        }
        if let Some(sample) = &argument.sample {
            argument
                .value_type
                .coerce_render_value(sample)
                .with_context(|| {
                    format!(
                        "{path} argument '{}' has invalid sample value",
                        argument.name
                    )
                })?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
/// A manifest file included by a package.
pub struct PackageModule {
    /// Package-relative path to a resource manifest.
    pub path: String,
    /// Module-specific metadata reserved for future use.
    pub metadata: serde_json::Value,
}

impl<'de> Deserialize<'de> for PackageModule {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Path(String),
            Object {
                path: String,
                #[serde(default)]
                metadata: serde_json::Value,
            },
        }

        match Input::deserialize(deserializer)? {
            Input::Path(path) => Ok(Self {
                path,
                metadata: serde_json::Value::Null,
            }),
            Input::Object { path, metadata } => Ok(Self { path, metadata }),
        }
    }
}
