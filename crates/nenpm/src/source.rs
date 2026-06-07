use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::Result;
use anyhow::Context;
use flate2::read::GzDecoder;
use nenjo_packages::{
    ClaudePluginCommand, ClaudePluginHook, ModulePackageManifest, PackageModule,
    claude_plugin_resources, detect_unsupported_claude_plugin_components,
    parse_claude_plugin_command, parse_claude_plugin_hooks, parse_claude_plugin_manifest,
    parse_claude_plugin_mcp_servers, parse_claude_plugin_skill, sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;

const NENPM_FETCH_MODE_ENV: &str = "NENPM_FETCH_MODE";
const PROVIDER_USER_AGENT: &str = "nenjo-nenpm";
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_MAX_FILES: usize = 10_000;
const PROVIDER_MAX_BYTES: usize = 50 * 1024 * 1024;
const PROVIDER_MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

/// Strategy for fetching git-backed package sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    /// Use `git clone` for git-backed package sources.
    Git,
    /// Use provider APIs for supported git hosts and fail for unsupported hosts.
    Provider,
    /// Use provider APIs for supported git hosts and fall back to `git clone`.
    Auto,
}

impl FetchMode {
    /// Parse a fetch mode from a user-facing string.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "git" | "clone" => Ok(Self::Git),
            "provider" | "raw" => Ok(Self::Provider),
            "auto" => Ok(Self::Auto),
            other => {
                bail!("unsupported fetch mode '{other}'; expected git, provider, raw, or auto")
            }
        }
    }

    /// Read `NENPM_FETCH_MODE`, defaulting to `git` when it is unset.
    pub fn from_env() -> Result<Self> {
        match std::env::var(NENPM_FETCH_MODE_ENV) {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::Git),
            Err(error) => Err(crate::NenpmError::source(error.to_string())),
        }
    }
}

