use std::path::{Path, PathBuf};

use anyhow::Result;
use nenjo_sessions::{SessionRecord, SessionStore};
use uuid::Uuid;

/// File-backed session metadata store.
#[derive(Debug, Clone)]
pub struct FileSessionStore {
    root: PathBuf,
}

impl FileSessionStore {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn path(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.json"))
    }

    fn write_atomic(&self, record: &SessionRecord) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path(record.session_id);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(record)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

impl SessionStore for FileSessionStore {
    fn list(&self) -> Result<Vec<SessionRecord>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&self.root)?.flatten().collect();
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let data = std::fs::read_to_string(&path)?;
            records.push(serde_json::from_str(&data)?);
        }

        Ok(records)
    }

    fn get(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        let path = self.path(session_id);
        match std::fs::read_to_string(path) {
            Ok(data) => Ok(Some(serde_json::from_str(&data)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn put(&self, record: &SessionRecord) -> Result<()> {
        self.write_atomic(record)
    }

    fn delete(&self, session_id: Uuid) -> Result<()> {
        let path = self.path(session_id);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn compare_and_swap(
        &self,
        session_id: Uuid,
        expected_version: u64,
        next: &SessionRecord,
    ) -> Result<bool> {
        let current = self.get(session_id)?;
        if current.as_ref().map(|r| r.version).unwrap_or_default() != expected_version {
            return Ok(false);
        }
        self.write_atomic(next)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::FileSessionStore;
    use chrono::Utc;
    use nenjo_sessions::{
        SessionKind, SessionRecord, SessionRefs, SessionStatus, SessionStore, SessionSummary,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    fn sample_record(session_id: Uuid, version: u64) -> SessionRecord {
        let now = Utc::now();
        SessionRecord {
            session_id,
            kind: SessionKind::Chat,
            status: SessionStatus::Active,
            project_id: None,
            agent_id: None,
            task_id: None,
            routine_id: None,
            execution_run_id: None,
            parent_session_id: None,
            version,
            refs: SessionRefs {
                transcript_ref: Some("transcripts/test.jsonl".to_string()),
                trace_ref: None,
                checkpoint_ref: None,
                memory_namespace: Some("agent_test_core".to_string()),
            },
            lease: Default::default(),
            scheduler: None,
            domain: None,
            summary: SessionSummary::default(),
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }

    #[test]
    fn put_get_list_and_delete_round_trip() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path());
        let session_id = Uuid::new_v4();
        let record = sample_record(session_id, 1);

        store.put(&record).unwrap();

        let loaded = store.get(session_id).unwrap().expect("record should exist");
        assert_eq!(loaded.session_id, session_id);
        assert_eq!(loaded.version, 1);
        assert_eq!(
            loaded.refs.transcript_ref.as_deref(),
            Some("transcripts/test.jsonl")
        );

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, session_id);

        store.delete(session_id).unwrap();
        assert!(store.get(session_id).unwrap().is_none());
    }

    #[test]
    fn compare_and_swap_respects_expected_version() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path());
        let session_id = Uuid::new_v4();

        let initial = sample_record(session_id, 1);
        store.put(&initial).unwrap();

        let mut next = initial.clone();
        next.version = 2;
        next.status = SessionStatus::Paused;

        assert!(!store.compare_and_swap(session_id, 0, &next).unwrap());

        assert!(store.compare_and_swap(session_id, 1, &next).unwrap());
        let loaded = store.get(session_id).unwrap().unwrap();
        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.status, SessionStatus::Paused);
    }
}
