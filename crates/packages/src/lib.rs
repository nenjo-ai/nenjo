//! Package catalog and manifest primitives for Nenjo package repositories.
//!
//! `nenjo-packages` handles the repository-facing package format: catalog files,
//! package descriptors, resource manifests, dependency graphs, GitHub fetching,
//! lockfile records, and small validation helpers. It intentionally keeps the
//! format-level logic independent from platform persistence so workers and
//! platform services can share the same package parsing rules.

use std::collections::{BTreeMap, BTreeSet};
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
}
