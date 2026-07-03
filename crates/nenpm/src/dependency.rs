use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{NenpmError, Result};
use anyhow::Context;
use nenjo_packages::{
    PackageRegistryReference as SchemaRegistryReference,
    PackageRegistrySource as SchemaRegistrySource,
};
use serde::{Deserialize, Serialize};

use crate::source::{PackageSource, validate_package_source};

/// User-authored nenpm dependency manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyManifest {
    /// Manifest schema, currently `nenjo.dependencies.v1`.
    pub schema: String,
    /// Package dependencies.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Package source overrides keyed by package name.
    #[serde(default)]
    pub overrides: BTreeMap<String, DependencyOverride>,
    /// Ordered package registries. Earlier registries win when more than one contains a package.
    #[serde(default)]
    pub registries: Vec<RegistryReference>,
}

impl DependencyManifest {
    /// Parse a dependency manifest from YAML.
    pub fn parse_yaml(content: &str) -> Result<Self> {
        let manifest: Self =
            serde_yaml::from_str(content).context("failed to parse nenpm dependency manifest")?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Load `nenpm.yml` or `nenpm.yaml` from a directory.
    pub fn load_from_dir(root: impl AsRef<Path>) -> Result<LoadedDependencyManifest> {
        let root = root.as_ref();
        let yml = root.join("nenpm.yml");
        let yaml = root.join("nenpm.yaml");
        let yml_exists = yml.exists();
        let yaml_exists = yaml.exists();
        match (yml_exists, yaml_exists) {
            (true, true) => Err(NenpmError::dependency_manifest(
                "found both nenpm.yml and nenpm.yaml; keep only one dependency file",
            )),
            (false, false) => Err(NenpmError::dependency_manifest(
                "missing nenpm.yml or nenpm.yaml",
            )),
            (true, false) => Self::load_file(yml),
            (false, true) => Self::load_file(yaml),
        }
    }

    /// Load a dependency manifest from a specific YAML file.
    pub fn load_file(path: impl AsRef<Path>) -> Result<LoadedDependencyManifest> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let manifest = Self::parse_yaml(&content)
            .with_context(|| format!("failed to load {}", path.display()))?;
        Ok(LoadedDependencyManifest {
            path: path.to_path_buf(),
            manifest,
        })
    }

    /// Validate schema, package names, and override values.
    pub fn validate(&self) -> Result<()> {
        if self.schema != "nenjo.dependencies.v1" {
            return Err(NenpmError::dependency_manifest(format!(
                "unsupported dependency manifest schema '{}'",
                self.schema
            )));
        }
        for name in self.dependencies.keys().chain(self.overrides.keys()) {
            nenjo_packages::validate_package_name(name)
                .with_context(|| format!("invalid dependency package name '{name}'"))?;
        }
        for (name, override_source) in &self.overrides {
            override_source
                .to_package_source()
                .with_context(|| format!("invalid override for {name}"))?;
        }
        for reference in &self.registries {
            reference.validate().context("invalid registry")?;
        }
        Ok(())
    }
}

/// Dependency manifest loaded from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDependencyManifest {
    /// Path that was loaded.
    pub path: PathBuf,
    /// Parsed manifest.
    pub manifest: DependencyManifest,
}

/// Registry reference in a dependency manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RegistryReference {
    /// Legacy registry index reference, such as `registry.yml` or an HTTPS URL.
    Index(String),
    /// Repository-style registry source, usually a git source pointing at `packages.yaml`.
    Source(PackageSource),
}

impl RegistryReference {
    /// Validate that this reference is usable.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Index(reference) => {
                if reference.trim().is_empty() {
                    bail!("registry reference cannot be empty");
                }
            }
            Self::Source(source) => validate_package_source(source)?,
        }
        Ok(())
    }
}

impl From<&SchemaRegistryReference> for RegistryReference {
    fn from(reference: &SchemaRegistryReference) -> Self {
        match reference {
            SchemaRegistryReference::Index(reference) => Self::Index(reference.clone()),
            SchemaRegistryReference::Source(source) => Self::Source(PackageSource::from(source)),
        }
    }
}

impl From<SchemaRegistryReference> for RegistryReference {
    fn from(reference: SchemaRegistryReference) -> Self {
        Self::from(&reference)
    }
}

impl From<&SchemaRegistrySource> for PackageSource {
    fn from(source: &SchemaRegistrySource) -> Self {
        match source {
            SchemaRegistrySource::Git {
                url,
                reference,
                manifest_path,
            } => Self::Git {
                url: url.clone(),
                reference: reference.clone(),
                manifest_path: manifest_path.clone(),
            },
            SchemaRegistrySource::Artifact {
                url,
                checksum,
                manifest_path,
            } => Self::Artifact {
                url: url.clone(),
                checksum: checksum.clone(),
                manifest_path: manifest_path.clone(),
            },
            SchemaRegistrySource::Local {
                root,
                manifest_path,
                scope,
            } => Self::Local {
                root: PathBuf::from(root),
                manifest_path: manifest_path.clone(),
                scope: scope.clone(),
            },
        }
    }
}

/// Source override in a dependency manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencyOverride {
    /// Shorthand source, such as `file:../packages#nenjo/core.package.yaml`.
    Shorthand(String),
    /// Structured source descriptor.
    Source(PackageSource),
}

impl DependencyOverride {
    /// Convert this override to a package source.
    pub fn to_package_source(&self) -> Result<PackageSource> {
        let source = match self {
            Self::Source(source) => Ok(source.clone()),
            Self::Shorthand(value) => parse_override_shorthand(value),
        }?;
        validate_package_source(&source)?;
        Ok(source)
    }
}

fn parse_override_shorthand(value: &str) -> Result<PackageSource> {
    let raw = value.trim();
    let Some(rest) = raw.strip_prefix("file:") else {
        bail!("unsupported override shorthand '{value}'");
    };
    let (root, manifest_path) = match rest.split_once('#') {
        Some((root, manifest_path)) => (root, manifest_path),
        None => (rest, "packages.yaml"),
    };
    if root.trim().is_empty() {
        bail!("file override root cannot be empty");
    }
    let manifest_path = nenjo_packages::validate_source_path(manifest_path)
        .context("file override manifest path is invalid")?;
    Ok(PackageSource::Local {
        root: PathBuf::from(root),
        manifest_path,
        scope: None,
    })
}
