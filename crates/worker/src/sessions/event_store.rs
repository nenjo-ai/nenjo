use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nenjo_sessions::{
    CheckpointQuery, CheckpointStore, SessionCheckpoint, SessionStatus, SessionStore,
    SessionTranscriptEvent, TraceEvent, TraceQuery, TraceStore, TranscriptQuery, TranscriptStore,
};
use uuid::Uuid;

use super::FileSessionStore;

#[derive(Debug, Clone)]
pub struct WorkerSessionStores {
    pub records: FileSessionStore,
    pub transcripts: FileTranscriptStore,
    pub traces: FileTraceStore,
    pub checkpoints: FileCheckpointStore,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionCleanupReport {
    pub scanned: usize,
    pub deleted: usize,
    pub retained: usize,
}

impl WorkerSessionStores {
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        let state_dir = state_dir.as_ref();
        let events_dir = state_dir.join("events");
        Self {
            records: FileSessionStore::new(&state_dir.join("sessions")),
            transcripts: FileTranscriptStore::new(events_dir.join("transcripts")),
            traces: FileTraceStore::new(events_dir.join("traces")),
            checkpoints: FileCheckpointStore::new(events_dir.join("checkpoints")),
        }
    }

    pub fn delete_session_files(&self, session_id: Uuid) -> Result<()> {
        self.transcripts.delete(session_id)?;
        self.traces.delete(session_id)?;
        self.checkpoints.delete(session_id)?;
        self.records.delete(session_id)
    }

    pub fn prune_terminal_sessions(&self, retention_days: u64) -> Result<SessionCleanupReport> {
        let cutoff = retention_cutoff(retention_days);
        let mut report = SessionCleanupReport::default();

        for record in self.records.list()? {
            report.scanned += 1;
            if !is_terminal(record.status)
                || retention_anchor(record.completed_at, record.updated_at) > cutoff
            {
                report.retained += 1;
                continue;
            }

            self.delete_session_files(record.session_id)?;
            report.deleted += 1;
        }

        Ok(report)
    }
}

fn retention_cutoff(retention_days: u64) -> DateTime<Utc> {
    let days = retention_days.min(i64::MAX as u64) as i64;
    Utc::now() - chrono::Duration::days(days)
}

fn retention_anchor(
    completed_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
) -> DateTime<Utc> {
    completed_at.unwrap_or(updated_at)
}

fn is_terminal(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
    )
}

#[derive(Debug, Clone)]
pub struct FileTranscriptStore {
    root: PathBuf,
}

impl FileTranscriptStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn path(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl"))
    }

    pub fn delete(&self, session_id: Uuid) -> Result<()> {
        remove_file_if_exists(self.path(session_id))
    }
}

#[async_trait]
impl TranscriptStore for FileTranscriptStore {
    async fn append(&self, mut event: SessionTranscriptEvent) -> Result<u64> {
        let mut events = read_jsonl::<SessionTranscriptEvent>(&self.path(event.session_id))?;
        let seq = events.last().map(|event| event.seq + 1).unwrap_or(1);
        event.seq = seq;
        events.push(event);
        write_jsonl(&self.path(events[0].session_id), &events)?;
        Ok(seq)
    }

    async fn read(
        &self,
        session_id: Uuid,
        query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>> {
        let mut events = read_jsonl::<SessionTranscriptEvent>(&self.path(session_id))?;
        if let Some(after_seq) = query.after_seq {
            events.retain(|event| event.seq > after_seq);
        }
        if let Some(limit) = query.limit {
            events.truncate(limit);
        }
        Ok(events)
    }
}

#[derive(Debug, Clone)]
pub struct FileTraceStore {
    root: PathBuf,
}

impl FileTraceStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn path(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl"))
    }

    pub fn delete(&self, session_id: Uuid) -> Result<()> {
        remove_file_if_exists(self.path(session_id))
    }
}

#[async_trait]
impl TraceStore for FileTraceStore {
    async fn append(&self, event: TraceEvent) -> Result<()> {
        append_jsonl(&self.path(event.session_id), &event)
    }

    async fn query(&self, query: TraceQuery) -> Result<Vec<TraceEvent>> {
        let mut events = Vec::new();

        if let Some(session_id) = query.session_id {
            events.extend(read_jsonl::<TraceEvent>(&self.path(session_id))?);
        } else if self.root.exists() {
            let mut entries: Vec<_> = std::fs::read_dir(&self.root)?.flatten().collect();
            entries.sort_by_key(|entry| entry.path());
            for entry in entries {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    events.extend(read_jsonl::<TraceEvent>(&path)?);
                }
            }
        }

        if let Some(agent_id) = query.agent_id {
            events.retain(|event| event.agent_id == Some(agent_id));
        }
        if let Some(phase) = query.phase {
            events.retain(|event| event.phase == phase);
        }
        events.sort_by_key(|event| event.recorded_at);
        if let Some(limit) = query.limit {
            events.truncate(limit);
        }

        Ok(events)
    }
}

#[derive(Debug, Clone)]
pub struct FileCheckpointStore {
    root: PathBuf,
}

