use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nenjo_content::ArtifactRef;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const MAX_ARTIFACT_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ARTIFACT_CACHE_ENTRIES: usize = 4_096;

#[derive(Debug, Clone)]
pub(crate) struct ArtifactCache {
    root: PathBuf,
}

impl ArtifactCache {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(crate) async fn read(
        &self,
        org_id: Uuid,
        artifact: &ArtifactRef,
    ) -> Result<Option<Vec<u8>>> {
        let cache = self.clone();
        let artifact = artifact.clone();
        tokio::task::spawn_blocking(move || cache.read_blocking(org_id, &artifact))
            .await
            .context("artifact cache read task failed")?
    }

    pub(crate) async fn write(
        &self,
        org_id: Uuid,
        artifact: &ArtifactRef,
        bytes: &[u8],
    ) -> Result<()> {
        let cache = self.clone();
        let artifact = artifact.clone();
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || cache.write_blocking(org_id, &artifact, &bytes))
            .await
            .context("artifact cache write task failed")?
    }

    fn read_blocking(&self, org_id: Uuid, artifact: &ArtifactRef) -> Result<Option<Vec<u8>>> {
        let path = self.content_path(org_id, artifact);
        verify_cache_path(&self.root, &path)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("failed to inspect artifact cache entry"),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("artifact cache entry is not a regular file");
        }
        let bytes = fs::read(&path).context("failed to read artifact cache entry")?;
        if metadata.len() != artifact.size().bytes() || digest(&bytes) != artifact.digest().as_str()
        {
            fs::remove_file(&path).context("failed to remove invalid artifact cache entry")?;
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    fn write_blocking(&self, org_id: Uuid, artifact: &ArtifactRef, bytes: &[u8]) -> Result<()> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.size().bytes() {
            bail!("artifact cache write size does not match its immutable reference");
        }
        if digest(bytes) != artifact.digest().as_str() {
            bail!("artifact cache write digest does not match its immutable reference");
        }
        let path = self.content_path(org_id, artifact);
        let parent = path.parent().context("artifact cache path has no parent")?;
        create_secure_cache_directory_chain(&self.root, parent)?;
        let temporary = parent.join(format!(".content-{}.tmp", Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .context("failed to create artifact cache entry")?;
        file.write_all(bytes)
            .context("failed to write artifact cache entry")?;
        file.sync_all()
            .context("failed to sync artifact cache entry")?;
        drop(file);
        match fs::rename(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary)
                    .context("failed to discard duplicate artifact cache entry")?;
            }
            Err(error) => return Err(error).context("failed to publish artifact cache entry"),
        }
        self.evict_oldest_entries(&path)
    }

    pub(crate) fn content_path(&self, org_id: Uuid, artifact: &ArtifactRef) -> PathBuf {
        self.root
            .join(org_id.simple().to_string())
            .join(artifact.id().as_uuid().simple().to_string())
            .join(artifact.digest().hex())
            .join("content")
    }

    fn evict_oldest_entries(&self, protected: &Path) -> Result<()> {
        let mut entries = Vec::new();
        collect_cache_entries(&self.root, &mut entries)?;
        let mut total = entries
            .iter()
            .fold(0_u64, |size, entry| size.saturating_add(entry.size));
        let mut entry_count = entries.len();
        if !cache_is_over_limit(total, entry_count) {
            return Ok(());
        }
        entries.sort_by_key(|entry| entry.last_accessed);
        for entry in entries {
            if !cache_is_over_limit(total, entry_count) {
                break;
            }
            if entry.path == protected {
                continue;
            }
            match fs::remove_file(&entry.path) {
                Ok(()) => {
                    total = total.saturating_sub(entry.size);
                    entry_count = entry_count.saturating_sub(1);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    entry_count = entry_count.saturating_sub(1);
                }
                Err(error) => return Err(error).context("failed to evict artifact cache entry"),
            }
        }
        Ok(())
    }
}

fn cache_is_over_limit(total_bytes: u64, entry_count: usize) -> bool {
    total_bytes > MAX_ARTIFACT_CACHE_BYTES || entry_count > MAX_ARTIFACT_CACHE_ENTRIES
}

struct CachedFile {
    path: PathBuf,
    size: u64,
    last_accessed: std::time::SystemTime,
}

