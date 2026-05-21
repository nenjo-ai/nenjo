use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Result;
use anyhow::Context;
use flate2::read::GzDecoder;
use nenjo_packages::sha256_hex;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

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
#[derive(Debug, Clone, Default)]
pub struct DefaultPackageSourceFetcher;

impl DefaultPackageSourceFetcher {
    /// Create the default source fetcher.
    pub fn new() -> Self {
        Self
    }
}

impl PackageSourceFetcher for DefaultPackageSourceFetcher {
    fn fetch(&self, source: &PackageSource) -> Result<FetchedPackageSource> {
        match source {
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
            } => fetch_git_source(url, reference, manifest_path),
            PackageSource::Artifact {
                url,
                checksum,
                manifest_path,
            } => fetch_artifact_source(url, checksum, manifest_path),
            PackageSource::Remote { url, checksum } => fetch_remote_manifest(url, checksum),
        }
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
        PackageSource::Git { url, .. } => github_org_from_url(url).map(github_org_to_scope),
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

fn github_org_to_scope(org: String) -> String {
    format!("@{org}")
}

fn github_org_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let path = if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else {
        return None;
    };
    let org = path.split('/').next()?.trim();
    if org.is_empty() || org == "." || org == ".." {
        return None;
    }
    Some(org.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn non_github_git_sources_do_not_have_scope() {
        assert_eq!(package_source_scope(&git_source("../packages")), None);
        assert_eq!(
            package_source_scope(&git_source("https://gitlab.com/acme/packages.git")),
            None
        );
    }
}
