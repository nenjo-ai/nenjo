use std::ffi::OsString;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use anyhow::{Context, Result, bail};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, OpenOptions};
use dashmap::DashMap;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

pub(crate) const MAX_FILE_MUTATION_BYTES: usize = 10 * 1024 * 1024;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A normalized path relative to a tool's scoped workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspacePath(PathBuf);

impl WorkspacePath {
    pub(crate) fn parse(path: &str) -> Result<Self> {
        let mut normalized = PathBuf::new();
        for component in Path::new(path).components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    bail!("path must remain relative to the scoped workspace")
                }
            }
        }
        if normalized.as_os_str().is_empty() {
            bail!("path must identify an item inside the scoped workspace");
        }
        Ok(Self(normalized))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn mutation_key(&self, workspace_root: &Path) -> PathBuf {
        workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf())
            .join(&self.0)
    }
}

/// Coordinates mutations of equal paths and ancestor/descendant paths.
#[derive(Debug, Default)]
pub(crate) struct FileMutationCoordinator {
    locks: DashMap<PathBuf, Weak<RwLock<()>>>,
}

/// Holds shared ancestor locks and an exclusive target lock for one mutation.
pub(crate) struct FileMutationGuard {
    _ancestors: Vec<OwnedRwLockReadGuard<()>>,
    _target: OwnedRwLockWriteGuard<()>,
}

impl FileMutationCoordinator {
    pub(crate) async fn lock(&self, path: &Path) -> FileMutationGuard {
        self.locks.retain(|_, lock| lock.strong_count() > 0);

        let mut ancestors = path.ancestors().skip(1).collect::<Vec<_>>();
        ancestors.reverse();
        let mut ancestor_guards = Vec::with_capacity(ancestors.len());
        for ancestor in ancestors {
            ancestor_guards.push(self.path_lock(ancestor).read_owned().await);
        }
        let target = self.path_lock(path).write_owned().await;

        FileMutationGuard {
            _ancestors: ancestor_guards,
            _target: target,
        }
    }

    fn path_lock(&self, path: &Path) -> Arc<RwLock<()>> {
        match self.locks.entry(path.to_path_buf()) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                if let Some(lock) = entry.get().upgrade() {
                    lock
                } else {
                    let lock = Arc::new(RwLock::new(()));
                    entry.insert(Arc::downgrade(&lock));
                    lock
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let lock = Arc::new(RwLock::new(()));
                entry.insert(Arc::downgrade(&lock));
                lock
            }
        }
    }
}

pub(crate) struct WorkspaceParent {
    pub(crate) dir: Dir,
    pub(crate) file_name: OsString,
}

/// Open a target's parent without granting path resolution outside `workspace_root`.
pub(crate) fn open_workspace_parent(
    workspace_root: &Path,
    target: &WorkspacePath,
    create_parent: bool,
) -> Result<WorkspaceParent> {
    let workspace = Dir::open_ambient_dir(workspace_root, ambient_authority())
        .with_context(|| format!("failed to open workspace {}", workspace_root.display()))?;
    let relative = target.as_path();
    let parent = relative
        .parent()
        .context("workspace path has no parent directory")?;
    let file_name = relative
        .file_name()
        .context("workspace path has no file name")?
        .to_os_string();

    if create_parent {
        workspace
            .create_dir_all(parent)
            .context("failed to create destination parent directories")?;
    }
    let dir = if parent.as_os_str().is_empty() {
        workspace.try_clone()
    } else {
        workspace.open_dir(parent)
    }
    .context("failed to open destination parent directory; resolved path escapes workspace or traverses a symlink")?;
    Ok(WorkspaceParent { dir, file_name })
}

struct TemporaryFile {
    parent: Dir,
    path: PathBuf,
    file: Option<File>,
    committed: bool,
}

impl TemporaryFile {
    fn new(parent: Dir, path: PathBuf, file: File) -> Self {
        Self {
            parent,
            path,
            file: Some(file),
            committed: false,
        }
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("temporary file is open")
    }

