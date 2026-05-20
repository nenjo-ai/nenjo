use anyhow::{Context, Result, anyhow};
use nenjo_packages::{GitHubSource, validate_source_path};

/// Raw GitHub content fetcher for repository-backed package sources.
///
/// This fetcher reads individual files from GitHub instead of cloning the
/// repository. It is intended for server-side package catalog and install paths
/// where cloning in the request path would be too heavy.
#[derive(Clone)]
pub struct GitHubRawFetcher {
    source: GitHubSource,
    client: reqwest::Client,
}

impl GitHubRawFetcher {
    /// Create a raw GitHub fetcher for a package source.
    pub fn new(source: GitHubSource) -> Self {
        Self {
            source,
            client: reqwest::Client::new(),
        }
    }

    /// Return the configured package source.
    pub fn source(&self) -> &GitHubSource {
        &self.source
    }

    /// Return the configured repository manifest path.
    pub fn manifest_path(&self) -> &str {
        &self.source.manifest_path
    }

    /// Clone this fetcher pinned to a concrete Git reference.
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
            .header("User-Agent", "nenjo-nenpm")
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

    /// Fetch one repository-relative text file from the configured reference.
    pub async fn fetch_text(&self, path: &str) -> Result<String> {
        let path = validate_source_path(path)?;
        let url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            self.source.owner, self.source.repo, self.source.reference, path
        );
        self.client
            .get(&url)
            .header("User-Agent", "nenjo-nenpm")
            .send()
            .await
            .with_context(|| format!("failed to request {url}"))?
            .error_for_status()
            .with_context(|| format!("failed to fetch {url}"))?
            .text()
            .await
            .with_context(|| format!("failed to read {url}"))
    }
}