fn collect_cache_entries(root: &Path, entries: &mut Vec<CachedFile>) -> Result<()> {
    let directory = match fs::read_dir(root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to scan artifact cache"),
    };
    for entry in directory {
        let entry = entry.context("failed to inspect artifact cache directory")?;
        let metadata = fs::symlink_metadata(entry.path())
            .context("failed to inspect artifact cache metadata")?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_cache_entries(&entry.path(), entries)?;
        } else if metadata.is_file() && entry.file_name() == "content" {
            entries.push(CachedFile {
                path: entry.path(),
                size: metadata.len(),
                last_accessed: metadata
                    .accessed()
                    .or_else(|_| metadata.modified())
                    .unwrap_or(std::time::UNIX_EPOCH),
            });
        }
    }
    Ok(())
}

fn set_private_directory_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .context("failed to secure artifact cache directory")?;
    }
    Ok(())
}

fn create_secure_cache_directory_chain(root: &Path, leaf: &Path) -> Result<()> {
    let relative = leaf
        .strip_prefix(root)
        .context("artifact cache path escaped its configured root")?;
    let mut directory = root.to_path_buf();
    create_secure_cache_directory(&directory)?;
    for component in relative.components() {
        directory.push(component);
        create_secure_cache_directory(&directory)?;
    }
    Ok(())
}

fn create_secure_cache_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to create artifact cache directory '{}'",
                    path.display()
                )
            });
        }
    }
    verify_cache_directory(path)
}

fn verify_cache_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "failed to inspect artifact cache directory '{}'",
            path.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "artifact cache directory '{}' is not a real directory",
            path.display()
        );
    }
    set_private_directory_permissions(path)
}

fn verify_cache_path(root: &Path, content: &Path) -> Result<()> {
    let leaf = content
        .parent()
        .context("artifact cache content path has no parent")?;
    let relative = leaf
        .strip_prefix(root)
        .context("artifact cache path escaped its configured root")?;
    let mut directory = root.to_path_buf();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            directory.push(component);
        }
        match fs::symlink_metadata(&directory) {
            Ok(_) => verify_cache_directory(&directory)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect artifact cache directory '{}'",
                        directory.display()
                    )
                });
            }
        }
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use nenjo_content::{ArtifactId, ArtifactSize, MediaType, Sha256Digest};

    use super::*;

    fn artifact(bytes: &[u8]) -> ArtifactRef {
        ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&digest(bytes)).unwrap(),
            MediaType::parse("text/plain").unwrap(),
            ArtifactSize::new(u64::try_from(bytes.len()).unwrap()),
        )
    }

    #[tokio::test]
    async fn plaintext_survives_new_cache_instances() {
        let root = tempfile::tempdir().unwrap();
        let org_id = Uuid::new_v4();
        let bytes = b"first\nsecond\n";
        let artifact = artifact(bytes);

        ArtifactCache::new(root.path())
            .write(org_id, &artifact, bytes)
            .await
            .unwrap();
        let cached = ArtifactCache::new(root.path())
            .read(org_id, &artifact)
            .await
            .unwrap();

        assert_eq!(cached.as_deref(), Some(bytes.as_slice()));
    }

    #[tokio::test]
    async fn corrupt_plaintext_entry_is_discarded() {
        let root = tempfile::tempdir().unwrap();
        let org_id = Uuid::new_v4();
        let bytes = b"expected";
        let artifact = artifact(bytes);
        let cache = ArtifactCache::new(root.path());
        cache.write(org_id, &artifact, bytes).await.unwrap();
        let path = cache.content_path(org_id, &artifact);
        fs::write(&path, b"corrupt!").unwrap();

        let cached = cache.read(org_id, &artifact).await.unwrap();

        assert!(cached.is_none());
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_symlinked_cache_directories() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let org_id = Uuid::new_v4();
        let artifact = artifact(b"secret");
        symlink(
            outside.path(),
            root.path().join(org_id.simple().to_string()),
        )
        .unwrap();

        let error = ArtifactCache::new(root.path())
            .write(org_id, &artifact, b"secret")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("not a real directory"));
        assert!(!outside.path().join(artifact.id().to_string()).exists());
    }

    #[test]
    fn cache_limits_bound_bytes_and_zero_length_entry_growth() {
        assert!(!cache_is_over_limit(
            MAX_ARTIFACT_CACHE_BYTES,
            MAX_ARTIFACT_CACHE_ENTRIES
        ));
        assert!(cache_is_over_limit(MAX_ARTIFACT_CACHE_BYTES + 1, 1));
        assert!(cache_is_over_limit(0, MAX_ARTIFACT_CACHE_ENTRIES + 1));
    }
}
