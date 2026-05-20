//! Package catalog and manifest primitives for Nenjo package repositories.
//!
//! `nenjo-packages` handles the repository-facing package format: catalog files,
//! package descriptors, resource manifests, dependency graphs, GitHub fetching,
//! lockfile records, and small validation helpers. It intentionally keeps the
//! format-level logic independent from platform persistence so workers and
//! platform services can share the same package parsing rules.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Map;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// External package format handled by the package importer.
pub enum PackageAdapter {
    /// Native Nenjo package catalog and descriptor files.
    NenjoPackages,
    /// Claude marketplace style packages.
    ClaudeMarketplace,
    /// Codex plugin directories.
    CodexPlugin,
}

impl PackageAdapter {
    /// Parse a stable adapter identifier.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "nenjo_packages" => Ok(Self::NenjoPackages),
            "claude_marketplace" => Ok(Self::ClaudeMarketplace),
            "codex_plugin" => Ok(Self::CodexPlugin),
            other => bail!("unsupported package adapter '{other}'"),
        }
    }

    /// Return the stable adapter identifier used in serialized metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NenjoPackages => "nenjo_packages",
            Self::ClaudeMarketplace => "claude_marketplace",
            Self::CodexPlugin => "codex_plugin",
        }
    }
}

impl FromStr for PackageAdapter {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Kind of resource a package installs.
pub enum PackageKind {
    /// Agent manifest.
    Agent,
    /// Ability/tool manifest.
    Ability,
    /// Domain manifest.
    Domain,
    /// Context block manifest.
    ContextBlock,
    /// Knowledge source or knowledge reference manifest.
    Knowledge,
    /// Codex-style skill manifest.
    Skill,
    /// Plugin manifest.
    Plugin,
    /// MCP server manifest.
    McpServer,
    /// Routine manifest.
    Routine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Supported manifest schema versions.
pub enum ManifestSchemaVersion {
    /// Initial package and resource schema version.
    V1,
}

impl ManifestSchemaVersion {
    /// Parse a schema version suffix such as `v1`.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "v1" => Ok(Self::V1),
            other => bail!("unsupported manifest schema version '{other}'"),
        }
    }

    /// Return the schema version suffix used in manifest schema strings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
        }
    }
}

impl FromStr for ManifestSchemaVersion {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Parsed `nenjo.<resource>.<version>` schema for a resource manifest.
pub struct ResourceSchema {
    /// Resource kind declared by the schema.
    pub kind: PackageKind,
    /// Schema version declared by the schema.
    pub version: ManifestSchemaVersion,
}

impl ResourceSchema {
    /// Parse a resource schema such as `nenjo.agent.v1`.
    pub fn parse(schema: &str) -> Result<Self> {
        let Some(rest) = schema.strip_prefix("nenjo.") else {
            bail!("resource schema '{schema}' must start with 'nenjo.'");
        };
        let Some((kind, version)) = rest.rsplit_once('.') else {
            bail!("resource schema '{schema}' must include a version suffix");
        };
        let version = ManifestSchemaVersion::parse(version)
            .with_context(|| format!("resource schema '{schema}' has unsupported version"))?;
        let kind = PackageKind::parse_kind(kind)?;
        Ok(Self { kind, version })
    }
}

impl FromStr for ResourceSchema {
    type Err = anyhow::Error;