impl FileCheckpointStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn path(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl"))
    }

    pub fn delete(&self, session_id: Uuid) -> Result<()> {
        remove_file_if_exists(self.path(session_id))
    }
}

#[async_trait]
impl CheckpointStore for FileCheckpointStore {
    async fn save(&self, checkpoint: SessionCheckpoint) -> Result<()> {
        append_jsonl(&self.path(checkpoint.session_id), &checkpoint)
    }

    async fn load_latest(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        let mut checkpoints = read_jsonl::<SessionCheckpoint>(&self.path(session_id))?;
        if let Some(max_seq) = query.before_or_at_seq {
            checkpoints.retain(|checkpoint| checkpoint.seq <= max_seq);
        }
        Ok(checkpoints
            .into_iter()
            .max_by_key(|checkpoint| checkpoint.seq))
    }
}

fn safe_existing_path(path: &Path) -> Result<()> {
    for component in path.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) => bail!("invalid session storage path"),
            Component::Normal(_) | Component::CurDir | Component::RootDir => {}
        }
    }
    Ok(())
}

fn append_jsonl<T>(path: &Path, value: &T) -> Result<()>
where
    T: serde::Serialize,
{
    safe_existing_path(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, value)?;
    use std::io::Write;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

fn write_jsonl<T>(path: &Path, values: &[T]) -> Result<()>
where
    T: serde::Serialize,
{
    safe_existing_path(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        for value in values {
            serde_json::to_writer(&mut file, value)?;
            use std::io::Write;
            file.write_all(b"\n")?;
        }
        use std::io::Write;
        file.flush()?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn read_jsonl<T>(path: &Path) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    safe_existing_path(path)?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<T>, _>>()
        .map_err(Into::into)
}

fn remove_file_if_exists(path: impl AsRef<Path>) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerSessionStores;
    use chrono::{Duration, Utc};
    use nenjo_sessions::{
        SessionKind, SessionRecord, SessionRefs, SessionStatus, SessionStore, SessionSummary,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    fn record(session_id: Uuid, status: SessionStatus, days_old: i64) -> SessionRecord {
        let timestamp = Utc::now() - Duration::days(days_old);
        SessionRecord {
            session_id,
            kind: SessionKind::Chat,
            status,
            project_id: None,
            agent_id: None,
            task_id: None,
            routine_id: None,
            execution_run_id: None,
            parent_session_id: None,
            version: 1,
            refs: SessionRefs {
                transcript_ref: Some(format!("transcripts/{session_id}.jsonl")),
                trace_ref: Some(format!("traces/{session_id}.jsonl")),
                checkpoint_ref: Some(format!("checkpoints/{session_id}.jsonl")),
                memory_namespace: None,
            },
            lease: Default::default(),
            scheduler: None,
            domain: None,
            summary: SessionSummary::default(),
            created_at: timestamp,
            updated_at: timestamp,
            completed_at: matches!(
                status,
                SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
            )
            .then_some(timestamp),
        }
    }

    fn write_event_files(root: &std::path::Path, session_id: Uuid) {
        for dir in ["transcripts", "traces", "checkpoints"] {
            let path = root
                .join("events")
                .join(dir)
                .join(format!("{session_id}.jsonl"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "{}\n").unwrap();
        }
    }

    #[test]
    fn prune_terminal_sessions_removes_record_and_event_files() {
        let dir = tempdir().unwrap();
        let stores = WorkerSessionStores::new(dir.path());
        let session_id = Uuid::new_v4();

        stores
            .records
            .put(&record(session_id, SessionStatus::Completed, 45))
            .unwrap();
        write_event_files(dir.path(), session_id);

        let report = stores.prune_terminal_sessions(30).unwrap();

        assert_eq!(report.scanned, 1);
        assert_eq!(report.deleted, 1);
        assert!(stores.records.get(session_id).unwrap().is_none());
        assert!(
            !dir.path()
                .join("events/transcripts")
                .join(format!("{session_id}.jsonl"))
                .exists()
        );
        assert!(
            !dir.path()
                .join("events/traces")
                .join(format!("{session_id}.jsonl"))
                .exists()
        );
        assert!(
            !dir.path()
                .join("events/checkpoints")
                .join(format!("{session_id}.jsonl"))
                .exists()
        );
    }

    #[test]
    fn prune_terminal_sessions_keeps_recent_and_non_terminal_sessions() {
        let dir = tempdir().unwrap();
        let stores = WorkerSessionStores::new(dir.path());
        let recent_terminal_id = Uuid::new_v4();
        let old_active_id = Uuid::new_v4();

        stores
            .records
            .put(&record(recent_terminal_id, SessionStatus::Failed, 2))
            .unwrap();
        stores
            .records
            .put(&record(old_active_id, SessionStatus::Active, 90))
            .unwrap();

        let report = stores.prune_terminal_sessions(30).unwrap();

        assert_eq!(report.scanned, 2);
        assert_eq!(report.deleted, 0);
        assert_eq!(report.retained, 2);
        assert!(stores.records.get(recent_terminal_id).unwrap().is_some());
        assert!(stores.records.get(old_active_id).unwrap().is_some());
    }
}
