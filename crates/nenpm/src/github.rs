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
        if is_full_git_object_id(&self.source.reference) {
            return Ok(self.source.reference.clone());
        }
        if let Some(sha) = self.resolve_git_ref("heads").await? {
            return Ok(sha);
        }
        if let Some(sha) = self.resolve_git_ref("tags").await? {
            return Ok(sha);
        }
        let url = format!(
            "https://api.github.com/repos/{}/{}/commits/{}",
            self.source.owner,
            self.source.repo,
            urlencoding::encode(&self.source.reference)
        );
        let value = send_github_get(&self.client, &url, None)
            .await
            .with_context(|| format!("failed to resolve GitHub ref {}", self.source.reference))?;
        if should_fallback_to_unresolved_ref(value.status()) {
            return Ok(self.source.reference.clone());
        }
        let value = value
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

    async fn resolve_git_ref(&self, namespace: &str) -> Result<Option<String>> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/ref/{}/{}",
            self.source.owner,
            self.source.repo,
            namespace,
            encode_path(&self.source.reference)
        );
        let response = send_github_get(&self.client, &url, None)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve GitHub {namespace} ref {}",
                    self.source.reference
                )
            })?;
        if should_ignore_ref_probe_status(response.status()) {
            return Ok(None);
        }
        let value = response
            .error_for_status()
            .with_context(|| format!("GitHub {namespace} ref resolution failed for {url}"))?
            .json::<serde_json::Value>()
            .await
            .with_context(|| format!("failed to parse GitHub ref response for {url}"))?;
        let object = value
            .get("object")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| anyhow!("GitHub ref response missing object"))?;
        let kind = object.get("type").and_then(serde_json::Value::as_str);
        if kind != Some("commit") {
            return Ok(None);
        }
        object
            .get("sha")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| anyhow!("GitHub ref response missing object.sha"))
    }

    /// Fetch one repository-relative text file from the configured reference.
    pub async fn fetch_text(&self, path: &str) -> Result<String> {
        let path = validate_source_path(path)?;
        let url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            self.source.owner,
            self.source.repo,
            urlencoding::encode(&self.source.reference),
            encode_path(&path)
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

async fn send_github_get(
    client: &reqwest::Client,
    url: &str,
    accept: Option<&'static str>,
) -> Result<reqwest::Response> {
    let response = github_get(client, url, true, accept)
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?;
    if should_retry_without_auth(response.status()) && github_token().is_some() {
        return github_get(client, url, false, accept)
            .send()
            .await
            .with_context(|| format!("failed to retry unauthenticated request {url}"));
    }
    Ok(response)
}

fn github_get(
    client: &reqwest::Client,
    url: &str,
    allow_auth: bool,
    accept: Option<&'static str>,
) -> reqwest::RequestBuilder {
    let mut request = client.get(url).header("User-Agent", "nenjo-nenpm");
    if let Some(accept) = accept {
        request = request.header(reqwest::header::ACCEPT, accept);
    }
    if allow_auth && let Some(token) = github_token() {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
    }
    request
}

fn github_token() -> Option<String> {
    ["GITHUB_TOKEN", "GH_TOKEN"].into_iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn should_retry_without_auth(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::UNAUTHORIZED
            | reqwest::StatusCode::FORBIDDEN
            | reqwest::StatusCode::NOT_FOUND
    )
}

fn should_ignore_ref_probe_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::UNAUTHORIZED
            | reqwest::StatusCode::FORBIDDEN
            | reqwest::StatusCode::NOT_FOUND
    )
}

fn should_fallback_to_unresolved_ref(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    )
}

fn is_full_git_object_id(reference: &str) -> bool {
    matches!(reference.len(), 40 | 64) && reference.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_auth_retry_covers_forbidden_token_failures() {
        assert!(should_retry_without_auth(reqwest::StatusCode::UNAUTHORIZED));
        assert!(should_retry_without_auth(reqwest::StatusCode::FORBIDDEN));
        assert!(should_retry_without_auth(reqwest::StatusCode::NOT_FOUND));
    }

    #[test]
    fn github_ref_probe_failures_are_non_fatal() {
        assert!(should_ignore_ref_probe_status(
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert!(should_ignore_ref_probe_status(
            reqwest::StatusCode::FORBIDDEN
        ));
        assert!(should_ignore_ref_probe_status(
            reqwest::StatusCode::NOT_FOUND
        ));
        assert!(should_fallback_to_unresolved_ref(
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert!(should_fallback_to_unresolved_ref(
            reqwest::StatusCode::FORBIDDEN
        ));
        assert!(!should_fallback_to_unresolved_ref(
            reqwest::StatusCode::NOT_FOUND
        ));
    }
}