/// Source location for a registry package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageSource {
    /// Package content comes from a git repository.
    Git {
        /// Git remote URL.
        url: String,
        /// Branch, tag, or commit reference.
        reference: String,
        /// Repository-relative package manifest path.
        manifest_path: String,
    },
    /// Package content comes from an immutable package artifact.
    Artifact {
        /// Artifact URL.
        url: String,
        /// Expected artifact checksum.
        checksum: String,
        /// Artifact-relative package manifest path.
        manifest_path: String,
    },
    /// Package content comes from a direct remote package manifest.
    Remote {
        /// Remote package manifest URL.
        url: String,
        /// Expected manifest checksum when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        checksum: Option<String>,
    },
    /// Package content comes from a local repository checkout.
    Local {
        /// Local repository root.
        root: PathBuf,
        /// Repository-relative package manifest path.
        manifest_path: String,
        /// Package scope used when the local source points at a registry manifest.
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

impl PackageSource {
    /// Return the package manifest path when the source has one.
    pub fn manifest_path(&self) -> Option<&str> {
        match self {
            Self::Git { manifest_path, .. }
            | Self::Artifact { manifest_path, .. }
            | Self::Local { manifest_path, .. } => Some(manifest_path),
            Self::Remote { .. } => None,
        }
    }
}

/// Locally available package source fetched from a registry source record.
#[derive(Debug)]
pub struct FetchedPackageSource {
    /// Local root containing package files.
    pub root: PathBuf,
    /// Root-relative package manifest path.
    pub manifest_path: String,
    _temp_dir: Option<TempDir>,
}

impl FetchedPackageSource {
    fn local(root: PathBuf, manifest_path: String) -> Self {
        Self {
            root,
            manifest_path,
            _temp_dir: None,
        }
    }

    fn temporary(root: PathBuf, manifest_path: String, temp_dir: TempDir) -> Self {
        Self {
            root,
            manifest_path,
            _temp_dir: Some(temp_dir),
        }
    }
}

/// Fetches package sources into local roots for manifest/module resolution.
pub trait PackageSourceFetcher {
    /// Fetch a source record into a local root.
    fn fetch(&self, source: &PackageSource) -> Result<FetchedPackageSource>;
}

/// Default package source fetcher for local, git, artifact, and remote sources.
#[derive(Debug, Clone)]
pub struct DefaultPackageSourceFetcher {
    fetch_mode: FetchModeSelection,
}

#[derive(Debug, Clone)]
enum FetchModeSelection {
    Mode(FetchMode),
    InvalidEnv(String),
}

impl FetchModeSelection {
    fn from_env() -> Self {
        match FetchMode::from_env() {
            Ok(fetch_mode) => Self::Mode(fetch_mode),
            Err(error) => Self::InvalidEnv(error.to_string()),
        }
    }

    fn resolve(&self) -> Result<FetchMode> {
        match self {
            Self::Mode(fetch_mode) => Ok(*fetch_mode),
            Self::InvalidEnv(error) => Err(crate::NenpmError::source(format!(
                "invalid {NENPM_FETCH_MODE_ENV}: {error}"
            ))),
        }
    }
}

impl DefaultPackageSourceFetcher {
    /// Create the default source fetcher.
    pub fn new() -> Self {
        Self {
            fetch_mode: FetchModeSelection::from_env(),
        }
    }

    /// Create a source fetcher with an explicit git fetch mode.
    pub fn with_fetch_mode(fetch_mode: FetchMode) -> Self {
        Self {
            fetch_mode: FetchModeSelection::Mode(fetch_mode),
        }
    }

    /// Return the configured git fetch mode.
    pub fn fetch_mode(&self) -> Result<FetchMode> {
        self.fetch_mode.resolve()
    }
}

impl Default for DefaultPackageSourceFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageSourceFetcher for DefaultPackageSourceFetcher {
    fn fetch(&self, source: &PackageSource) -> Result<FetchedPackageSource> {
        let fetch_mode = self.fetch_mode.resolve()?;
        let fetched = match source {
            PackageSource::Local {
                root,
                manifest_path,
                ..
            } => Ok(FetchedPackageSource::local(
                root.clone(),
                manifest_path.clone(),
            )),
            PackageSource::Git {
                url,
                reference,
                manifest_path,
            } => fetch_git_source_for_mode(url, reference, manifest_path, fetch_mode),
            PackageSource::Artifact {
                url,
                checksum,
                manifest_path,
            } => fetch_artifact_source(url, checksum, manifest_path),
            PackageSource::Remote { url, checksum } => fetch_remote_manifest(url, checksum),
        }?;
        adapt_claude_plugin_source(fetched)
    }
}

pub(crate) fn normalize_source_paths(source: PackageSource, manifest_dir: &Path) -> PackageSource {
    match source {
        PackageSource::Local {
            root,
            manifest_path,
            scope,
        } => {
            let root = if root.is_absolute() {
                root
            } else {
                manifest_dir.join(root)
            };
            PackageSource::Local {
                root,
                manifest_path,
                scope,
            }
        }
        other => other,
    }
}

pub(crate) fn normalize_fetch_url(source: &mut PackageSource, base_dir: &Path) {
    match source {
        PackageSource::Artifact { url, .. } | PackageSource::Remote { url, .. } => {
            if is_relative_file_reference(url) {
                *url = base_dir.join(&url).to_string_lossy().to_string();
            }
        }
        PackageSource::Git { .. } | PackageSource::Local { .. } => {}
    }
}

pub(crate) fn validate_package_source(source: &PackageSource) -> Result<()> {
    if let Some(manifest_path) = source.manifest_path() {
        nenjo_packages::validate_source_path(manifest_path)
            .context("package source manifest path is invalid")?;
    }
    match source {
        PackageSource::Git { url, reference, .. } => {
            if url.trim().is_empty() {
                bail!("git source url cannot be empty");
            }
            if reference.trim().is_empty() {
                bail!("git source reference cannot be empty");
            }
        }
        PackageSource::Artifact { url, checksum, .. } => {
            if url.trim().is_empty() {
                bail!("artifact source url cannot be empty");
            }
            if checksum.trim().is_empty() {
                bail!("artifact source checksum cannot be empty");
            }
        }
        PackageSource::Remote { url, .. } => {
            if url.trim().is_empty() {
                bail!("remote source url cannot be empty");
            }
        }
        PackageSource::Local { root, scope, .. } => {
            if root.as_os_str().is_empty() {
                bail!("local source root cannot be empty");
            }
            if let Some(scope) = scope {
                validate_local_scope(scope)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn source_fetch_key(source: &PackageSource) -> String {
    match source {
        PackageSource::Git { url, reference, .. } => format!("git:{url}#{reference}"),
        PackageSource::Artifact { url, checksum, .. } => format!("artifact:{url}#{checksum}"),
        PackageSource::Remote { url, checksum } => {
            format!("remote:{url}#{}", checksum.as_deref().unwrap_or(""))
        }
        PackageSource::Local { root, .. } => format!("local:{}", root.display()),
    }
}

pub fn package_source_scope(source: &PackageSource) -> Option<String> {
    match source {
        PackageSource::Git { url, .. } => git_namespace_from_url(url).map(git_namespace_to_scope),
        PackageSource::Artifact { .. } | PackageSource::Remote { .. } => None,
        PackageSource::Local { scope, .. } => scope.clone(),
    }
}

fn validate_local_scope(scope: &str) -> Result<()> {
    if !scope.starts_with('@') || scope.contains('/') {
        bail!("local registry scope must look like @scope");
    }
    nenjo_packages::validate_package_name(&format!("{scope}/package"))
        .context("local registry scope is invalid")?;
    Ok(())
}

fn git_namespace_to_scope(namespace: String) -> String {
    format!("@{namespace}")
}

fn git_namespace_from_url(url: &str) -> Option<String> {
    if let Some((owner, _)) = parse_github_url(url) {
        return Some(owner);
    }
    let (_, path) = parse_gitlab_url(url)?;
    let namespace = path.split('/').next()?.trim();
    if namespace.is_empty() || namespace == "." || namespace == ".." {
        return None;
    }
    Some(namespace.to_string())
}

pub(crate) fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return Ok(fs::read(path).with_context(|| format!("failed to read {url}"))?);
    }
    if !url.contains("://") {
        return Ok(fs::read(url).with_context(|| format!("failed to read {url}"))?);
    }
    let response = reqwest::blocking::get(url)
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {url}"))?;
    Ok(response
        .bytes()
        .map(|bytes| bytes.to_vec())
        .with_context(|| format!("failed to read response body for {url}"))?)
}

fn is_relative_file_reference(value: &str) -> bool {
    !value.contains("://") && !Path::new(value).is_absolute()
}

fn fetch_git_source_for_mode(
    url: &str,
    reference: &str,
    manifest_path: &str,
    fetch_mode: FetchMode,
) -> Result<FetchedPackageSource> {
    match fetch_mode {
        FetchMode::Git => fetch_git_source(url, reference, manifest_path),
        FetchMode::Provider => fetch_provider_git_source(url, reference, manifest_path),
        FetchMode::Auto => {
            if ProviderGitSource::parse(url, reference, manifest_path).is_some() {
                fetch_provider_git_source(url, reference, manifest_path)
            } else {
                fetch_git_source(url, reference, manifest_path)
            }
        }
    }
}

fn fetch_git_source(
    url: &str,
    reference: &str,
    manifest_path: &str,
) -> Result<FetchedPackageSource> {
    let temp_dir = tempfile::tempdir().context("failed to create git fetch temp dir")?;
    let checkout = temp_dir.path().join("repo");
    let shallow_status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(reference)
        .arg(url)
        .arg(&checkout)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run git clone for {url}"))?;

    if !shallow_status.success() {
        if checkout.exists() {
            fs::remove_dir_all(&checkout).context("failed to clean failed git checkout")?;
        }
        let clone_status = Command::new("git")
            .arg("clone")
            .arg(url)
            .arg(&checkout)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to run git clone for {url}"))?;
        if !clone_status.success() {
            bail!("git clone failed for {url}");
        }
        let checkout_status = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .arg("checkout")
            .arg(reference)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to run git checkout {reference} for {url}"))?;
        if !checkout_status.success() {
            bail!("git checkout failed for {url} at {reference}");
        }
    }
    Ok(FetchedPackageSource::temporary(
        checkout,
        manifest_path.to_string(),
        temp_dir,
    ))
}

fn fetch_provider_git_source(
    url: &str,
    reference: &str,
    manifest_path: &str,
) -> Result<FetchedPackageSource> {
    let Some(source) = ProviderGitSource::parse(url, reference, manifest_path) else {
        bail!("provider fetch mode does not support git source {url}");
    };
    let manifest_path = nenjo_packages::validate_source_path(manifest_path)
        .context("provider source manifest path is invalid")?;
    let client = provider_client()?;
    let resolved_ref = source.resolve_ref(&client)?;
    let files = source.list_files(&client, &resolved_ref)?;
    if files.len() > PROVIDER_MAX_FILES {
        bail!(
            "provider source {} contains {} files, which exceeds the limit of {}",
            source.display_url(),
            files.len(),
            PROVIDER_MAX_FILES
        );
    }
    if !files.iter().any(|path| path == &manifest_path) {
        bail!(
            "provider source {} at {resolved_ref} does not contain {manifest_path}",
            source.display_url()
        );
    }

    let temp_dir = tempfile::tempdir().context("failed to create provider fetch temp dir")?;
    let root = temp_dir.path().join("provider");
    fs::create_dir_all(&root).context("failed to create provider fetch dir")?;

    let mut total_bytes = 0usize;
    for path in files {
        let path = nenjo_packages::validate_source_path(&path)
            .with_context(|| format!("provider returned invalid path {path}"))?;
        let bytes = source.fetch_file(&client, &resolved_ref, &path)?;
        if bytes.len() > PROVIDER_MAX_FILE_BYTES {
            bail!(
                "provider source file {path} is {} bytes, which exceeds the per-file limit of {}",
                bytes.len(),
                PROVIDER_MAX_FILE_BYTES
            );
        }
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > PROVIDER_MAX_BYTES {
            bail!(
                "provider source {} exceeds the total fetch limit of {} bytes",
                source.display_url(),
                PROVIDER_MAX_BYTES
            );
        }
        let output_path = root.join(&path);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&output_path, bytes)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
    }

    Ok(FetchedPackageSource::temporary(
        root,
        manifest_path,
        temp_dir,
    ))
}

#[derive(Debug, Clone)]
enum ProviderGitSource {
    GitHub {
        owner: String,
        repo: String,
        reference: String,
    },
    GitLab {
        host: String,
        project_path: String,
        reference: String,
    },
}

impl ProviderGitSource {
    fn parse(url: &str, reference: &str, manifest_path: &str) -> Option<Self> {
        nenjo_packages::validate_source_path(manifest_path).ok()?;
        parse_github_url(url)
            .map(|(owner, repo)| Self::GitHub {
                owner,
                repo,
                reference: reference.to_string(),
            })
            .or_else(|| {
                parse_gitlab_url(url).map(|(host, project_path)| Self::GitLab {
                    host,
                    project_path,
                    reference: reference.to_string(),
                })
            })
    }

    fn display_url(&self) -> String {
        match self {
            Self::GitHub { owner, repo, .. } => format!("https://github.com/{owner}/{repo}.git"),
            Self::GitLab {
                host, project_path, ..
            } => format!("https://{host}/{project_path}.git"),
        }
    }

    fn resolve_ref(&self, client: &reqwest::blocking::Client) -> Result<String> {
        match self {
            Self::GitHub {
                owner,
                repo,
                reference,
            } => {
                let url = format!(
                    "https://api.github.com/repos/{owner}/{repo}/commits/{}",
                    encode_path_segment(reference)
                );
                let value: GitHubCommitResponse = get_json(client, &url)
                    .with_context(|| format!("failed to resolve GitHub ref {reference}"))?;
                Ok(value.sha)
            }
            Self::GitLab {
                host,
                project_path,
                reference,
            } => {
                let url = format!(
                    "https://{host}/api/v4/projects/{}/repository/commits/{}",
                    encode_gitlab_project_path(project_path),
                    encode_path_segment(reference)
                );
                let value: GitLabCommitResponse = get_json(client, &url)
                    .with_context(|| format!("failed to resolve GitLab ref {reference}"))?;
                Ok(value.id)
            }
        }
    }

    fn list_files(
        &self,
        client: &reqwest::blocking::Client,
        resolved_ref: &str,
    ) -> Result<Vec<String>> {
        match self {
            Self::GitHub { owner, repo, .. } => {
                let url = format!(
                    "https://api.github.com/repos/{owner}/{repo}/git/trees/{}?recursive=1",
                    encode_path_segment(resolved_ref)
                );
                let value: GitHubTreeResponse = get_json(client, &url)
                    .with_context(|| format!("failed to list GitHub tree for {owner}/{repo}"))?;
                if value.truncated.unwrap_or(false) {
                    bail!(
                        "GitHub tree for {owner}/{repo} is truncated; provider fetch cannot continue safely"
                    );
                }
                Ok(value
                    .tree
                    .into_iter()
                    .filter(|entry| entry.kind == "blob")
                    .map(|entry| entry.path)
                    .collect())
            }
            Self::GitLab {
                host, project_path, ..
            } => {
                let mut files = Vec::new();
                let mut page = 1usize;
                loop {
                    let url = format!(
                        "https://{host}/api/v4/projects/{}/repository/tree?ref={}&recursive=true&per_page=100&page={page}",
                        encode_gitlab_project_path(project_path),
                        urlencoding::encode(resolved_ref)
                    );
                    let page_files: Vec<GitLabTreeEntry> =
                        get_json(client, &url).with_context(|| {
                            format!("failed to list GitLab tree for {project_path}")
                        })?;
                    if page_files.is_empty() {
                        break;
                    }
                    files.extend(
                        page_files
                            .into_iter()
                            .filter(|entry| entry.kind == "blob")
                            .map(|entry| entry.path),
                    );
                    page += 1;
                    if page > 1_000 {
                        bail!("GitLab tree for {project_path} exceeded pagination limit");
                    }
                }
                Ok(files)
            }
        }
    }

    fn fetch_file(
        &self,
        client: &reqwest::blocking::Client,
        resolved_ref: &str,
        path: &str,
    ) -> Result<Vec<u8>> {
        match self {
            Self::GitHub { owner, repo, .. } => {
                let url = format!(
                    "https://raw.githubusercontent.com/{owner}/{repo}/{}/{}",
                    encode_path_segment(resolved_ref),
                    encode_path(path)
                );
                Ok(get_bytes(client, &url)
                    .with_context(|| format!("failed to fetch GitHub file {path}"))?)
            }
            Self::GitLab {
                host, project_path, ..
            } => {
                let url = format!(
                    "https://{host}/api/v4/projects/{}/repository/files/{}/raw?ref={}",
                    encode_gitlab_project_path(project_path),
                    encode_gitlab_file_path(path),
                    urlencoding::encode(resolved_ref)
                );
                Ok(get_bytes(client, &url)
                    .with_context(|| format!("failed to fetch GitLab file {path}"))?)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct GitHubCommitResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeResponse {
    tree: Vec<GitHubTreeEntry>,
    truncated: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct GitLabCommitResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GitLabTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

fn provider_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(PROVIDER_TIMEOUT)
        .user_agent(PROVIDER_USER_AGENT)
        .build()
        .context("failed to build provider HTTP client")
        .map_err(Into::into)
}

fn get_json<T>(client: &reqwest::blocking::Client, url: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    Ok(provider_get(client, url)
        .send()
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {url}"))?
        .json()
        .with_context(|| format!("failed to parse JSON from {url}"))?)
}

fn get_bytes(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    Ok(provider_get(client, url)
        .send()
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {url}"))?
        .bytes()
        .with_context(|| format!("failed to read response body from {url}"))?
        .to_vec())
}

fn provider_get(
    client: &reqwest::blocking::Client,
    url: &str,
) -> reqwest::blocking::RequestBuilder {
    let request = client.get(url);
    if (url.contains("github.com/") || url.contains("githubusercontent.com/"))
        && let Some(token) = env_token(["GITHUB_TOKEN", "GH_TOKEN"])
    {
        return request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if url.contains("gitlab.com/")
        && let Some(token) = env_token(["GITLAB_TOKEN", "GL_TOKEN"])
    {
        return request.header("PRIVATE-TOKEN", token);
    }
    request
}

fn env_token<const N: usize>(names: [&str; N]) -> Option<String> {
    names.into_iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn parse_github_url(url: &str) -> Option<(String, String)> {
    let path = git_url_host_path(url, "github.com")?;
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn parse_gitlab_url(url: &str) -> Option<(String, String)> {
    let (host, path) = git_url_host_and_path(url)?;
    if host != "gitlab.com" || !path.contains('/') {
        return None;
    }
    Some((host, path))
}

fn git_url_host_path(url: &str, expected_host: &str) -> Option<String> {
    let (host, path) = git_url_host_and_path(url)?;
    (host == expected_host).then_some(path)
}

fn git_url_host_and_path(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let (host, path) = if let Some(rest) = trimmed.strip_prefix("https://") {
        rest.split_once('/')?
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        rest.split_once('/')?
    } else if let Some(rest) = trimmed.strip_prefix("ssh://git@") {
        rest.split_once('/')?
    } else if let Some(rest) = trimmed.strip_prefix("git@") {
        rest.split_once(':')?
    } else {
        return None;
    };
    let path = path.trim_matches('/');
    if host.is_empty() || path.is_empty() || path.contains("..") {
        return None;
    }
    Some((host.to_ascii_lowercase(), path.to_string()))
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_path_segment(segment: &str) -> String {
    urlencoding::encode(segment).into_owned()
}

fn encode_gitlab_project_path(path: &str) -> String {
    urlencoding::encode(path).into_owned()
}

fn encode_gitlab_file_path(path: &str) -> String {
    urlencoding::encode(path).into_owned()
}

fn fetch_artifact_source(
    url: &str,
    checksum: &str,
    manifest_path: &str,
) -> Result<FetchedPackageSource> {
    let bytes = fetch_bytes(url).with_context(|| format!("failed to fetch artifact {url}"))?;
    let actual = sha256_hex(&bytes);
    if actual != checksum {
        bail!("artifact checksum mismatch for {url}: expected {checksum}, got {actual}");
    }

    let temp_dir = tempfile::tempdir().context("failed to create artifact temp dir")?;
    let root = temp_dir.path().join("artifact");
    fs::create_dir_all(&root).context("failed to create artifact extraction dir")?;
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(&root)
        .with_context(|| format!("failed to unpack artifact {url}"))?;
    Ok(FetchedPackageSource::temporary(
        root,
        manifest_path.to_string(),
        temp_dir,
    ))
}

fn fetch_remote_manifest(url: &str, checksum: &Option<String>) -> Result<FetchedPackageSource> {
    let bytes =
        fetch_bytes(url).with_context(|| format!("failed to fetch remote manifest {url}"))?;
    if let Some(expected) = checksum {
        let actual = sha256_hex(&bytes);
        if &actual != expected {
            bail!("remote manifest checksum mismatch for {url}: expected {expected}, got {actual}");
        }
    }
    let temp_dir = tempfile::tempdir().context("failed to create remote manifest temp dir")?;
    let path = temp_dir.path().join("remote.package.yaml");
    let mut file = fs::File::create(&path).context("failed to create remote package manifest")?;
    file.write_all(&bytes)
        .context("failed to write remote package manifest")?;
    Ok(FetchedPackageSource::temporary(
        temp_dir.path().to_path_buf(),
        "remote.package.yaml".to_string(),
        temp_dir,
    ))
}

fn adapt_claude_plugin_source(fetched: FetchedPackageSource) -> Result<FetchedPackageSource> {
    let Some(plugin_root_path) = claude_plugin_root_path(&fetched.manifest_path) else {
        return Ok(fetched);
    };
    let plugin_root = if plugin_root_path == "." {
        fetched.root.clone()
    } else {
        fetched.root.join(&plugin_root_path)
    };
    if !plugin_root.is_dir() {
        bail!(
            "Claude plugin root {} does not exist",
            plugin_root.display()
        );
    }

    let temp_dir = tempfile::tempdir().context("failed to create Claude plugin temp dir")?;
    let synthetic_root = temp_dir.path().join("plugin");
    copy_dir_all(&plugin_root, &synthetic_root).with_context(|| {
        format!(
            "failed to copy Claude plugin root {}",
            plugin_root.display()
        )
    })?;
    write_synthetic_claude_plugin_package(&synthetic_root)?;
    Ok(FetchedPackageSource::temporary(
        synthetic_root,
        "package.yaml".to_string(),
        temp_dir,
    ))
}

fn claude_plugin_root_path(manifest_path: &str) -> Option<String> {
    let path = Path::new(manifest_path);
    if path.file_name()? != "plugin.json" {
        return None;
    }
    let claude_plugin_dir = path.parent()?;
    if claude_plugin_dir.file_name()? != ".claude-plugin" {
        return None;
    }
    let root = claude_plugin_dir.parent().unwrap_or_else(|| Path::new(""));
    if root.as_os_str().is_empty() {
        Some(".".to_string())
    } else {
        Some(root.to_string_lossy().replace('\\', "/"))
    }
}

fn write_synthetic_claude_plugin_package(root: &Path) -> Result<()> {
    let plugin_json_path = root.join(".claude-plugin").join("plugin.json");
    let plugin_json = fs::read_to_string(&plugin_json_path)
        .with_context(|| format!("failed to read {}", plugin_json_path.display()))?;
    let plugin = parse_claude_plugin_manifest(&plugin_json)?;
    let skills = discover_claude_plugin_skills(root)?;
    let commands = discover_claude_plugin_commands(root)?;
    let hooks = discover_claude_plugin_hooks(root)?;
    let mcp_servers = discover_claude_plugin_mcp_servers(root)?;
    let scripts = discover_claude_plugin_scripts(root)?;
    let unsupported = detect_unsupported_claude_plugin_components(top_level_component_paths(root)?);
    let resources = claude_plugin_resources(
        &plugin,
        &skills,
        &commands,
        &hooks,
        &mcp_servers,
        &unsupported,
        ".",
    )?;

    let mut modules = Vec::with_capacity(resources.len());
    for resource in resources {
        let path = resource.path;
        let output_path = root.join(&path);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let content = serde_yaml::to_string(&resource.manifest)
            .context("failed to serialize generated Claude plugin resource")?;
        fs::write(&output_path, content)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
        modules.push(PackageModule {
            path,
            metadata: serde_json::Value::Null,
        });
    }

    let package = ModulePackageManifest {
        schema: "nenjo.package.v1".to_string(),
        name: plugin.slug.clone(),
        version: plugin
            .version
            .clone()
            .unwrap_or_else(|| "0.1.0".to_string()),
        description: plugin.description.clone(),
        dependencies: Default::default(),
        modules,
        metadata: json!({
            "adapter": "claude_plugin",
            "claude": {
                "plugin": plugin.raw,
                "dependencies": plugin.dependencies,
                "components": {
                    "skills": skills,
                    "commands": commands,
                    "hooks": hooks,
                    "scripts": scripts,
                    "mcp_servers": mcp_servers,
                    "unsupported": unsupported
                },
                "unsupported_components": unsupported
            }
        }),
    };
    let content =
        serde_yaml::to_string(&package).context("failed to serialize Claude plugin package")?;
    fs::write(root.join("package.yaml"), content)
        .with_context(|| format!("failed to write {}", root.join("package.yaml").display()))?;
    Ok(())
}

fn discover_claude_plugin_skills(root: &Path) -> Result<Vec<nenjo_packages::ClaudePluginSkill>> {
    let root_skill_path = root.join("SKILL.md");
    let mut skills = Vec::new();
    if root_skill_path.is_file() {
        let content = fs::read_to_string(&root_skill_path)
            .with_context(|| format!("failed to read {}", root_skill_path.display()))?;
        skills.push(parse_claude_plugin_skill(&content, "SKILL.md")?);
    }

    let skills_dir = root.join("skills");
    if !skills_dir.is_dir() {
        skills.sort_by(|left, right| left.slug.cmp(&right.slug));
        return Ok(skills);
    }
    for entry in fs::read_dir(&skills_dir)
        .with_context(|| format!("failed to read {}", skills_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let skill_name = entry.file_name().to_string_lossy().to_string();
        let skill_path = entry.path().join("SKILL.md");
        if !skill_path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&skill_path)
            .with_context(|| format!("failed to read {}", skill_path.display()))?;
        let source_path = format!("skills/{skill_name}/SKILL.md");
        skills.push(parse_claude_plugin_skill(&content, &source_path)?);
    }
    skills.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(skills)
}

fn discover_claude_plugin_commands(root: &Path) -> Result<Vec<ClaudePluginCommand>> {
    let commands_dir = root.join("commands");
    if !commands_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut commands = Vec::new();
    for entry in fs::read_dir(&commands_dir)
        .with_context(|| format!("failed to read {}", commands_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let command_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("command.md")
            .to_string();
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let source_path = format!("commands/{command_name}");
        commands.push(parse_claude_plugin_command(&content, &source_path)?);
    }
    commands.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(commands)
}

fn discover_claude_plugin_hooks(root: &Path) -> Result<Vec<ClaudePluginHook>> {
    let path = root.join("hooks").join("hooks.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_claude_plugin_hooks(&content)?)
}

fn discover_claude_plugin_mcp_servers(
    root: &Path,
) -> Result<Vec<nenjo_packages::ClaudePluginMcpServer>> {
    let path = root.join(".mcp.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_claude_plugin_mcp_servers(&content)?)
}

fn discover_claude_plugin_scripts(root: &Path) -> Result<Vec<String>> {
    let scripts_dir = root.join("scripts");
    if !scripts_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut scripts = Vec::new();
    collect_relative_files(&scripts_dir, "scripts", &mut scripts)?;
    scripts.sort();
    Ok(scripts)
}

fn collect_relative_files(dir: &Path, relative_dir: &str, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        let relative_path = format!("{relative_dir}/{file_name}");
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_relative_files(&entry.path(), &relative_path, out)?;
        } else if file_type.is_file() {
            out.push(relative_path);
        }
    }
    Ok(())
}

fn top_level_component_paths(root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let mut path = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() {
            path.push('/');
        }
        paths.push(path);
    }
    Ok(paths)
}

fn copy_dir_all(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("failed to create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("failed to read {}", from.display()))? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&source, &target)?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::copy(&source, &target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo_packages::{PackageKind, ResourceManifest};

    fn git_source(url: &str) -> PackageSource {
        PackageSource::Git {
            url: url.to_string(),
            reference: "main".to_string(),
            manifest_path: "packages.yaml".to_string(),
        }
    }

    #[test]
    fn derives_scope_from_github_org() {
        assert_eq!(
            package_source_scope(&git_source("https://github.com/nenjo-ai/packages.git")),
            Some("@nenjo-ai".to_string())
        );
        assert_eq!(
            package_source_scope(&git_source("git@github.com:acme/packages.git")),
            Some("@acme".to_string())
        );
    }

    #[test]
    fn derives_scope_from_gitlab_namespace() {
        assert_eq!(
            package_source_scope(&git_source("https://gitlab.com/acme/packages.git")),
            Some("@acme".to_string())
        );
        assert_eq!(
            package_source_scope(&git_source("git@gitlab.com:acme/platform/packages.git")),
            Some("@acme".to_string())
        );
    }

    #[test]
    fn local_git_sources_do_not_have_scope() {
        assert_eq!(package_source_scope(&git_source("../packages")), None);
    }

    #[test]
    fn unsupported_git_hosts_do_not_have_scope() {
        assert_eq!(
            package_source_scope(&git_source("https://example.com/acme/packages.git")),
            None
        );
    }

    #[test]
    fn parses_fetch_modes() {
        assert_eq!(FetchMode::parse("git").unwrap(), FetchMode::Git);
        assert_eq!(FetchMode::parse("provider").unwrap(), FetchMode::Provider);
        assert_eq!(FetchMode::parse("raw").unwrap(), FetchMode::Provider);
        assert_eq!(FetchMode::parse("auto").unwrap(), FetchMode::Auto);
        assert!(FetchMode::parse("archive").is_err());
    }

    #[test]
    fn provider_mode_parses_github_sources() {
        let source = ProviderGitSource::parse(
            "https://github.com/nenjo-ai/packages.git",
            "main",
            "packages.yaml",
        )
        .unwrap();
        match source {
            ProviderGitSource::GitHub {
                owner,
                repo,
                reference,
            } => {
                assert_eq!(owner, "nenjo-ai");
                assert_eq!(repo, "packages");
                assert_eq!(reference, "main");
            }
            ProviderGitSource::GitLab { .. } => panic!("expected GitHub source"),
        }
    }

    #[test]
    fn provider_mode_parses_gitlab_sources() {
        let source = ProviderGitSource::parse(
            "git@gitlab.com:acme/platform/packages.git",
            "main",
            "packages.yaml",
        )
        .unwrap();
        match source {
            ProviderGitSource::GitLab {
                host,
                project_path,
                reference,
            } => {
                assert_eq!(host, "gitlab.com");
                assert_eq!(project_path, "acme/platform/packages");
                assert_eq!(reference, "main");
            }
            ProviderGitSource::GitHub { .. } => panic!("expected GitLab source"),
        }
    }

    #[test]
    fn provider_mode_rejects_unsupported_sources() {
        assert!(ProviderGitSource::parse("../packages", "main", "packages.yaml").is_none());
        assert!(
            ProviderGitSource::parse(
                "https://example.com/acme/packages.git",
                "main",
                "packages.yaml"
            )
            .is_none()
        );
        assert!(
            ProviderGitSource::parse(
                "https://github.com/acme/packages.git",
                "main",
                "../packages.yaml"
            )
            .is_none()
        );
    }

    #[test]
    fn provider_mode_fails_without_git_fallback_for_unsupported_sources() {
        let err = DefaultPackageSourceFetcher::with_fetch_mode(FetchMode::Provider)
            .fetch(&git_source("../packages"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("provider fetch mode does not support git source"),
            "{err}"
        );
    }

    #[test]
    fn auto_mode_falls_back_to_git_for_unsupported_sources() {
        let err = DefaultPackageSourceFetcher::with_fetch_mode(FetchMode::Auto)
            .fetch(&git_source("../missing-packages"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("failed to run git clone") || err.contains("git clone failed"),
            "{err}"
        );
    }

    #[test]
    fn encodes_provider_paths() {
        assert_eq!(
            encode_path("skills/my skill/SKILL.md"),
            "skills/my%20skill/SKILL.md"
        );
        assert_eq!(
            encode_gitlab_project_path("acme/platform/packages"),
            "acme%2Fplatform%2Fpackages"
        );
        assert_eq!(
            encode_gitlab_file_path("skills/my skill/SKILL.md"),
            "skills%2Fmy%20skill%2FSKILL.md"
        );
    }

    #[test]
    fn non_git_sources_do_not_have_scope() {
        assert_eq!(
            package_source_scope(&PackageSource::Artifact {
                url: "https://example.com/packages.tar.gz".to_string(),
                checksum: "abc".to_string(),
                manifest_path: "packages.yaml".to_string(),
            }),
            None
        );
    }

    #[test]
    fn synthetic_claude_plugin_package_supports_metadata_only_layout() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{
              "name": "Docs Only",
              "version": "0.2.0",
              "description": "Documentation-only plugin"
            }"#,
        );
        write_fixture_file(temp.path(), "README.md", "# Docs Only\n");

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "docs_only");
        assert_eq!(package.version, "0.2.0");
        assert_eq!(package.modules.len(), 1);
        assert_eq!(
            package.modules[0].path,
            ".nenjo/generated/claude-plugin/plugin.yaml"
        );
        assert!(
            package.metadata["claude"]["components"]["skills"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            package.metadata["claude"]["components"]["commands"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            package.metadata["claude"]["components"]["mcp_servers"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let plugin =
            read_resource_manifest(temp.path(), ".nenjo/generated/claude-plugin/plugin.yaml");
        assert_eq!(plugin.kind().unwrap(), PackageKind::Plugin);
        assert!(
            plugin.manifest["unsupported_components"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn synthetic_claude_plugin_package_supports_command_only_layout() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Command Pack","description":"Slash commands only"}"#,
        );
        write_fixture_file(
            temp.path(),
            "commands/help.md",
            r#"---
description: Show package-specific help.
argument-hint: TOPIC
---
Show help for the requested topic.
"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "command_pack");
        assert_eq!(package.modules.len(), 2);
        assert!(
            package
                .modules
                .iter()
                .any(|module| module.path == ".nenjo/generated/claude-plugin/commands/help.yaml")
        );

        let command = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/commands/help.yaml",
        );
        assert_eq!(command.kind().unwrap(), PackageKind::Command);
        assert_eq!(command.manifest["name"], "command_pack__help");
        assert_eq!(command.manifest["command"], "/help");
        assert_eq!(command.manifest["root_path"], "commands");
        assert_eq!(command.manifest["entry_path"], "help.md");
        assert!(command.manifest["hooks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn synthetic_claude_plugin_package_maps_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{
              "name": "Deploy Kit",
              "dependencies": [
                "audit-logger",
                { "name": "secrets-vault", "version": "~2.1.0" },
                { "name": "shared-kit", "marketplace": "acme-shared", "version": "^1.0" }
              ]
            }"#,
        );
        write_fixture_file(
            temp.path(),
            "commands/deploy.md",
            r#"---
description: Deploy safely.
---
Run the deploy workflow.
"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert!(package.dependencies.is_empty());
        assert_eq!(
            package.metadata["claude"]["dependencies"],
            json!([
                {
                    "kind": "same_marketplace",
                    "name": "audit-logger",
                    "version": null
                },
                {
                    "kind": "same_marketplace",
                    "name": "secrets-vault",
                    "version": "~2.1.0"
                },
                {
                    "kind": "cross_marketplace",
                    "name": "shared-kit",
                    "marketplace": "acme-shared",
                    "version": "^1.0"
                }
            ])
        );

        let plugin =
            read_resource_manifest(temp.path(), ".nenjo/generated/claude-plugin/plugin.yaml");
        assert_eq!(
            plugin.manifest["dependencies"],
            package.metadata["claude"]["dependencies"]
        );
    }

    #[test]
    fn synthetic_claude_plugin_package_discovers_root_skill_and_mcp() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Root Skill Tools","description":"Root skill plus MCP"}"#,
        );
        write_fixture_file(
            temp.path(),
            "SKILL.md",
            r#"---
name: Root Skill
description: Use the root skill.
---
Use `scripts/root.sh` for the workflow.
"#,
        );
        write_fixture_file(
            temp.path(),
            ".mcp.json",
            r#"{
              "mcpServers": {
                "review-server": {
                  "command": "node",
                  "args": ["servers/review.js"],
                  "env": {
                    "MODE": "local",
                    "TOKEN": "$TOKEN"
                  }
                }
              }
            }"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "root_skill_tools");
        assert_eq!(
            package
                .modules
                .iter()
                .map(|module| module.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                ".nenjo/generated/claude-plugin/plugin.yaml",
                ".nenjo/generated/claude-plugin/skills/root_skill.yaml",
                ".nenjo/generated/claude-plugin/mcp/review_server.yaml",
            ]
        );

        let skill = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/skills/root_skill.yaml",
        );
        assert_eq!(skill.kind().unwrap(), PackageKind::Skill);
        assert_eq!(skill.manifest["name"], "root_skill_tools__root_skill");
        assert_eq!(skill.manifest["root_path"], ".");
        assert_eq!(skill.manifest["entry_path"], "SKILL.md");

        let server = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/mcp/review_server.yaml",
        );
        assert_eq!(server.kind().unwrap(), PackageKind::McpServer);
        assert_eq!(server.manifest["name"], "root_skill_tools__review_server");
        assert_eq!(server.manifest["metadata"]["runtime"]["cwd_path"], ".");
        assert_eq!(
            server.manifest["metadata"]["runtime"]["env"]["MODE"],
            "local"
        );
        assert!(server.manifest["metadata"]["runtime"]["env"]["TOKEN"].is_null());
    }

    #[test]
    fn synthetic_claude_plugin_package_discovers_nested_skills_command_and_hooks() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Workflow Pack","description":"Skills, command, hooks"}"#,
        );
        write_fixture_file(
            temp.path(),
            "skills/review/SKILL.md",
            r#"---
name: Review
description: Review code.
---
Use scripts/review.sh.
"#,
        );
        write_fixture_file(
            temp.path(),
            "skills/deploy/SKILL.md",
            r#"---
name: Deploy
description: Deploy safely.
hooks:
  - PreToolUse deploy-pre
---
Use scripts/deploy.sh.
"#,
        );
        write_fixture_file(
            temp.path(),
            "commands/runbook.md",
            r#"---
description: Run the operational runbook.
---
Follow the runbook.
"#,
        );
        write_fixture_file(
            temp.path(),
            "hooks/hooks.json",
            r#"{
              "hooks": {
                "PreToolUse": [
                  {
                    "matcher": "shell",
                    "hooks": [
                      { "type": "command", "command": "scripts/deploy-pre.sh" }
                    ]
                  }
                ],
                "Stop": [
                  {
                    "matcher": "*",
                    "hooks": [
                      { "type": "command", "command": "scripts/runbook-stop.sh" }
                    ]
                  }
                ]
              }
            }"#,
        );

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        assert_eq!(package.name, "workflow_pack");
        assert_eq!(package.modules.len(), 6);

        let deploy = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/skills/deploy.yaml",
        );
        assert_eq!(deploy.kind().unwrap(), PackageKind::Skill);
        assert_eq!(
            deploy.manifest["hooks"],
            json!(["workflow_pack__pretooluse_deploy_pre"])
        );

        let command = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/commands/runbook.yaml",
        );
        assert_eq!(command.kind().unwrap(), PackageKind::Command);
        assert_eq!(
            command.manifest["hooks"],
            json!([
                "workflow_pack__pretooluse_deploy_pre",
                "workflow_pack__stop_runbook_stop"
            ])
        );

        let pre_hook = read_resource_manifest(
            temp.path(),
            ".nenjo/generated/claude-plugin/hooks/pretooluse_deploy_pre.yaml",
        );
        assert_eq!(pre_hook.kind().unwrap(), PackageKind::Hook);
        assert_eq!(pre_hook.manifest["event"], "PreToolUse");
        assert_eq!(pre_hook.manifest["matcher"], "shell");
    }

    #[test]
    fn synthetic_claude_plugin_package_records_unsupported_layout_components() {
        let temp = tempfile::tempdir().unwrap();
        write_fixture_file(
            temp.path(),
            ".claude-plugin/plugin.json",
            r#"{"name":"Unsupported Pack","description":"Unsupported components"}"#,
        );
        write_fixture_file(temp.path(), "README.md", "# Unsupported Pack\n");
        write_fixture_file(temp.path(), "agents/reviewer.md", "# Reviewer agent\n");
        write_fixture_file(temp.path(), "lsp/server.json", "{}\n");

        write_synthetic_claude_plugin_package(temp.path()).unwrap();

        let package = read_package_manifest(temp.path());
        let unsupported = package.metadata["claude"]["unsupported_components"]
            .as_array()
            .unwrap();
        assert_eq!(unsupported.len(), 2);
        assert!(unsupported.iter().any(|item| item["kind"] == "agents"));
        assert!(unsupported.iter().any(|item| item["kind"] == "lsp"));

        let plugin =
            read_resource_manifest(temp.path(), ".nenjo/generated/claude-plugin/plugin.yaml");
        let unsupported = plugin.manifest["unsupported_components"]
            .as_array()
            .unwrap();
        assert_eq!(unsupported.len(), 2);
        assert!(unsupported.iter().any(|item| item["kind"] == "agents"));
        assert!(unsupported.iter().any(|item| item["kind"] == "lsp"));
    }

    fn write_fixture_file(root: &Path, relative_path: &str, content: &str) {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn read_package_manifest(root: &Path) -> ModulePackageManifest {
        let content = fs::read_to_string(root.join("package.yaml")).unwrap();
        serde_yaml::from_str(&content).unwrap()
    }

    fn read_resource_manifest(root: &Path, relative_path: &str) -> ResourceManifest {
        let content = fs::read_to_string(root.join(relative_path)).unwrap();
        serde_yaml::from_str(&content).unwrap()
    }
}
