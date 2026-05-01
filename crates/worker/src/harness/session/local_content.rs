use std::path::{Component, Path, PathBuf};
use std::{fs::OpenOptions, io::Write};

use anyhow::{Result, bail};
use nenjo_sessions::SessionContentStore;

/// File-backed session content store rooted under the worker state dir.
pub struct FileSessionContentStore {
    root: PathBuf,
}

impl FileSessionContentStore {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn path_for_key(&self, key: &str) -> Result<PathBuf> {
        let rel = Path::new(key);
        if rel.is_absolute() {
            bail!("absolute content keys are not allowed");
        }

        let mut out = self.root.clone();
        for comp in rel.components() {
            match comp {
                Component::Normal(seg) => out.push(seg),
                Component::CurDir => {}
                Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                    bail!("invalid content key path");
                }
            }
        }

        Ok(out)
    }
}

impl SessionContentStore for FileSessionContentStore {
    fn read_blob(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for_key(key)?;
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write_blob(&self, key: &str, body: &[u8]) -> Result<()> {
        let path = self.path_for_key(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    fn append_blob(&self, key: &str, body: &[u8]) -> Result<()> {
        let path = self.path_for_key(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(body)?;
        file.flush()?;
        Ok(())
    }

    fn delete_blob(&self, key: &str) -> Result<()> {
        let path = self.path_for_key(key)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FileSessionContentStore;
    use nenjo_sessions::SessionContentStore;
    use tempfile::tempdir;

    #[test]
    fn writes_reads_and_deletes_blob() {
        let dir = tempdir().unwrap();
        let store = FileSessionContentStore::new(dir.path());
        let key = "chat/session-1/history.json";

        store.write_blob(key, b"{\"ok\":true}").unwrap();
        let bytes = store.read_blob(key).unwrap().expect("blob should exist");
        assert_eq!(bytes, b"{\"ok\":true}");

        store.append_blob(key, b"\n{\"appended\":true}").unwrap();
        let bytes = store.read_blob(key).unwrap().expect("blob should exist");
        assert_eq!(bytes, b"{\"ok\":true}\n{\"appended\":true}");

        store.delete_blob(key).unwrap();
        assert!(store.read_blob(key).unwrap().is_none());
    }

    #[test]
    fn rejects_path_traversal_keys() {
        let dir = tempdir().unwrap();
        let store = FileSessionContentStore::new(dir.path());

        assert!(store.write_blob("../escape.txt", b"nope").is_err());
        assert!(store.write_blob("/absolute/path.txt", b"nope").is_err());
        assert!(store.read_blob("../escape.txt").is_err());
    }
}
