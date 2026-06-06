//! Shared update checks and bundle installation for Nenjo command-line tools.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Cursor;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::TempDir;
use thiserror::Error;

pub const REPOSITORY: &str = "nenjo-ai/nenjo";
pub const BUNDLE_BINARIES: &[&str] = &["nenjo", "nenpm", "nenjoup"];

const CHECK_CACHE_FILE: &str = "update-check.json";
const DEFAULT_UPDATE_CHECK_TTL: Duration = Duration::from_secs(60 * 60 * 24);
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    UseCache,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    pub update_command: String,
}

impl UpdateNotice {
    pub fn render(&self) -> String {
        format!(
            "Nenjo v{} is available. You have v{}.\nRun `{}` to update the installed tools.",
            self.latest_version, self.current_version, self.update_command
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    pub version: Option<String>,
    pub install_dir: Option<PathBuf>,
}

impl UpdateOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn install_dir(mut self, install_dir: impl Into<PathBuf>) -> Self {
        self.install_dir = Some(install_dir.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    pub version_tag: String,
    pub target: String,
    pub install_dir: PathBuf,
    pub installed_binaries: Vec<String>,
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("unsupported platform target: {0}")]
    UnsupportedTarget(String),

    #[error("could not determine the Nenjo home directory")]
    HomeDirectoryUnavailable,

    #[error("invalid version '{value}'")]
    InvalidVersion {
        value: String,
        #[source]
        source: semver::Error,
    },

    #[error("failed to make HTTP request")]
    Http(#[from] reqwest::Error),

    #[error("failed to read or write {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse JSON")]
    Json(#[from] serde_json::Error),

    #[error("release {tag} did not include a usable version")]
    MissingReleaseVersion { tag: String },

    #[error("release checksum for {artifact_name} was empty")]
    EmptyChecksum { artifact_name: String },

    #[error("checksum mismatch for {artifact_name}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        artifact_name: String,
        expected: String,
        actual: String,
    },

    #[error("release archive contains an unsafe path: {0}")]
    UnsafeArchivePath(PathBuf),

    #[error("release archive is missing {0}")]
    MissingBinary(String),

    #[error("{binary} --version failed with status {status}")]
    SmokeCheckFailed { binary: String, status: String },

    #[error("failed to execute {binary}")]
    CommandIo {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not find bundled nenjoup; reinstall Nenjo or run nenjoup directly")]
    UpdaterNotFound,
}

#[derive(Debug, Clone)]
struct LatestRelease {
    tag: String,
    version: Version,
    release_url: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct UpdateCheckCache {
    checked_at_unix: u64,
    target: String,
    latest_tag: String,
    latest_version: String,
    release_url: String,
}

pub fn maybe_update_notice(current_version: &str, update_command: &str) -> Option<UpdateNotice> {
    if cfg!(debug_assertions) || update_checks_disabled() || find_nenjoup().is_none() {
        return None;
    }

    check_for_update(current_version, update_command, CacheMode::UseCache)
        .ok()
        .flatten()
}

pub fn check_for_update(
    current_version: &str,
    update_command: &str,
    cache_mode: CacheMode,
) -> Result<Option<UpdateNotice>, UpdateError> {
    let target = detect_target()?;
    let latest = match cache_mode {
        CacheMode::UseCache => match read_fresh_cache(&target, DEFAULT_UPDATE_CHECK_TTL)? {
            Some(latest) => Some(latest),
            None => fetch_latest_release_and_cache(&target).ok(),
        },
        CacheMode::Refresh => Some(fetch_latest_release_and_cache(&target)?),
    };

    let Some(latest) = latest else {
        return Ok(None);
    };

    notice_for_latest(current_version, update_command, &latest)
}

pub fn update_bundle(options: UpdateOptions) -> Result<UpdateReport, UpdateError> {
    let target = detect_target()?;
    let UpdateOptions {
        version,
        install_dir,
    } = options;
    let should_cache_release = version.is_none();
    let install_dir = install_dir.unwrap_or_else(default_install_dir_for_current_exe);
    let release = resolve_update_release(version.as_deref(), &target)?;
    let artifact_name = artifact_name(&target);
    let archive_url = release_asset_url(&release.tag, &artifact_name);
    let checksum_url = format!("{archive_url}.sha256");

    let archive_bytes = download_bytes(&archive_url)?;
    let checksum_text = String::from_utf8_lossy(&download_bytes(&checksum_url)?).into_owned();
    verify_checksum(&artifact_name, &archive_bytes, &checksum_text)?;

    let temp_dir = TempDir::new().map_err(|source| UpdateError::Io {
        path: env::temp_dir(),
        source,
    })?;
    unpack_archive(&archive_bytes, temp_dir.path())?;
    verify_extracted_bundle(temp_dir.path())?;

    fs::create_dir_all(&install_dir).map_err(|source| UpdateError::Io {
        path: install_dir.clone(),
        source,
    })?;

    let mut installed_binaries = Vec::with_capacity(BUNDLE_BINARIES.len());
    for binary in BUNDLE_BINARIES {
        install_binary(
            temp_dir.path().join(binary_file_name(binary)),
            &install_dir,
            binary,
        )?;
        installed_binaries.push((*binary).to_string());
    }

    if should_cache_release {
        let _ = write_cache(&target, &release);
    }

    Ok(UpdateReport {
        version_tag: release.tag,
        target,
        install_dir,
        installed_binaries,
    })
}

pub fn run_nenjoup_update(version: Option<&str>) -> Result<ExitStatus, UpdateError> {
    let mut args = vec!["update".to_string()];
    if let Some(version) = version {
        args.push("--version".to_string());
        args.push(version.to_string());
    }
    run_nenjoup(&args)
}

pub fn run_nenjoup(args: &[String]) -> Result<ExitStatus, UpdateError> {
    let updater = find_nenjoup().ok_or(UpdateError::UpdaterNotFound)?;
    Command::new(&updater)
        .args(args)
        .status()
        .map_err(|source| UpdateError::CommandIo {
            binary: updater,
            source,
        })
}

pub fn find_nenjoup() -> Option<PathBuf> {
    let updater_name = binary_file_name("nenjoup");
    if let Ok(current_exe) = env::current_exe() {
        if current_exe
            .file_name()
            .is_some_and(|name| name == OsStr::new(&updater_name))
        {
            return Some(current_exe);
        }
        if let Some(parent) = current_exe.parent() {
            let sibling = parent.join(&updater_name);
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }

    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join(&updater_name))
            .find(|path| path.is_file())
    })
}

pub fn detect_target() -> Result<String, UpdateError> {
    let arch = match env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(UpdateError::UnsupportedTarget(other.to_string())),
    };

    let os = match env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        other => return Err(UpdateError::UnsupportedTarget(format!("{arch}-{other}"))),
    };

    let target = format!("{arch}-{os}");
    match target.as_str() {
        "x86_64-unknown-linux-gnu" | "aarch64-apple-darwin" => Ok(target),
        _ => Err(UpdateError::UnsupportedTarget(target)),
    }
}

fn resolve_update_release(
    requested_version: Option<&str>,
    target: &str,
) -> Result<LatestRelease, UpdateError> {
    if let Some(version) = requested_version {
        let tag = normalize_tag(version);
        let parsed = parse_release_version(&tag)?;
        return Ok(LatestRelease {
            release_url: format!("https://github.com/{REPOSITORY}/releases/tag/{tag}"),
            tag,
            version: parsed,
        });
    }

    fetch_latest_release_and_cache(target)
}

fn notice_for_latest(
    current_version: &str,
    update_command: &str,
    latest: &LatestRelease,
) -> Result<Option<UpdateNotice>, UpdateError> {
    let current = parse_release_version(current_version)?;
    if latest.version <= current {
        return Ok(None);
    }

    Ok(Some(UpdateNotice {
        current_version: current.to_string(),
        latest_version: latest.version.to_string(),
        release_url: latest.release_url.clone(),
        update_command: update_command.to_string(),
    }))
}

fn fetch_latest_release_and_cache(target: &str) -> Result<LatestRelease, UpdateError> {
    let latest = fetch_latest_release()?;
    let _ = write_cache(target, &latest);
    Ok(latest)
}

fn fetch_latest_release() -> Result<LatestRelease, UpdateError> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let body = http_client().get(url).send()?.error_for_status()?.text()?;
    let release: GitHubRelease = serde_json::from_str(&body)?;
    let version = parse_release_version(&release.tag_name)?;
    let release_url = release.html_url.unwrap_or_else(|| {
        format!(
            "https://github.com/{REPOSITORY}/releases/tag/{}",
            release.tag_name
        )
    });