    fn close(&mut self) {
        self.file.take();
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        self.close();
        if !self.committed {
            let _ = self.parent.remove_file(&self.path);
        }
    }
}

/// Replace a file through an already-open parent directory.
pub(crate) fn atomic_replace_file_at(
    parent: &Dir,
    target_name: &Path,
    contents: &[u8],
) -> Result<()> {
    let file_name = target_name
        .file_name()
        .and_then(|name| name.to_str())
        .context("file replacement target has no valid file name")?;
    let existing_permissions = match parent.symlink_metadata(target_name) {
        Ok(metadata) if metadata.is_symlink() => bail!("refusing to replace a symlink"),
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("failed to inspect replacement target"),
    };

    let mut opened = None;
    for _ in 0..16 {
        let nonce = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = PathBuf::from(format!(
            ".{file_name}.nenjo-{}-{nonce}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        match parent.open_with(&temp_path, &options) {
            Ok(file) => {
                opened = Some(TemporaryFile::new(parent.try_clone()?, temp_path, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("failed to create replacement file"),
        }
    }

    let Some(mut temporary) = opened else {
        bail!("failed to allocate a unique replacement file");
    };
    if let Some(permissions) = existing_permissions {
        temporary
            .file_mut()
            .set_permissions(permissions)
            .context("failed to preserve destination permissions")?;
    }
    temporary
        .file_mut()
        .write_all(contents)
        .context("failed to write replacement file")?;
    temporary
        .file_mut()
        .flush()
        .context("failed to flush replacement file")?;
    temporary
        .file_mut()
        .sync_all()
        .context("failed to sync replacement file")?;
    temporary.close();

    parent
        .rename(&temporary.path, parent, target_name)
        .context("failed to atomically replace destination")?;
    temporary.committed = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn coordinator_serializes_the_same_resolved_path() {
        let coordinator = Arc::new(FileMutationCoordinator::default());
        let path = PathBuf::from("/workspace/example.txt");
        let first = coordinator.lock(&path).await;
        let waiting = tokio::spawn({
            let coordinator = coordinator.clone();
            let path = path.clone();
            async move { coordinator.lock(&path).await }
        });

        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(first);
        drop(waiting.await.expect("lock task"));
    }

    #[tokio::test]
    async fn coordinator_blocks_descendant_mutations_while_parent_is_locked() {
        let coordinator = Arc::new(FileMutationCoordinator::default());
        let parent = coordinator.lock(Path::new("/workspace/tree")).await;
        let waiting = tokio::spawn({
            let coordinator = coordinator.clone();
            async move {
                coordinator
                    .lock(Path::new("/workspace/tree/nested/file.txt"))
                    .await
            }
        });

        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(parent);
        drop(waiting.await.expect("lock task"));
    }

    #[tokio::test]
    async fn coordinator_allows_sibling_mutations() {
        let coordinator = Arc::new(FileMutationCoordinator::default());
        let _first = coordinator.lock(Path::new("/workspace/one/file.txt")).await;
        let sibling = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            coordinator.lock(Path::new("/workspace/two/file.txt")),
        )
        .await;
        assert!(sibling.is_ok());
    }

    #[test]
    fn workspace_path_rejects_escape_components() {
        assert!(WorkspacePath::parse("../outside").is_err());
        assert!(WorkspacePath::parse("/absolute").is_err());
        assert!(WorkspacePath::parse("nested/file.txt").is_ok());
    }

    #[test]
    fn atomic_replace_preserves_existing_permissions_and_cleans_temps() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let target = workspace.path().join("script.sh");
        std::fs::write(&target, "old").expect("seed file");
        let parent =
            Dir::open_ambient_dir(workspace.path(), ambient_authority()).expect("open workspace");

        #[cfg(unix)]
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("set executable mode");

        atomic_replace_file_at(&parent, Path::new("script.sh"), b"new")
            .expect("atomic replacement");
        assert_eq!(std::fs::read(&target).expect("read target"), b"new");
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&target)
                .expect("target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(
            std::fs::read_dir(workspace.path())
                .expect("read workspace")
                .all(|entry| !entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".nenjo-"))
        );
    }
}