    fn from_str(schema: &str) -> Result<Self, Self::Err> {
        Self::parse(schema)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Parsed schema for top-level package files.
pub enum PackageFileSchema {
    /// Catalog file schema such as `nenjo.packages.v1`.
    Catalog(ManifestSchemaVersion),
    /// Package descriptor schema such as `nenjo.package.v1`.
    Descriptor(ManifestSchemaVersion),
}

impl PackageFileSchema {
    /// Parse and validate a catalog schema string.
    pub fn parse_catalog(schema: &str) -> Result<Self> {
        parse_package_file_schema(schema, "packages").map(Self::Catalog)
    }

    /// Parse and validate a package descriptor schema string.
    pub fn parse_descriptor(schema: &str) -> Result<Self> {
        parse_package_file_schema(schema, "package").map(Self::Descriptor)
    }

    /// Return the package file schema version.
    pub fn version(self) -> ManifestSchemaVersion {
        match self {
            Self::Catalog(version) | Self::Descriptor(version) => version,
        }
    }
}

fn parse_package_file_schema(schema: &str, expected_kind: &str) -> Result<ManifestSchemaVersion> {
    let Some(rest) = schema.strip_prefix("nenjo.") else {
        bail!("package schema '{schema}' must start with 'nenjo.'");
    };
    let Some((kind, version)) = rest.rsplit_once('.') else {
        bail!("package schema '{schema}' must include a version suffix");
    };
    if kind != expected_kind {
        bail!("expected schema 'nenjo.{expected_kind}.*', got '{schema}'");
    }
    ManifestSchemaVersion::parse(version)
        .with_context(|| format!("package schema '{schema}' has unsupported version"))
}

impl PackageKind {
    /// Parse the resource kind from a full resource schema string.
    pub fn parse_schema(schema: &str) -> Result<Self> {
        Ok(ResourceSchema::parse(schema)?.kind)
    }

    fn parse_kind(kind: &str) -> Result<Self> {
        match kind {
            "agent" => Ok(Self::Agent),
            "ability" => Ok(Self::Ability),
            "domain" => Ok(Self::Domain),
            "context_block" => Ok(Self::ContextBlock),
            "knowledge" | "knowledge_ref" => Ok(Self::Knowledge),
            "skill" => Ok(Self::Skill),
            "plugin" => Ok(Self::Plugin),
            "mcp_server" => Ok(Self::McpServer),
            "routine" => Ok(Self::Routine),
            other => bail!("unsupported package resource schema '{other}'"),
        }
    }

    /// Return the stable package kind identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Ability => "ability",
            Self::Domain => "domain",
            Self::ContextBlock => "context_block",
            Self::Knowledge => "knowledge",
            Self::Skill => "skill",
            Self::Plugin => "plugin",
            Self::McpServer => "mcp_server",
            Self::Routine => "routine",
        }
    }
}

impl FromStr for PackageKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_kind(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Top-level package catalog listing installable packages in a repository.
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
/// Repository manifest listing packages available from one source repository.
pub struct PackageRepositoryManifest {
    /// Repository schema string, for example `nenjo.repository.v1`.
    pub schema: String,
    /// Optional human-readable repository name.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional repository description.
    #[serde(default)]
    pub description: Option<String>,
    /// Package names mapped to repository-relative package manifest paths.
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
}

impl PackageRepositoryManifest {
    /// Return the validated repository schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        parse_package_file_schema(&self.schema, "repository")
    }

    /// Validate the repository schema, package names, and manifest paths.
    pub fn validate(&self) -> Result<()> {
        self.schema_version()?;
        for (name, path) in &self.packages {
            validate_package_name(name)
                .with_context(|| format!("repository package '{name}' is invalid"))?;
            validate_source_path(path)
                .with_context(|| format!("repository package '{name}' has invalid path"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Multi-module package manifest used by the nenpm package model.
pub struct ModulePackageManifest {
    /// Package schema string, for example `nenjo.package.v1`.
    pub schema: String,
    /// Registry package name, for example `@nenjo/nenji`.
    pub name: String,
    /// Semantic version published by the package author.
    pub version: String,
    /// Optional human-readable package description.
    #[serde(default)]
    pub description: Option<String>,
    /// Package-level dependency requirements keyed by package name.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Manifest modules included by this package.
    #[serde(default)]
    pub modules: Vec<PackageModule>,
    /// Optional public aliases for selected modules.
    #[serde(default)]
    pub exports: BTreeMap<String, PackageExport>,
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
        let mut module_paths = BTreeSet::new();
        for module in &self.modules {
            validate_source_path(&module.path)
                .with_context(|| format!("{path} has invalid module path '{}'", module.path))?;
            if !module_paths.insert(module.path.clone()) {
                bail!("{path} declares duplicate module path '{}'", module.path);
            }
        }
        for (name, export) in &self.exports {
            validate_export_name(name)
                .with_context(|| format!("{path} has invalid export '{name}'"))?;
            ModuleTarget::parse(&export.path)
                .with_context(|| format!("{path} has invalid export path '{}'", export.path))?;
        }
        Ok(())
    }
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

#[derive(Debug, Clone, Serialize)]
/// Optional stable public alias for a package module.
pub struct PackageExport {
    /// Package-relative path to the exported module manifest.
    pub path: String,
    /// Export metadata reserved for future use.
    pub metadata: serde_json::Value,
}

impl<'de> Deserialize<'de> for PackageExport {
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

fn parse_module_file(content: &str, source_path: &str) -> Result<Vec<ResourceManifest>> {
    let value = parse_json_or_yaml(content)
        .with_context(|| format!("failed to parse module file {source_path}"))?;
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("{source_path} is missing schema"))?;
    if schema == "nenjo.modules.v1" {
        let bundle: ModuleBundle = serde_json::from_value(value)
            .with_context(|| format!("failed to parse module bundle {source_path}"))?;
        bundle.validate(source_path)?;
        Ok(bundle.resources)
    } else {
        let manifest: ResourceManifest = serde_json::from_value(value)
            .with_context(|| format!("failed to parse resource manifest {source_path}"))?;
        Ok(vec![manifest])
    }
}

fn extract_module_imports(manifest: &serde_json::Value) -> Vec<ModuleImport> {
    let Some(imports) = manifest
        .get("imports")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (surface, value) in imports {
        match value {
            serde_json::Value::String(reference) => {
                out.push(ModuleImport {
                    surface: surface.clone(),
                    reference: reference.clone(),
                });
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    if let Some(reference) = item.as_str() {
                        out.push(ModuleImport {
                            surface: surface.clone(),
                            reference: reference.to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Package-relative module target with an optional resource selector.
pub struct ModuleTarget {
    /// Package-relative module file path.
    pub path: String,
    /// Optional resource name selector inside a multi-resource module file.
    pub resource: Option<String>,
}

impl ModuleTarget {
    /// Parse a module target such as `agents/nenji.yaml#nenji`.
    pub fn parse(value: &str) -> Result<Self> {
        let raw = value.trim();
        let (path, resource) = match raw.split_once('#') {
            Some((path, resource)) => {
                if resource.trim().is_empty() || resource.contains('/') {
                    bail!("invalid module resource selector '{resource}'");
                }
                (path, Some(resource.trim().to_string()))
            }
            None => (raw, None),
        };
        Ok(Self {
            path: validate_source_path(path)?,
            resource,
        })
    }

    /// Return the canonical module target key.
    pub fn key(&self) -> String {
        match &self.resource {
            Some(resource) => format!("{}#{resource}", self.path),
            None => self.path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Bundle envelope for a module file that contains multiple resource manifests.
pub struct ModuleBundle {
    /// Bundle schema string, for example `nenjo.modules.v1`.
    pub schema: String,
    /// Resource manifests included in this module file.
    #[serde(default)]
    pub resources: Vec<ResourceManifest>,
}

impl ModuleBundle {
    /// Return the validated bundle schema version.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        parse_package_file_schema(&self.schema, "modules")
    }

    /// Validate bundle schema and all included resource manifests.
    pub fn validate(&self, path: &str) -> Result<()> {
        self.schema_version()
            .with_context(|| format!("{path} has unsupported module bundle schema"))?;
        let mut names = BTreeSet::new();
        for resource in &self.resources {
            resource
                .name()
                .with_context(|| format!("failed to validate bundled resource in {path}"))?;
            let name = resource.name().expect("resource name was just validated");
            if !names.insert(name.to_string()) {
                bail!("{path} declares duplicate bundled resource '{name}'");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Resource manifest envelope stored at a package descriptor's `entry`.
pub struct ResourceManifest {
    /// Resource schema string, for example `nenjo.agent.v1`.
    pub schema: String,
    /// Optional platform or package slug for the resource.
    #[serde(default)]
    pub slug: Option<String>,
    /// Optional root URI used to identify source-managed resources.
    #[serde(default)]
    pub root_uri: Option<String>,
    /// Optional stable selector used when syncing or replacing source-managed resources.
    #[serde(default)]
    pub selector: Option<String>,
    /// Resource-specific manifest body.
    #[serde(default)]
    pub manifest: serde_json::Value,
}

impl ResourceManifest {
    /// Return the parsed resource schema.
    pub fn resource_schema(&self) -> Result<ResourceSchema> {
        ResourceSchema::parse(&self.schema)
    }

    /// Return the resource kind declared by `schema`.
    pub fn kind(&self) -> Result<PackageKind> {
        Ok(self.resource_schema()?.kind)
    }

    /// Return the resource schema version declared by `schema`.
    pub fn schema_version(&self) -> Result<ManifestSchemaVersion> {
        Ok(self.resource_schema()?.version)
    }

    /// Return the resource manifest body as an object.
    pub fn manifest_object(&self) -> Result<&Map<String, serde_json::Value>> {
        self.manifest
            .as_object()
            .ok_or_else(|| anyhow!("resource manifest body must be an object"))
    }

    /// Return the required resource name from the manifest body.
    pub fn name(&self) -> Result<&str> {
        self.manifest_object()?
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("resource manifest body is missing name"))
    }

    /// Return the optional resource version from the manifest body.
    pub fn version(&self) -> Option<&str> {
        self.manifest
            .get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    /// Return the optional resource slug.
    pub fn slug(&self) -> Option<&str> {
        self.slug.as_deref()
    }

    /// Return the optional source root URI.
    pub fn root_uri(&self) -> Option<&str> {
        self.root_uri.as_deref()
    }

    /// Return the source selector, falling back to `root_uri` when absent.
    pub fn selector(&self) -> Option<&str> {
        self.selector.as_deref().or(self.root_uri())
    }

    /// Return structured module imports declared by this resource manifest.
    pub fn imports(&self) -> Vec<ModuleImport> {
        extract_module_imports(&self.manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Structured resource-level import discovered from a manifest body.
pub struct ModuleImport {
    /// Import surface, such as `abilities`, `domains`, `mcp_servers`, or `context`.
    pub surface: String,
    /// Raw reference string supplied by the manifest author.
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Dependency on another package descriptor in the same package repository.
pub struct ResourceDependency {
    /// Repository-relative path to the dependency descriptor.
    pub path: String,
    /// Optional version requirement. Exact versions and `^major.minor.patch` are supported.
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
/// Resolved descriptor, manifest, and content hash for one package resource.
pub struct ResolvedResource {
    /// Repository-relative descriptor path.
    pub path: String,
    /// Repository-relative resource manifest path.
    pub entry_path: String,
    /// SHA-256 hash of the descriptor and resource manifest content.
    pub hash: String,
    /// Resolved resource kind.
    pub kind: PackageKind,
    /// Parsed package descriptor.
    pub descriptor: PackageDescriptor,
    /// Parsed resource manifest envelope.
    pub manifest: ResourceManifest,
}

#[derive(Debug, Clone)]
/// Resolved package module with inferred runtime information.
pub struct ResolvedModule {
    /// Package name that owns this module.
    pub package_name: String,
    /// Package version that owns this module.
    pub package_version: String,
    /// Package-relative module manifest path.
    pub path: String,
    /// Repository-relative module manifest path.
    pub source_path: String,
    /// SHA-256 hash of the module manifest content.
    pub hash: String,
    /// Resource kind inferred from the module manifest schema.
    pub kind: PackageKind,
    /// Parsed module manifest.
    pub manifest: ResourceManifest,
    /// Structured resource imports declared by this module.
    pub imports: Vec<ModuleImport>,
}

impl ResolvedModule {
    /// Return the validated resource name inferred from the module manifest body.
    pub fn name(&self) -> &str {
        self.manifest
            .name()
            .expect("resolved module manifest was validated")
    }

    /// Return the manifest schema string.
    pub fn schema(&self) -> &str {
        &self.manifest.schema
    }

    /// Return the canonical key for this resolved module resource.
    pub fn key(&self) -> String {
        format!("{}#{}", self.path, self.name())
    }
}

#[derive(Debug, Clone)]
/// Resolved package manifest and all included modules.
pub struct ResolvedPackage {
    /// Repository package name.
    pub name: String,
    /// Repository-relative package manifest path.
    pub path: String,
    /// Package version.
    pub version: String,
    /// SHA-256 hash of the package manifest content.
    pub hash: String,
    /// Parsed package manifest.
    pub manifest: ModulePackageManifest,
    /// Resolved modules keyed by package-relative module path.
    pub modules: BTreeMap<String, ResolvedModule>,
}

impl ResolvedPackage {
    /// Return package dependencies as a name-to-version-requirement map.
    pub fn dependencies(&self) -> &BTreeMap<String, String> {
        &self.manifest.dependencies
    }
}

#[derive(Debug, Clone)]
/// Dependency graph for a root package and all resolved package dependencies.
pub struct ResolvedPackageGraph {
    /// Package name requested by the installer.
    pub root_package: String,
    /// Resolved packages keyed by package name.
    pub packages: BTreeMap<String, ResolvedPackage>,
}

impl ResolvedPackageGraph {
    /// Return dependency-first package install order with the root package last.
    pub fn topo_order(&self) -> Result<Vec<String>> {
        fn visit(
            name: &str,
            graph: &BTreeMap<String, ResolvedPackage>,
            temp: &mut BTreeSet<String>,
            perm: &mut BTreeSet<String>,
            out: &mut Vec<String>,
        ) -> Result<()> {
            if perm.contains(name) {
                return Ok(());
            }
            if !temp.insert(name.to_string()) {
                bail!("dependency cycle includes {name}");
            }
            let package = graph
                .get(name)
                .ok_or_else(|| anyhow!("dependency {name} was not resolved"))?;
            for dependency in package.dependencies().keys() {
                visit(dependency, graph, temp, perm, out)?;
            }
            temp.remove(name);
            perm.insert(name.to_string());
            out.push(name.to_string());
            Ok(())
        }

        let mut out = Vec::new();
        visit(
            &self.root_package,
            &self.packages,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut out,
        )?;
        if let Some(pos) = out.iter().position(|name| name == &self.root_package) {
            let root = out.remove(pos);
            out.push(root);
        }
        Ok(out)
    }

    /// Validate package dependency version requirements against resolved versions.
    pub fn validate_versions(&self) -> Result<()> {
        for (name, package) in &self.packages {
            for (dependency, required) in package.dependencies() {
                let resolved = self
                    .packages
                    .get(dependency)
                    .ok_or_else(|| anyhow!("{name} depends on unresolved {dependency}"))?;
                if !version_satisfies(&resolved.version, required) {
                    bail!(
                        "{name} requires {dependency} version {required}, got {}",
                        resolved.version
                    );
                }
            }
        }
        Ok(())
    }
}

impl ResolvedResource {
    /// Return the validated resource name.
    pub fn name(&self) -> &str {
        self.manifest
            .name()
            .expect("resolved resource manifest was validated")
    }

    /// Return the optional resource slug.
    pub fn slug(&self) -> Option<&str> {
        self.manifest.slug()
    }

    /// Return the optional source root URI.
    pub fn root_uri(&self) -> Option<&str> {
        self.manifest.root_uri()
    }

    /// Return the source selector, falling back to the root URI.
    pub fn selector(&self) -> Option<&str> {
        self.manifest.selector()
    }

    /// Return the package descriptor version.
    pub fn version(&self) -> Option<&str> {
        self.descriptor.version.as_deref()
    }

    /// Return the package dependencies declared by the descriptor.
    pub fn dependencies(&self) -> &[ResourceDependency] {
        &self.descriptor.depends_on
    }
}

#[derive(Debug, Clone)]
/// Dependency graph for a root package resource and all resolved dependencies.
pub struct ResolvedResourceGraph {
    /// Repository-relative descriptor path requested by the installer.
    pub root_path: String,
    /// Resolved resources keyed by descriptor path.
    pub resources: BTreeMap<String, ResolvedResource>,
}

impl ResolvedResourceGraph {
    /// Return dependency-first install order with the root resource last.
    pub fn topo_order(&self) -> Result<Vec<String>> {
        fn visit(
            path: &str,
            graph: &BTreeMap<String, ResolvedResource>,
            temp: &mut BTreeSet<String>,
            perm: &mut BTreeSet<String>,
            out: &mut Vec<String>,
        ) -> Result<()> {
            if perm.contains(path) {
                return Ok(());
            }
            if !temp.insert(path.to_string()) {
                bail!("dependency cycle includes {path}");
            }
            let resource = graph
                .get(path)
                .ok_or_else(|| anyhow!("dependency {path} was not resolved"))?;
            for dep in resource.dependencies() {
                visit(&validate_source_path(&dep.path)?, graph, temp, perm, out)?;
            }
            temp.remove(path);
            perm.insert(path.to_string());
            out.push(path.to_string());
            Ok(())
        }

        let mut out = Vec::new();
        visit(
            &self.root_path,
            &self.resources,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut out,
        )?;
        if let Some(pos) = out.iter().position(|path| path == &self.root_path) {
            let root = out.remove(pos);
            out.push(root);
        }
        Ok(out)
    }

    /// Validate all dependency version requirements against resolved package versions.
    pub fn validate_versions(&self) -> Result<()> {
        for (path, resource) in &self.resources {
            for dep in resource.dependencies() {
                let Some(required) = dep.version.as_deref() else {
                    continue;
                };
                let dep_path = validate_source_path(&dep.path)?;
                let resolved = self
                    .resources
                    .get(&dep_path)
                    .ok_or_else(|| anyhow!("{path} depends on unresolved {dep_path}"))?;
                let actual = resolved.version().unwrap_or("0.0.0");
                if !version_satisfies(actual, required) {
                    bail!("{path} requires {dep_path} version {required}, got {actual}");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
/// GitHub repository location for a package catalog.
pub struct GitHubSource {
    /// Repository owner or organization.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Branch, tag, or commit reference to read from.
    pub reference: String,
    /// Repository-relative path to the catalog file.
    pub manifest_path: String,
}

#[derive(Debug, Clone)]
/// Fetches package catalogs and descriptors from GitHub raw content.
pub struct GitHubFetcher {
    source: GitHubSource,
    client: reqwest::Client,
}

impl GitHubFetcher {
    /// Create a fetcher for a GitHub package source.
    pub fn new(source: GitHubSource) -> Self {
        Self {
            source,
            client: reqwest::Client::new(),
        }
    }

    /// Return the configured GitHub source.
    pub fn source(&self) -> &GitHubSource {
        &self.source
    }

    /// Clone this fetcher with a different Git reference.
    pub fn with_reference(&self, reference: impl Into<String>) -> Self {
        let mut source = self.source.clone();
        source.reference = reference.into();
        Self {
            source,
            client: self.client.clone(),
        }
    }

    /// Resolve the configured branch, tag, or commit reference to a commit SHA.
    pub async fn resolve_ref(&self) -> Result<String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/commits/{}",
            self.source.owner, self.source.repo, self.source.reference
        );
        let value = self
            .client
            .get(&url)
            .header("User-Agent", "nenjo-packages")
            .send()
            .await
            .with_context(|| format!("failed to resolve GitHub ref {}", self.source.reference))?
            .error_for_status()
            .with_context(|| format!("GitHub ref resolution failed for {url}"))?
            .json::<serde_json::Value>()
            .await
            .with_context(|| format!("failed to parse GitHub commit response for {url}"))?;
        value
            .get("sha")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("GitHub commit response missing sha"))
    }

    /// Fetch a repository-relative text file from the configured GitHub reference.
    pub async fn fetch_text(&self, path: &str) -> Result<String> {
        let path = validate_source_path(path)?;
        let url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            self.source.owner, self.source.repo, self.source.reference, path
        );
        self.client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to request {url}"))?
            .error_for_status()
            .with_context(|| format!("failed to fetch {url}"))?
            .text()
            .await
            .with_context(|| format!("failed to read {url}"))
    }

    /// Fetch and validate the configured catalog, returning the original JSON value.
    pub async fn fetch_catalog_value(&self) -> Result<serde_json::Value> {
        let content = self.fetch_text(&self.source.manifest_path).await?;
        let catalog: PackageCatalog =
            parse_json_or_yaml_as(&content).context("failed to parse package catalog")?;
        catalog
            .validate()
            .context("failed to validate package catalog")?;
        parse_json_or_yaml(&content).context("failed to parse package catalog")
    }

    /// Fetch and validate the configured catalog.
    pub async fn fetch_catalog(&self) -> Result<PackageCatalog> {
        let content = self.fetch_text(&self.source.manifest_path).await?;
        let catalog: PackageCatalog =
            parse_json_or_yaml_as(&content).context("failed to parse package catalog")?;
        catalog
            .validate()
            .context("failed to validate package catalog")?;
        Ok(catalog)
    }

    /// Resolve a root descriptor and all package dependencies into a graph.
    pub async fn resolve_resource_graph(&self, root_path: &str) -> Result<ResolvedResourceGraph> {
        let root_path = validate_source_path(root_path)?;
        let mut resources = BTreeMap::new();
        let mut stack = vec![root_path.clone()];
        while let Some(path) = stack.pop() {
            if resources.contains_key(&path) {
                continue;
            }
            let descriptor_content = self.fetch_text(&path).await?;
            let descriptor: PackageDescriptor = parse_json_or_yaml_as(&descriptor_content)
                .with_context(|| format!("failed to parse package descriptor {path}"))?;
            descriptor.validate(&path)?;
            let entry_path = package_entry_path(&path, &descriptor.entry)?;
            let entry_content = self.fetch_text(&entry_path).await?;
            let manifest: ResourceManifest = parse_json_or_yaml_as(&entry_content)
                .with_context(|| format!("failed to parse resource manifest {entry_path}"))?;
            let resource_schema = manifest.resource_schema()?;
            manifest
                .name()
                .with_context(|| format!("failed to validate resource manifest {entry_path}"))?;
            if resource_schema.kind != descriptor.kind {
                bail!(
                    "{path} declares package type '{}' but {entry_path} is '{}'",
                    descriptor.kind.as_str(),
                    resource_schema.kind.as_str()
                );
            }
            let hash = sha256_hex(
                format!("{descriptor_content}\n---entry---\n{entry_content}").as_bytes(),
            );
            for dep in &descriptor.depends_on {
                stack.push(validate_source_path(&dep.path)?);
            }
            resources.insert(
                path.clone(),
                ResolvedResource {
                    path,
                    entry_path,
                    hash,
                    kind: descriptor.kind,
                    descriptor,
                    manifest,
                },
            );
        }
        let graph = ResolvedResourceGraph {
            root_path,
            resources,
        };
        graph.validate_versions()?;
        Ok(graph)
    }
}

#[derive(Debug, Clone)]
/// Filesystem-backed package resolver for local package development and tests.
pub struct LocalPackageResolver {
    root: PathBuf,
    repository_path: String,
}

impl LocalPackageResolver {
    /// Create a local resolver rooted at a package repository directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            repository_path: "packages.yaml".to_string(),
        }
    }

    /// Create a local resolver with an explicit repository manifest path.
    pub fn with_repository_path(
        root: impl Into<PathBuf>,
        repository_path: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            repository_path: repository_path.into(),
        }
    }

    /// Return the local repository root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the configured repository manifest path.
    pub fn repository_path(&self) -> &str {
        &self.repository_path
    }

    /// Read a repository-relative text file from the local package repository.
    pub fn read_text(&self, path: &str) -> Result<String> {
        let path = validate_source_path(path)?;
        fs::read_to_string(self.root.join(&path))
            .with_context(|| format!("failed to read local package file {path}"))
    }

    /// Load and validate the local repository manifest.
    pub fn load_repository(&self) -> Result<PackageRepositoryManifest> {
        let content = self.read_text(&self.repository_path)?;
        let repository: PackageRepositoryManifest =
            parse_json_or_yaml_as(&content).context("failed to parse package repository")?;
        repository
            .validate()
            .context("failed to validate package repository")?;
        Ok(repository)
    }

    /// Resolve a package and its dependencies from the local repository.
    pub fn resolve_package_graph(&self, root_package: &str) -> Result<ResolvedPackageGraph> {
        validate_package_name(root_package)?;
        let repository = self.load_repository()?;
        let mut packages = BTreeMap::new();
        let mut stack = vec![root_package.to_string()];

        while let Some(name) = stack.pop() {
            if packages.contains_key(&name) {
                continue;
            }
            let path = repository
                .packages
                .get(&name)
                .ok_or_else(|| anyhow!("package {name} is not listed in repository"))?;
            let package = self.resolve_package_manifest(path)?;
            if package.name != name {
                bail!(
                    "repository maps {name} to {path}, but package manifest declares {}",
                    package.name
                );
            }
            for dependency in package.dependencies().keys() {
                stack.push(dependency.clone());
            }
            packages.insert(name, package);
        }

        let graph = ResolvedPackageGraph {
            root_package: root_package.to_string(),
            packages,
        };
        graph.validate_versions()?;
        Ok(graph)
    }

    /// Resolve one package manifest and its included modules without following dependencies.
    pub fn resolve_package_manifest(&self, package_path: &str) -> Result<ResolvedPackage> {
        let package_path = validate_source_path(package_path)?;
        let package_content = self.read_text(&package_path)?;
        let manifest: ModulePackageManifest = parse_json_or_yaml_as(&package_content)
            .with_context(|| format!("failed to parse package manifest {package_path}"))?;
        manifest.validate(&package_path)?;

        let mut modules = BTreeMap::new();
        for module in &manifest.modules {
            let source_path = package_module_source_path(&package_path, &module.path)?;
            let module_content = self.read_text(&source_path)?;
            let resources = parse_module_file(&module_content, &source_path)?;
            let multiple_resources = resources.len() > 1;
            for resource_manifest in resources {
                let kind = resource_manifest.kind()?;
                resource_manifest
                    .name()
                    .with_context(|| format!("failed to validate module manifest {source_path}"))?;
                let resource_name = resource_manifest
                    .name()
                    .expect("resource name was just validated")
                    .to_string();
                let imports = resource_manifest.imports();
                let resolved = ResolvedModule {
                    package_name: manifest.name.clone(),
                    package_version: manifest.version.clone(),
                    path: module.path.clone(),
                    source_path: source_path.clone(),
                    hash: sha256_hex(module_content.as_bytes()),
                    kind,
                    manifest: resource_manifest,
                    imports,
                };
                let resource_key = format!("{}#{resource_name}", module.path);
                if modules
                    .insert(resource_key.clone(), resolved.clone())
                    .is_some()
                {
                    bail!("{source_path} declares duplicate resolved module '{resource_key}'");
                }
                if !multiple_resources && modules.insert(module.path.clone(), resolved).is_some() {
                    bail!(
                        "{source_path} declares duplicate resolved module '{}'",
                        module.path
                    );
                }
            }
        }

        for (name, export) in &manifest.exports {
            let target = ModuleTarget::parse(&export.path)?;
            if target.resource.is_some() {
                if !modules.contains_key(&target.key()) {
                    bail!(
                        "{package_path} export {name} points at '{}' which is not listed in modules",
                        export.path
                    );
                }
            } else if !modules.contains_key(&target.path)
                && !modules.contains_key(&target.key())
                && !modules
                    .keys()
                    .any(|key| key.starts_with(&format!("{}#", target.path)))
            {
                bail!(
                    "{package_path} export {name} points at '{}' which is not listed in modules",
                    export.path
                );
            }
        }

        Ok(ResolvedPackage {
            name: manifest.name.clone(),
            path: package_path,
            version: manifest.version.clone(),
            hash: sha256_hex(package_content.as_bytes()),
            manifest,
            modules,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Lockfile for an installed package graph.
pub struct PackageLock {
    /// Root package descriptor path requested by the user.
    pub root_path: String,
    /// Branch, tag, or commit reference requested by the user.
    pub requested_ref: String,
    /// Resolved Git commit SHA used for installation.
    pub resolved_commit_sha: String,
    /// Locked resources in install order.
    pub resources: Vec<PackageLockResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Lockfile entry for one resolved package resource.
pub struct PackageLockResource {
    /// Repository-relative descriptor path.
    pub path: String,
    /// Stable resource kind identifier.
    #[serde(rename = "type")]
    pub kind: String,
    /// Resource name from the manifest body.
    pub name: String,
    /// Optional package descriptor version.
    pub version: Option<String>,
    /// Platform resource identifier created or updated by install.
    pub resource_id: String,
    /// SHA-256 hash of the descriptor and manifest content.
    pub hash: String,
    /// Optional source selector used for source-managed replacement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

/// Validate a repository-relative package path and return its normalized form.
pub fn validate_source_path(path: &str) -> Result<String> {
    let raw = path.trim();
    if raw.is_empty() || raw.starts_with('/') || raw.contains("..") {
        bail!("invalid package path '{path}'");
    }
    let trimmed = raw.trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("invalid package path '{path}'");
    }
    Ok(trimmed.to_string())
}

/// Validate a registry package name.
pub fn validate_package_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        bail!("invalid package name '{name}'");
    }
    if trimmed.contains("..") || trimmed.contains('#') || trimmed.contains(':') {
        bail!("invalid package name '{name}'");
    }
    if trimmed.starts_with('@') {
        let Some((scope, package)) = trimmed.split_once('/') else {
            bail!("scoped package name '{name}' must include a package segment");
        };
        if scope.len() <= 1 || package.is_empty() || package.contains('/') {
            bail!("invalid scoped package name '{name}'");
        }
    } else if trimmed.contains('/') {
        bail!("unscoped package name '{name}' must not include '/'");
    }
    Ok(())
}

/// Validate a package export name.
pub fn validate_export_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed == "." {
        return Ok(());
    }
    if !trimmed.starts_with("./")
        || trimmed.contains("..")
        || trimmed.ends_with('/')
        || trimmed.trim_start_matches("./").is_empty()
    {
        bail!("invalid export name '{name}'");
    }
    Ok(())
}

/// Resolve a package descriptor's entry filename to a repository-relative path.
pub fn package_entry_path(package_path: &str, entry: &str) -> Result<String> {
    let package_path = validate_source_path(package_path)?;
    let entry = validate_source_path(entry)?;
    if entry.contains('/') {
        bail!("package entry must be relative to the package directory");
    }
    let Some((dir, _)) = package_path.rsplit_once('/') else {
        bail!("package descriptor path '{package_path}' must include a directory");
    };
    validate_source_path(&format!("{dir}/{entry}"))
}

/// Resolve a package-relative module path to a repository-relative source path.
pub fn package_module_source_path(package_path: &str, module_path: &str) -> Result<String> {
    let package_path = validate_source_path(package_path)?;
    let module_path = validate_source_path(module_path)?;
    let Some((dir, _)) = package_path.rsplit_once('/') else {
        return Ok(module_path);
    };
    validate_source_path(&format!("{dir}/{module_path}"))
}

/// Return whether a package version satisfies an exact or caret major requirement.
pub fn version_satisfies(actual: &str, required: &str) -> bool {
    let required = required.trim();
    if let Some(prefix) = required.strip_prefix('^') {
        let actual_major = actual.trim_start_matches('v').split('.').next();
        let required_major = prefix.trim_start_matches('v').split('.').next();
        return actual_major == required_major;
    }
    actual.trim_start_matches('v') == required.trim_start_matches('v')
}

/// Parse JSON or YAML content as a generic JSON value.
pub fn parse_json_or_yaml(content: &str) -> Result<serde_json::Value> {
    serde_json::from_str(content)
        .or_else(|_| serde_yaml::from_str(content))
        .context("failed to parse JSON or YAML")
}

/// Parse JSON or YAML content as a concrete deserializable type.
pub fn parse_json_or_yaml_as<T>(content: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(content)
        .or_else(|_| serde_yaml::from_str(content))
        .context("failed to parse JSON or YAML")
}

/// Return a `sha256:<hex>` digest string for the provided bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("nenjo-packages-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(root: &Path, path: &str, content: &str) {
        let full_path = root.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full_path, content).unwrap();
    }

    fn resolved_resource(
        path: &str,
        version: Option<&str>,
        depends_on: Vec<ResourceDependency>,
    ) -> ResolvedResource {
        ResolvedResource {
            path: path.to_string(),
            entry_path: path.replace("package.yaml", "agent.yaml"),
            hash: sha256_hex(path.as_bytes()),
            kind: PackageKind::Agent,
            descriptor: PackageDescriptor {
                schema: "nenjo.package.v1".to_string(),
                kind: PackageKind::Agent,
                slug: path.replace('/', "-"),
                name: path.to_string(),
                version: version.map(str::to_string),
                entry: "agent.yaml".to_string(),
                depends_on,
                metadata: serde_json::Value::Null,
            },
            manifest: ResourceManifest {
                schema: "nenjo.agent.v1".to_string(),
                slug: None,
                root_uri: None,
                selector: None,
                manifest: serde_json::json!({
                    "name": path,
                }),
            },
        }
    }

    #[test]
    fn parses_resource_schema_version() {
        let schema = ResourceSchema::parse("nenjo.agent.v1").unwrap();
        assert_eq!(schema.kind, PackageKind::Agent);
        assert_eq!(schema.version, ManifestSchemaVersion::V1);
    }

    #[test]
    fn parses_all_supported_resource_types() {
        let cases = [
            ("nenjo.agent.v1", PackageKind::Agent),
            ("nenjo.ability.v1", PackageKind::Ability),
            ("nenjo.domain.v1", PackageKind::Domain),
            ("nenjo.context_block.v1", PackageKind::ContextBlock),
            ("nenjo.knowledge.v1", PackageKind::Knowledge),
            ("nenjo.knowledge_ref.v1", PackageKind::Knowledge),
            ("nenjo.skill.v1", PackageKind::Skill),
            ("nenjo.plugin.v1", PackageKind::Plugin),
            ("nenjo.mcp_server.v1", PackageKind::McpServer),
            ("nenjo.routine.v1", PackageKind::Routine),
        ];

        for (schema, expected) in cases {
            assert_eq!(PackageKind::parse_schema(schema).unwrap(), expected);
            assert_eq!(
                ResourceSchema::parse(schema).unwrap().version.as_str(),
                "v1"
            );
        }
    }

    #[test]
    fn parses_and_serializes_package_adapters() {
        let cases = [
            ("nenjo_packages", PackageAdapter::NenjoPackages),
            ("claude_marketplace", PackageAdapter::ClaudeMarketplace),
            ("codex_plugin", PackageAdapter::CodexPlugin),
        ];

        for (adapter_name, expected) in cases {
            let parsed: PackageAdapter = adapter_name.parse().unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_str(), adapter_name);
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                serde_json::Value::String(adapter_name.to_string())
            );
        }
    }

    #[test]
    fn rejects_unknown_package_adapter() {
        let err = PackageAdapter::parse("unknown").unwrap_err().to_string();
        assert!(err.contains("unsupported package adapter"));
    }

    #[test]
    fn rejects_unversioned_resource_schema() {
        let err = ResourceSchema::parse("agent").unwrap_err().to_string();
        assert!(err.contains("must start with 'nenjo.'"));
    }

    #[test]
    fn rejects_unknown_resource_schema_version() {
        let err = ResourceSchema::parse("nenjo.agent.v2")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported version"));
    }

    #[test]
    fn validates_package_catalog_schema() {
        let catalog: PackageCatalog = parse_json_or_yaml_as(
            r#"
schema: nenjo.packages.v1
packages:
- type: agent
  slug: nenji
  path: nenjo/agents/nenji/package.yaml
"#,
        )
        .unwrap();
        assert_eq!(catalog.schema_version().unwrap(), ManifestSchemaVersion::V1);
        catalog.validate().unwrap();
    }

    #[test]
    fn rejects_wrong_package_file_schema_kind() {
        let err = PackageFileSchema::parse_descriptor("nenjo.packages.v1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected schema 'nenjo.package.*'"));
    }

    #[test]
    fn validates_package_descriptor_schema() {
        let descriptor: PackageDescriptor = parse_json_or_yaml_as(
            r#"
schema: nenjo.package.v1
type: ability
slug: build-agent
name: Build Agent
entry: ability.yaml
"#,
        )
        .unwrap();
        assert_eq!(
            descriptor.schema_version().unwrap(),
            ManifestSchemaVersion::V1
        );
        descriptor
            .validate("nenjo/abilities/build_agent/package.yaml")
            .unwrap();
    }

    #[test]
    fn validates_repository_manifest_schema() {
        let repository: PackageRepositoryManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.repository.v1
packages:
  "@nenjo/core": packages/core/nenjo.package.yaml
  "@nenjo/nenji": packages/nenji/nenjo.package.yaml
"#,
        )
        .unwrap();
        assert_eq!(
            repository.schema_version().unwrap(),
            ManifestSchemaVersion::V1
        );
        repository.validate().unwrap();
    }

    #[test]
    fn parses_module_package_manifest_with_string_modules_and_exports() {
        let package: ModulePackageManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.package.v1
name: "@nenjo/nenji"
version: "0.1.0"
dependencies:
  "@nenjo/core": "^0.1.0"
modules:
  - agents/nenji.yaml
  - path: abilities/design_agent.yaml
    metadata:
      optional: false
exports:
  ".": agents/nenji.yaml
  "./design-agent":
    path: abilities/design_agent.yaml
"#,
        )
        .unwrap();
        package
            .validate("packages/nenji/nenjo.package.yaml")
            .unwrap();
        assert_eq!(package.modules[0].path, "agents/nenji.yaml");
        assert_eq!(package.modules[1].path, "abilities/design_agent.yaml");
        assert_eq!(package.exports["."].path, "agents/nenji.yaml");
        assert_eq!(
            package.exports["./design-agent"].path,
            "abilities/design_agent.yaml"
        );
    }

    #[test]
    fn rejects_invalid_export_names() {
        let package: ModulePackageManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.package.v1
name: "@nenjo/bad"
version: "0.1.0"
modules:
  - agents/bad.yaml
exports:
  "agent": agents/bad.yaml
"#,
        )
        .unwrap();
        let err = package
            .validate("packages/bad/nenjo.package.yaml")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid export"));
    }

    #[test]
    fn reads_resource_manifest_body_name() {
        let manifest: ResourceManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.agent.v1
manifest:
  name: system
  display_name: Nenji
"#,
        )
        .unwrap();
        assert_eq!(manifest.name().unwrap(), "system");
    }

    #[test]
    fn reads_resource_manifest_body_version() {
        let manifest: ResourceManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.skill.v1
manifest:
  name: rust-review
  version: 1.2.3
"#,
        )
        .unwrap();
        assert_eq!(manifest.version(), Some("1.2.3"));
    }

    #[test]
    fn rejects_non_object_resource_manifest_body() {
        let manifest: ResourceManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.agent.v1
manifest: []
"#,
        )
        .unwrap();
        let err = manifest.manifest_object().unwrap_err().to_string();
        assert!(err.contains("must be an object"));
    }

    #[test]
    fn rejects_resource_manifest_without_name() {
        let manifest: ResourceManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.agent.v1
manifest:
  display_name: Nenji
"#,
        )
        .unwrap();
        let err = manifest.name().unwrap_err().to_string();
        assert!(err.contains("missing name"));
    }

    #[test]
    fn allows_selector_metadata_outside_manifest_body() {
        let manifest: ResourceManifest = parse_json_or_yaml_as(
            r#"
schema: nenjo.agent.v1
selector: git://nenjo-ai/packages/nenjo/agent
root_uri: git://nenjo-ai/packages/nenjo/agent/
manifest:
  name: system
  display_name: Nenji
"#,
        )
        .unwrap();
        assert_eq!(
            manifest.selector(),
            Some("git://nenjo-ai/packages/nenjo/agent")
        );
        assert_eq!(
            manifest.root_uri(),
            Some("git://nenjo-ai/packages/nenjo/agent/")
        );
        assert_eq!(manifest.name().unwrap(), "system");
    }

    #[test]
    fn caret_version_matches_major() {
        assert!(version_satisfies("0.1.2", "^0.1.0"));
        assert!(version_satisfies("v1.2.3", "^1.0.0"));
        assert!(!version_satisfies("2.0.0", "^1.0.0"));
    }

    #[test]
    fn exact_version_ignores_leading_v_prefix() {
        assert!(version_satisfies("v1.2.3", "1.2.3"));
        assert!(version_satisfies("1.2.3", "v1.2.3"));
        assert!(!version_satisfies("1.2.4", "1.2.3"));
    }

    #[test]
    fn source_path_rejects_escape() {
        assert!(validate_source_path("nenjo/agents/nenji.yaml").is_ok());
        assert!(validate_source_path("../nenjo/agents/nenji.yaml").is_err());
        assert!(validate_source_path("/nenjo/agents/nenji.yaml").is_err());
    }

    #[test]
    fn source_path_normalizes_relative_prefix_and_trailing_slash() {
        assert_eq!(
            validate_source_path("./nenjo/agents/nenji/").unwrap(),
            "nenjo/agents/nenji"
        );
    }

    #[test]
    fn package_entry_path_must_stay_in_descriptor_directory() {
        assert_eq!(
            package_entry_path("nenjo/agents/nenji/package.yaml", "agent.yaml").unwrap(),
            "nenjo/agents/nenji/agent.yaml"
        );
        let err = package_entry_path("nenjo/agents/nenji/package.yaml", "nested/agent.yaml")
            .unwrap_err()
            .to_string();
        assert!(err.contains("relative to the package directory"));
    }

    #[test]
    fn package_module_source_path_resolves_package_relative_paths() {
        assert_eq!(
            package_module_source_path("packages/nenji/nenjo.package.yaml", "agents/nenji.yaml")
                .unwrap(),
            "packages/nenji/agents/nenji.yaml"
        );
        assert!(
            package_module_source_path("packages/nenji/nenjo.package.yaml", "../agent.yaml")
                .is_err()
        );
    }

    #[test]
    fn graph_topo_order_places_dependencies_before_root() {
        let root_path = "packages/root/package.yaml".to_string();
        let dependency_path = "packages/dependency/package.yaml".to_string();
        let mut resources = BTreeMap::new();
        resources.insert(
            root_path.clone(),
            resolved_resource(
                &root_path,
                Some("1.0.0"),
                vec![ResourceDependency {
                    path: dependency_path.clone(),
                    version: Some("^2.0.0".to_string()),
                }],
            ),
        );
        resources.insert(
            dependency_path.clone(),
            resolved_resource(&dependency_path, Some("2.1.0"), Vec::new()),
        );

        let graph = ResolvedResourceGraph {
            root_path: root_path.clone(),
            resources,
        };
        assert_eq!(
            graph.topo_order().unwrap(),
            vec![dependency_path, root_path]
        );
        graph.validate_versions().unwrap();
    }

    #[test]
    fn graph_rejects_dependency_cycle() {
        let first_path = "packages/first/package.yaml".to_string();
        let second_path = "packages/second/package.yaml".to_string();
        let mut resources = BTreeMap::new();
        resources.insert(
            first_path.clone(),
            resolved_resource(
                &first_path,
                None,
                vec![ResourceDependency {
                    path: second_path.clone(),
                    version: None,
                }],
            ),
        );
        resources.insert(
            second_path,
            resolved_resource(
                "packages/second/package.yaml",
                None,
                vec![ResourceDependency {
                    path: first_path.clone(),
                    version: None,
                }],
            ),
        );

        let graph = ResolvedResourceGraph {
            root_path: first_path,
            resources,
        };
        let err = graph.topo_order().unwrap_err().to_string();
        assert!(err.contains("dependency cycle"));
    }

    #[test]
    fn graph_rejects_unsatisfied_dependency_version() {
        let root_path = "packages/root/package.yaml".to_string();
        let dependency_path = "packages/dependency/package.yaml".to_string();
        let mut resources = BTreeMap::new();
        resources.insert(
            root_path.clone(),
            resolved_resource(
                &root_path,
                Some("1.0.0"),
                vec![ResourceDependency {
                    path: dependency_path.clone(),
                    version: Some("^2.0.0".to_string()),
                }],
            ),
        );
        resources.insert(
            dependency_path,
            resolved_resource(
                "packages/dependency/package.yaml",
                Some("1.9.0"),
                Vec::new(),
            ),
        );

        let graph = ResolvedResourceGraph {
            root_path,
            resources,
        };
        let err = graph.validate_versions().unwrap_err().to_string();
        assert!(err.contains("requires packages/dependency/package.yaml version ^2.0.0"));
    }

    #[test]
    fn local_resolver_resolves_package_modules_and_dependencies() {
        let root = temp_repo("local-resolver");
        write_file(
            &root,
            "packages.yaml",
            r#"
schema: nenjo.repository.v1
packages:
  "@nenjo/core": packages/core/nenjo.package.yaml
  "@nenjo/nenji": packages/nenji/nenjo.package.yaml
"#,
        );
        write_file(
            &root,
            "packages/core/nenjo.package.yaml",
            r#"
schema: nenjo.package.v1
name: "@nenjo/core"
version: "0.1.0"
modules:
  - context_blocks/methodology.yaml
exports:
  "./methodology": context_blocks/methodology.yaml
"#,
        );
        write_file(
            &root,
            "packages/core/context_blocks/methodology.yaml",
            r#"
schema: nenjo.context_block.v1
manifest:
  name: methodology
  template: think clearly
"#,
        );
        write_file(
            &root,
            "packages/nenji/nenjo.package.yaml",
            r#"
schema: nenjo.package.v1
name: "@nenjo/nenji"
version: "0.1.0"
dependencies:
  "@nenjo/core": "^0.1.0"
modules:
  - agents/nenji.yaml
  - abilities/design_agent.yaml
exports:
  ".": agents/nenji.yaml
"#,
        );
        write_file(
            &root,
            "packages/nenji/agents/nenji.yaml",
            r#"
schema: nenjo.agent.v1
manifest:
  name: nenji
"#,
        );
        write_file(
            &root,
            "packages/nenji/abilities/design_agent.yaml",
            r#"
schema: nenjo.ability.v1
manifest:
  name: design_agent
"#,
        );

        let graph = LocalPackageResolver::new(&root)
            .resolve_package_graph("@nenjo/nenji")
            .unwrap();
        assert_eq!(
            graph.topo_order().unwrap(),
            vec!["@nenjo/core".to_string(), "@nenjo/nenji".to_string()]
        );
        let nenji = &graph.packages["@nenjo/nenji"];
        assert_eq!(nenji.modules.len(), 4);
        assert_eq!(
            nenji.modules["agents/nenji.yaml"].source_path,
            "packages/nenji/agents/nenji.yaml"
        );
        assert_eq!(nenji.modules["agents/nenji.yaml"].kind, PackageKind::Agent);
        assert_eq!(nenji.modules["agents/nenji.yaml"].name(), "nenji");
        assert_eq!(
            graph.packages["@nenjo/core"].modules["context_blocks/methodology.yaml"].kind,
            PackageKind::ContextBlock
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_resolver_rejects_export_to_unlisted_module() {
        let root = temp_repo("bad-export");
        write_file(
            &root,
            "packages.yaml",
            r#"
schema: nenjo.repository.v1
packages:
  "@nenjo/bad": packages/bad/nenjo.package.yaml
"#,
        );
        write_file(
            &root,
            "packages/bad/nenjo.package.yaml",
            r#"
schema: nenjo.package.v1
name: "@nenjo/bad"
version: "0.1.0"
modules:
  - agents/bad.yaml
exports:
  ".": agents/other.yaml
"#,
        );
        write_file(
            &root,
            "packages/bad/agents/bad.yaml",
            r#"
schema: nenjo.agent.v1
manifest:
  name: bad
"#,
        );

        let err = LocalPackageResolver::new(&root)
            .resolve_package_graph("@nenjo/bad")
            .unwrap_err()
            .to_string();
        assert!(err.contains("which is not listed in modules"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_resolver_rejects_unsatisfied_package_dependency() {
        let root = temp_repo("bad-version");
        write_file(
            &root,
            "packages.yaml",
            r#"
schema: nenjo.repository.v1
packages:
  "@nenjo/core": packages/core/nenjo.package.yaml
  "@nenjo/nenji": packages/nenji/nenjo.package.yaml
"#,
        );
        write_file(
            &root,
            "packages/core/nenjo.package.yaml",
            r#"
schema: nenjo.package.v1
name: "@nenjo/core"
version: "1.0.0"
modules:
  - context_blocks/core.yaml
"#,
        );
        write_file(
            &root,
            "packages/core/context_blocks/core.yaml",
            r#"
schema: nenjo.context_block.v1
manifest:
  name: core
"#,
        );
        write_file(
            &root,
            "packages/nenji/nenjo.package.yaml",
            r#"
schema: nenjo.package.v1
name: "@nenjo/nenji"
version: "0.1.0"
dependencies:
  "@nenjo/core": "^2.0.0"
modules:
  - agents/nenji.yaml
"#,
        );
        write_file(
            &root,
            "packages/nenji/agents/nenji.yaml",
            r#"
schema: nenjo.agent.v1
manifest:
  name: nenji
"#,
        );

        let err = LocalPackageResolver::new(&root)
            .resolve_package_graph("@nenjo/nenji")
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires @nenjo/core version ^2.0.0"));
        fs::remove_dir_all(root).unwrap();
    }
}