    Ok(LatestRelease {
        tag: release.tag_name,
        version,
        release_url,
    })
}

fn download_bytes(url: &str) -> Result<Vec<u8>, UpdateError> {
    Ok(http_client()
        .get(url)
        .send()?
        .error_for_status()?
        .bytes()?
        .to_vec())
}

fn http_client() -> Client {
    Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("nenjo-updater/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("valid reqwest client")
}

fn read_fresh_cache(target: &str, max_age: Duration) -> Result<Option<LatestRelease>, UpdateError> {
    let path = cache_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let text = fs::read_to_string(&path).map_err(|source| UpdateError::Io {
        path: path.clone(),
        source,
    })?;
    let cache: UpdateCheckCache = match serde_json::from_str(&text) {
        Ok(cache) => cache,
        Err(_) => return Ok(None),
    };

    if cache.target != target {
        return Ok(None);
    }

    let now = unix_timestamp();
    let age = now.saturating_sub(cache.checked_at_unix);
    if age > max_age.as_secs() {
        return Ok(None);
    }

    Ok(Some(LatestRelease {
        tag: cache.latest_tag,
        version: parse_release_version(&cache.latest_version)?,
        release_url: cache.release_url,
    }))
}

fn write_cache(target: &str, latest: &LatestRelease) -> Result<(), UpdateError> {
    let path = cache_path()?;
    let parent = path.parent().ok_or(UpdateError::HomeDirectoryUnavailable)?;
    fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let cache = UpdateCheckCache {
        checked_at_unix: unix_timestamp(),
        target: target.to_string(),
        latest_tag: latest.tag.clone(),
        latest_version: latest.version.to_string(),
        release_url: latest.release_url.clone(),
    };
    let text = serde_json::to_string_pretty(&cache)?;
    fs::write(&path, text).map_err(|source| UpdateError::Io { path, source })
}

fn cache_path() -> Result<PathBuf, UpdateError> {
    Ok(nenjo_home_dir()?.join(CHECK_CACHE_FILE))
}

fn nenjo_home_dir() -> Result<PathBuf, UpdateError> {
    if let Some(path) = env::var_os("NENJO_HOME") {
        return Ok(PathBuf::from(path));
    }

    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".nenjo"))
        .ok_or(UpdateError::HomeDirectoryUnavailable)
}

