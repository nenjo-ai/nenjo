use anyhow::Context;
use std::collections::BTreeMap;

use crate::{
    PackageCatalog, PackageDescriptor, PackageError, ResolvedResource, ResolvedResourceGraph,
    ResourceManifest, Result, package_entry_path, parse_json_or_yaml, parse_json_or_yaml_as,
    sha256_hex, validate_source_path,
};

#[derive(Debug, Clone)]
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
            .ok_or_else(|| PackageError::fetch("GitHub commit response missing sha"))
    }

    /// Fetch a repository-relative text file from the configured GitHub reference.
    pub async fn fetch_text(&self, path: &str) -> Result<String> {
        let path = validate_source_path(path)?;
        let url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            self.source.owner, self.source.repo, self.source.reference, path
        );
        Ok(self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to request {url}"))?
            .error_for_status()
            .with_context(|| format!("failed to fetch {url}"))?
            .text()
            .await
            .with_context(|| format!("failed to read {url}"))?)
    }

    /// Fetch and validate the configured catalog, returning the original JSON value.
    pub async fn fetch_catalog_value(&self) -> Result<serde_json::Value> {
        let content = self.fetch_text(&self.source.manifest_path).await?;
        let catalog: PackageCatalog =
            parse_json_or_yaml_as(&content).context("failed to parse package catalog")?;
        catalog
            .validate()
            .context("failed to validate package catalog")?;
        parse_json_or_yaml(&content)
            .map_err(|error| error.context("failed to parse package catalog"))
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