fn default_install_dir_for_current_exe() -> PathBuf {
    if let Some(path) = env::var_os("NENJO_INSTALL_DIR") {
        return PathBuf::from(path);
    }

    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| {
            nenjo_home_dir()
                .map(|home| home.join("bin"))
                .unwrap_or_else(|_| PathBuf::from("."))
        })
}

fn unpack_archive(bytes: &[u8], destination: &Path) -> Result<(), UpdateError> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    for entry in archive.entries().map_err(|source| UpdateError::Io {
        path: destination.to_path_buf(),
        source,
    })? {
        let mut entry = entry.map_err(|source| UpdateError::Io {
            path: destination.to_path_buf(),
            source,
        })?;
        let path = entry
            .path()
            .map_err(|source| UpdateError::Io {
                path: destination.to_path_buf(),
                source,
            })?
            .into_owned();
        if !is_safe_archive_path(&path) {
            return Err(UpdateError::UnsafeArchivePath(path));
        }
        let output = destination.join(&path);
        entry.unpack(&output).map_err(|source| UpdateError::Io {
            path: output,
            source,
        })?;
    }
    Ok(())
}

fn is_safe_archive_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn verify_extracted_bundle(root: &Path) -> Result<(), UpdateError> {
    for binary in BUNDLE_BINARIES {
        let path = root.join(binary_file_name(binary));
        if !path.is_file() {
            return Err(UpdateError::MissingBinary((*binary).to_string()));
        }
        make_executable(&path)?;
        smoke_check(binary, &path)?;
    }
    Ok(())
}

fn smoke_check(binary: &str, path: &Path) -> Result<(), UpdateError> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|source| UpdateError::CommandIo {
            binary: path.to_path_buf(),
            source,
        })?;

    if output.status.success() {
        Ok(())
    } else {
        Err(UpdateError::SmokeCheckFailed {
            binary: binary.to_string(),
            status: output.status.to_string(),
        })
    }
}

fn install_binary(source: PathBuf, install_dir: &Path, binary: &str) -> Result<(), UpdateError> {
    let destination = install_dir.join(binary_file_name(binary));
    let temporary = install_dir.join(format!("{}.new", binary_file_name(binary)));

    fs::copy(&source, &temporary).map_err(|source| UpdateError::Io {
        path: temporary.clone(),
        source,
    })?;
    make_executable(&temporary)?;
    fs::rename(&temporary, &destination).map_err(|source| UpdateError::Io {
        path: destination,
        source,
    })
}

fn make_executable(path: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        let metadata = fs::metadata(path).map_err(|source| UpdateError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        fs::set_permissions(path, permissions).map_err(|source| UpdateError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn verify_checksum(
    artifact_name: &str,
    archive_bytes: &[u8],
    checksum_text: &str,
) -> Result<(), UpdateError> {
    let expected = checksum_text
        .split_whitespace()
        .next()
        .ok_or_else(|| UpdateError::EmptyChecksum {
            artifact_name: artifact_name.to_string(),
        })?
        .to_ascii_lowercase();
    let actual = sha256_hex(archive_bytes);

    if expected == actual {
        Ok(())
    } else {
        Err(UpdateError::ChecksumMismatch {
            artifact_name: artifact_name.to_string(),
            expected,
            actual,
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    format!("{digest:x}")
}

fn release_asset_url(tag: &str, artifact_name: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{artifact_name}")
}

fn artifact_name(target: &str) -> String {
    format!("nenjo-{target}.tar.gz")
}

fn binary_file_name(binary: &str) -> String {
    format!("{binary}{}", env::consts::EXE_SUFFIX)
}

fn normalize_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    }
}

fn parse_release_version(value: &str) -> Result<Version, UpdateError> {
    let trimmed = value.trim().trim_start_matches('v');
    if trimmed.is_empty() {
        return Err(UpdateError::MissingReleaseVersion {
            tag: value.to_string(),
        });
    }
    Version::parse(trimmed).map_err(|source| UpdateError::InvalidVersion {
        value: value.to_string(),
        source,
    })
}

fn update_checks_disabled() -> bool {
    env::var("NENJO_NO_UPDATE_CHECK")
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_is_returned_for_newer_release() {
        let latest = LatestRelease {
            tag: "v0.13.0".into(),
            version: Version::parse("0.13.0").unwrap(),
            release_url: "https://example.test/release".into(),
        };

        let notice = notice_for_latest("0.12.0", "nenjo update", &latest)
            .unwrap()
            .unwrap();

        assert_eq!(notice.latest_version, "0.13.0");
        assert_eq!(notice.current_version, "0.12.0");
        assert!(notice.render().contains("Run `nenjo update`"));
    }

    #[test]
    fn notice_is_skipped_for_current_release() {
        let latest = LatestRelease {
            tag: "v0.12.0".into(),
            version: Version::parse("0.12.0").unwrap(),
            release_url: "https://example.test/release".into(),
        };

        let notice = notice_for_latest("0.12.0", "nenjo update", &latest).unwrap();

        assert_eq!(notice, None);
    }

    #[test]
    fn unsafe_archive_paths_are_rejected() {
        assert!(is_safe_archive_path(Path::new("nenjo")));
        assert!(is_safe_archive_path(Path::new("bin/nenjo")));
        assert!(!is_safe_archive_path(Path::new("../nenjo")));
        assert!(!is_safe_archive_path(Path::new("/tmp/nenjo")));
    }
}
