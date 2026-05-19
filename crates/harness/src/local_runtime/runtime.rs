use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nenjo_sessions::{
    CheckpointQuery, CheckpointStore, DomainSessionUpsert, SchedulerSessionUpsert,
    SessionCheckpoint, SessionCheckpointUpdate, SessionCoordinator, SessionKind, SessionLease,
    SessionRecord, SessionRefs, SessionRuntime, SessionRuntimeEvent, SessionStatus, SessionStore,
    SessionSummary, SessionTranscriptAppend, SessionTranscriptEvent, SessionTransition,
    SessionUpsert, TraceStore, TranscriptQuery, TranscriptStore,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::warn;
use uuid::Uuid;

use super::event_store::FileSessionStores;

#[async_trait]
/// Restores durable local sessions that were active when a process stopped.
///
/// `FileSessionRuntime` calls this hook during recovery for session kinds that
/// require host integration, such as domain runners, cron schedules, and agent
/// heartbeats. Embedded users can keep the default no-op methods when they only
/// need persisted records/transcripts/traces.
pub trait FileSessionRecoveryHandler: Send + Sync {
    /// Recreate an in-memory domain session from a persisted domain record.
    async fn restore_domain_session(&self, _request: DomainSessionRecovery) -> Result<()> {
        Ok(())
    }

    /// Re-register a persisted cron schedule with the host scheduler.
    async fn restore_cron_session(&self, _request: CronSessionRecovery) -> Result<()> {
        Ok(())
    }

    /// Re-register a persisted agent heartbeat with the host scheduler.
    async fn restore_heartbeat_session(&self, _request: HeartbeatSessionRecovery) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DomainSessionRecovery {
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub agent_id: Uuid,
    pub domain_command: String,
}

#[derive(Debug, Clone)]
pub struct CronSessionRecovery {
    pub session_id: Uuid,
    pub project_id: Option<Uuid>,
    pub schedule_expr: String,
    pub timezone: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct HeartbeatSessionRecovery {
    pub session_id: Uuid,
    pub interval: Duration,
    pub timezone: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub previous_output_ref: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub start_paused: bool,
}

#[derive(Clone)]
pub struct FileSessionRuntime {
    records: Arc<dyn SessionStore>,
    transcripts: Arc<dyn TranscriptStore>,
    traces: Arc<dyn TraceStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    coordinator: Arc<dyn SessionCoordinator>,
    record_locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
    worker_id: String,
}

impl FileSessionRuntime {
    /// Create a local file-backed session runtime.
    ///
    /// This uses [`LocalSessionCoordinator`](super::LocalSessionCoordinator)
    /// and the default host id `"local"`. Use [`with_host`](Self::with_host)
    /// or [`with_coordinator`](Self::with_coordinator) when persisted leases
    /// should identify a specific host or process.
    pub fn new(stores: FileSessionStores) -> Self {
        Self::with_host(stores, "local")
    }

    /// Create a local file-backed session runtime with an explicit host id.
    ///
    /// The host id is written into active session leases. A single-process app
    /// can usually use [`new`](Self::new); named services and workers should
    /// use this constructor or [`with_coordinator`](Self::with_coordinator).
    pub fn with_host(stores: FileSessionStores, host_id: impl Into<String>) -> Self {
        Self::with_coordinator(
            stores,
            super::coordinator::LocalSessionCoordinator::new(),
            host_id,
        )
    }

    /// Create a file-backed session runtime with an explicit coordinator.
    ///
    /// Worker processes use this to share the same lease identity and recovery
    /// semantics as the rest of the runtime.
    pub fn with_coordinator<Coordinator>(
        stores: FileSessionStores,
        coordinator: Coordinator,
        host_id: impl Into<String>,
    ) -> Self
    where
        Coordinator: SessionCoordinator + 'static,
    {
        Self {
            records: Arc::new(stores.records),
            transcripts: Arc::new(stores.transcripts),
            traces: Arc::new(stores.traces),
            checkpoints: Arc::new(stores.checkpoints),
            coordinator: Arc::new(coordinator),
            record_locks: Arc::new(DashMap::new()),
            worker_id: host_id.into(),
        }
    }

    /// Stable identifier written into active session leases.
    pub fn host_id(&self) -> &str {
        &self.worker_id
    }

    async fn record_guard(&self, session_id: Uuid) -> OwnedMutexGuard<()> {
        self.record_locks
            .entry(session_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
            .lock_owned()
            .await
    }

    fn handle_session_upsert(&self, upsert: SessionUpsert) -> Result<()> {
        let now = Utc::now();
        let mut record = self
            .records
            .get(upsert.session_id)?
            .unwrap_or(SessionRecord {
                session_id: upsert.session_id,
                kind: upsert.kind,
                status: upsert.status,
                project_id: upsert.project_id,
                agent_id: upsert.agent_id,
                task_id: upsert.task_id,
                routine_id: upsert.routine_id,
                execution_run_id: upsert.execution_run_id,
                parent_session_id: upsert.parent_session_id,
                version: 0,
                refs: SessionRefs::default(),
                lease: Default::default(),
                scheduler: None,
                domain: None,
                summary: SessionSummary::default(),
                created_at: now,
                updated_at: now,
                completed_at: None,
            });

        record.kind = upsert.kind;
        record.status = upsert.status;
        record.agent_id = upsert.agent_id;
        record.project_id = upsert.project_id;
        record.task_id = upsert.task_id;
        record.routine_id = upsert.routine_id;
        record.execution_run_id = upsert.execution_run_id;
        record.parent_session_id = upsert.parent_session_id;
        if let Some(lease) = upsert.lease {
            record.lease = lease;
        } else if upsert.kind == SessionKind::Task {
            record.lease = self.lease_for_status(
                upsert.session_id,
                &self.worker_id,
                upsert.status,
                &record.lease,
            );
        }
        record.version += 1;
        record.updated_at = now;
        record.completed_at = if Self::is_terminal_status(upsert.status) {
            Some(now)
        } else {
            None
        };
        record.refs.transcript_ref = upsert
            .refs
            .transcript_ref
            .or_else(|| Some(format!("transcripts/{}.jsonl", upsert.session_id)));
        record.refs.trace_ref = upsert
            .refs
            .trace_ref
            .or_else(|| Some(format!("traces/{}.jsonl", upsert.session_id)));
        if let Some(checkpoint_ref) = upsert.refs.checkpoint_ref {
            record.refs.checkpoint_ref = Some(checkpoint_ref);
        }
        record.refs.memory_namespace = upsert.refs.memory_namespace.or(upsert.memory_namespace);
        if let Some(progress) = upsert
            .metadata
            .get("progress")
            .and_then(serde_json::Value::as_str)
        {
            record.summary.last_progress_message = Some(progress.to_string());
        }

        self.records.put(&record)
    }

    fn is_terminal_status(status: SessionStatus) -> bool {
        matches!(
            status,
            SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
        )
    }

    fn lease_for_status(
        &self,
        session_id: Uuid,
        worker_id: &str,
        status: SessionStatus,
        existing: &SessionLease,
    ) -> SessionLease {
        if Self::is_terminal_status(status) {
            if let Some(lease_token) = existing.lease_token {
                let _ = self.coordinator.release_lease(session_id, lease_token);
            }
            SessionLease::default()
        } else {
            self.coordinator
                .acquire_lease(session_id, worker_id, std::time::Duration::from_secs(30))
                .map(|grant| SessionLease {
                    worker_id: Some(grant.worker_id),
                    lease_token: Some(grant.lease_token),
                    lease_expires_at: Some(grant.lease_expires_at),
                })
                .unwrap_or_else(|_| existing.clone())
        }
    }

    fn update_session_status(
        &self,
        session_id: Uuid,
        worker_id: &str,
        status: SessionStatus,
    ) -> Result<bool> {
        let Some(mut record) = self.records.get(session_id)? else {
            return Ok(false);
        };
        let now = Utc::now();
        record.lease = self.lease_for_status(session_id, worker_id, status, &record.lease);
        record.status = status;
        record.version += 1;
        record.updated_at = now;
        record.completed_at = if Self::is_terminal_status(status) {
            Some(now)
        } else {
            None
        };
        self.records.put(&record)?;
        Ok(true)
    }

    async fn handle_transcript(
        &self,
        record: nenjo_sessions::SessionTranscriptRecord,
    ) -> Result<()> {
        let _ = self
            .append_transcript_record(SessionTranscriptAppend {
                session_id: record.session_id,
                turn_id: record.turn_id,
                payload: record.payload,
                transcript_state: nenjo_sessions::TranscriptState::MidTurn,
            })
            .await?;
        Ok(())
    }

    async fn append_transcript_record(
        &self,
        append: SessionTranscriptAppend,
    ) -> Result<Option<SessionTranscriptEvent>> {
        const MAX_CAS_RETRIES: usize = 8;

        for _ in 0..MAX_CAS_RETRIES {
            let Some(record) = self.records.get(append.session_id)? else {
                return Ok(None);
            };

            let transcript_ref = record
                .refs
                .transcript_ref
                .clone()
                .unwrap_or_else(|| format!("transcripts/{}.jsonl", append.session_id));
            let event = SessionTranscriptEvent {
                session_id: append.session_id,
                seq: record.summary.last_transcript_seq + 1,
                recorded_at: Utc::now(),
                turn_id: append.turn_id,
                payload: append.payload.clone(),
            };

            let mut next = record.clone();
            next.refs.transcript_ref = Some(transcript_ref);
            next.summary.last_transcript_seq = event.seq;
            next.summary.transcript_state = append.transcript_state;
            next.version += 1;
            next.updated_at = event.recorded_at;

            if !self
                .records
                .compare_and_swap(append.session_id, record.version, &next)?
            {
                continue;
            }

            self.transcripts.append(event.clone()).await?;
            return Ok(Some(event));
        }

        anyhow::bail!("failed to append transcript event after compare-and-swap retries")
    }

    async fn update_checkpoint_record(&self, update: SessionCheckpointUpdate) -> Result<bool> {
        let Some(mut session) = self.records.get(update.session_id)? else {
            return Ok(false);
        };

        let saved_at = Utc::now();
        let seq = session.summary.last_checkpoint_seq + 1;
        let base = self
            .checkpoints
            .load_latest(update.session_id, Default::default())
            .await?
            .unwrap_or(SessionCheckpoint {
                session_id: update.session_id,
                seq,
                saved_at,
                current_phase: None,
                active_tool_name: None,
                worktree: None,
                scheduler_runtime: None,
            });

        let checkpoint = SessionCheckpoint {
            session_id: update.session_id,
            seq,
            saved_at,
            current_phase: Some(update.phase),
            active_tool_name: update.active_tool_name.or(base.active_tool_name),
            worktree: update.worktree.or(base.worktree),
            scheduler_runtime: update.scheduler_runtime.or(base.scheduler_runtime),
        };

        self.checkpoints.save(checkpoint).await?;
        session.summary.last_checkpoint_seq = seq;
        session.refs.checkpoint_ref = Some(format!("checkpoints/{}.jsonl", update.session_id));
        session.version += 1;
        session.updated_at = saved_at;
        self.records.put(&session)?;
        Ok(true)
    }

    async fn save_checkpoint_record(
        &self,
        mut record: nenjo_sessions::CheckpointRecord,
    ) -> Result<()> {
        let latest_seq = self
            .checkpoints
            .load_latest(record.session_id, Default::default())
            .await?
            .map(|checkpoint| checkpoint.seq)
            .unwrap_or_default();
        record.checkpoint.seq = latest_seq + 1;
        record.checkpoint.saved_at = Utc::now();
        record.checkpoint.session_id = record.session_id;
        self.checkpoints.save(record.checkpoint.clone()).await?;
        if let Some(mut session) = self.records.get(record.session_id)? {
            session.summary.last_checkpoint_seq = latest_seq + 1;
            session.refs.checkpoint_ref = Some(format!("checkpoints/{}.jsonl", record.session_id));
            session.version += 1;
            session.updated_at = Utc::now();
            let _ = self.records.put(&session);
        }
        Ok(())
    }

    async fn transition_session_record(&self, transition: SessionTransition) -> Result<bool> {
        if let Some(phase) = transition.phase {
            let _ = self
                .update_checkpoint_record(SessionCheckpointUpdate {
                    session_id: transition.session_id,
                    phase,
                    worktree: None,
                    active_tool_name: None,
                    scheduler_runtime: None,
                })
                .await?;
        }
        let worker_id = if transition.worker_id.is_empty() {
            &self.worker_id
        } else {
            &transition.worker_id
        };
        self.update_session_status(transition.session_id, worker_id, transition.status)
    }

    async fn handle_checkpoint(&self, record: nenjo_sessions::CheckpointRecord) -> Result<()> {
        self.save_checkpoint_record(record).await
    }

    async fn upsert_scheduler_session_record(
        &self,
        upsert: SchedulerSessionUpsert,
    ) -> Result<bool> {
        let now = Utc::now();
        let mut record = self
            .records
            .get(upsert.session_id)?
            .unwrap_or(SessionRecord {
                session_id: upsert.session_id,
                kind: upsert.kind,
                status: upsert.status,
                project_id: upsert.project_id,
                agent_id: upsert.agent_id,
                task_id: None,
                routine_id: upsert.routine_id,
                execution_run_id: None,
                parent_session_id: None,
                version: 0,
                refs: SessionRefs::default(),
                lease: Default::default(),
                scheduler: None,
                domain: None,
                summary: SessionSummary::default(),
                created_at: now,
                updated_at: now,
                completed_at: None,
            });

        record.kind = upsert.kind;
        record.status = upsert.status;
        record.project_id = upsert.project_id;
        record.agent_id = upsert.agent_id;
        record.routine_id = upsert.routine_id;
        record.version += 1;
        record.updated_at = now;
        record.completed_at = if Self::is_terminal_status(upsert.status) {
            Some(now)
        } else {
            None
        };
        record.refs.memory_namespace = upsert.memory_namespace;
        record.scheduler = Some(upsert.scheduler);
        if let Some(progress_message) = upsert.progress_message {
            record.summary.last_progress_message = Some(progress_message);
        }
        let worker_id = if upsert.worker_id.is_empty() {
            &self.worker_id
        } else {
            &upsert.worker_id
        };
        record.lease =
            self.lease_for_status(upsert.session_id, worker_id, upsert.status, &record.lease);
        self.records.put(&record)?;
        Ok(true)
    }

    async fn recover_reconcilable_session_record(&self, mut record: SessionRecord) -> Result<bool> {
        let checkpoint_phase = self
            .checkpoints
            .load_latest(record.session_id, Default::default())
            .await?
            .and_then(|checkpoint| checkpoint.current_phase);

        if let Some(lease_token) = record.lease.lease_token {
            let _ = self
                .coordinator
                .release_lease(record.session_id, lease_token);
        }

        record.status = SessionStatus::Waiting;
        record.lease = SessionLease::default();
        record.version += 1;
        record.updated_at = Utc::now();
        record.completed_at = None;
        record.summary.last_progress_message = Some(match checkpoint_phase {
            Some(nenjo_sessions::ExecutionPhase::Preparing) => {
                "recoverable from preparing checkpoint".to_string()
            }
            Some(nenjo_sessions::ExecutionPhase::CallingModel) => {
                "recoverable from model call checkpoint".to_string()
            }
            Some(nenjo_sessions::ExecutionPhase::ExecutingTools) => {
                "recoverable from tool execution checkpoint".to_string()
            }
            Some(nenjo_sessions::ExecutionPhase::Waiting) => {
                "recoverable from waiting checkpoint".to_string()
            }
            Some(nenjo_sessions::ExecutionPhase::Finalizing) => {
                "recoverable from finalizing checkpoint".to_string()
            }
            None => "recoverable from persisted session state".to_string(),
        });
        self.records.put(&record)?;
        Ok(true)
    }

    async fn recover_record(
        &self,
        record: SessionRecord,
        handler: &(dyn FileSessionRecoveryHandler + Send + Sync),
    ) -> Result<()> {
        if !matches!(record.status, SessionStatus::Active | SessionStatus::Paused) {
            return Ok(());
        }

        match record.kind {
            SessionKind::Domain => {
                let Some(domain) = record.domain.clone() else {
                    return Ok(());
                };
                let Some(agent_id) = record.agent_id else {
                    return Ok(());
                };
                let request = DomainSessionRecovery {
                    session_id: record.session_id,
                    project_id: record.project_id.unwrap_or_else(Uuid::nil),
                    agent_id,
                    domain_command: domain.domain_command,
                };
                if let Err(error) = handler.restore_domain_session(request).await {
                    warn!(session_id = %record.session_id, error = %error, "Failed to restore domain session");
                    let _ = self.records.delete(record.session_id);
                }
            }
            SessionKind::Chat | SessionKind::Task => {
                self.recover_reconcilable_session_record(record).await?;
            }
            SessionKind::CronSchedule => {
                if record.status != SessionStatus::Active {
                    return Ok(());
                }
                let Some(nenjo_sessions::ScheduleState::Cron(state)) = record.scheduler.clone()
                else {
                    return Ok(());
                };
                handler
                    .restore_cron_session(CronSessionRecovery {
                        session_id: record.session_id,
                        project_id: record.project_id,
                        schedule_expr: state.schedule_expr,
                        timezone: state.timezone,
                        next_run_at: state.next_run_at,
                    })
                    .await?;
            }
            SessionKind::HeartbeatSchedule => {
                let Some(nenjo_sessions::ScheduleState::Heartbeat(state)) =
                    record.scheduler.clone()
                else {
                    return Ok(());
                };
                handler
                    .restore_heartbeat_session(HeartbeatSessionRecovery {
                        session_id: record.session_id,
                        interval: std::time::Duration::from_secs(state.interval_secs.max(1)),
                        timezone: state.timezone,
                        next_run_at: state.next_run_at,
                        previous_output_ref: state.previous_output_ref,
                        last_run_at: state.last_run_at,
                        start_paused: record.status == SessionStatus::Paused,
                    })
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn recover_reconcilable_sessions(
        &self,
        handler: &(dyn FileSessionRecoveryHandler + Send + Sync),
    ) -> Result<()> {
        for record in self.records.list()? {
            if let Err(error) = self.recover_record(record.clone(), handler).await {
                warn!(session_id = %record.session_id, error = %error, "Failed to recover persisted session");
            }
        }
        Ok(())
    }
}

#[async_trait]
impl SessionRuntime for FileSessionRuntime {
    async fn record(&self, event: SessionRuntimeEvent) -> Result<()> {
        match event {
            SessionRuntimeEvent::SessionUpsert(upsert) => {
                let _guard = self.record_guard(upsert.session_id).await;
                self.handle_session_upsert(upsert)
            }
            SessionRuntimeEvent::Transcript(record) => {
                let _guard = self.record_guard(record.session_id).await;
                self.handle_transcript(record).await
            }
            SessionRuntimeEvent::Trace(event) => self.traces.append(event).await,
            SessionRuntimeEvent::Checkpoint(record) => {
                let _guard = self.record_guard(record.session_id).await;
                self.handle_checkpoint(record).await
            }
        }
    }

    async fn get_session(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        self.records.get(session_id)
    }

    async fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        self.records.list()
    }

    async fn delete_session(&self, session_id: Uuid) -> Result<()> {
        self.records.delete(session_id)
    }

    async fn read_transcript(
        &self,
        session_id: Uuid,
        query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>> {
        if self.records.get(session_id)?.is_none() {
            return Ok(Vec::new());
        }
        self.transcripts.read(session_id, query).await
    }

    async fn append_transcript(
        &self,
        append: SessionTranscriptAppend,
    ) -> Result<Option<SessionTranscriptEvent>> {
        let _guard = self.record_guard(append.session_id).await;
        self.append_transcript_record(append).await
    }

    async fn load_latest_checkpoint(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        self.checkpoints.load_latest(session_id, query).await
    }

    async fn update_checkpoint(&self, update: SessionCheckpointUpdate) -> Result<bool> {
        let _guard = self.record_guard(update.session_id).await;
        self.update_checkpoint_record(update).await
    }

    async fn upsert_scheduler_session(&self, upsert: SchedulerSessionUpsert) -> Result<bool> {
        let _guard = self.record_guard(upsert.session_id).await;
        self.upsert_scheduler_session_record(upsert).await
    }

    async fn upsert_domain_session(&self, upsert: DomainSessionUpsert) -> Result<bool> {
        let _guard = self.record_guard(upsert.session_id).await;
        let now = Utc::now();
        let mut record = self
            .records
            .get(upsert.session_id)?
            .unwrap_or(SessionRecord {
                session_id: upsert.session_id,
                kind: SessionKind::Domain,
                status: upsert.status,
                project_id: upsert.project_id,
                agent_id: Some(upsert.agent_id),
                task_id: None,
                routine_id: None,
                execution_run_id: None,
                parent_session_id: None,
                version: 0,
                refs: SessionRefs::default(),
                lease: Default::default(),
                scheduler: None,
                domain: None,
                summary: SessionSummary::default(),
                created_at: now,
                updated_at: now,
                completed_at: None,
            });

        record.kind = SessionKind::Domain;
        record.status = upsert.status;
        record.project_id = upsert.project_id;
        record.agent_id = Some(upsert.agent_id);
        record.refs.memory_namespace = upsert.memory_namespace;
        record.domain = upsert.domain;
        record.version += 1;
        record.updated_at = now;
        record.completed_at = if Self::is_terminal_status(upsert.status) {
            Some(now)
        } else {
            None
        };
        let worker_id = if upsert.worker_id.is_empty() {
            &self.worker_id
        } else {
            &upsert.worker_id
        };
        record.lease =
            self.lease_for_status(upsert.session_id, worker_id, upsert.status, &record.lease);
        self.records.put(&record)?;
        Ok(true)
    }

    async fn transition_session(&self, transition: SessionTransition) -> Result<bool> {
        let _guard = self.record_guard(transition.session_id).await;
        self.transition_session_record(transition).await
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nenjo_sessions::{
        CheckpointStore, ExecutionPhase, SessionCheckpointUpdate, SessionKind, SessionRecord,
        SessionRefs, SessionRuntime, SessionStatus, SessionStore, SessionSummary,
        SessionTranscriptAppend, SessionTranscriptChatMessage, SessionTranscriptEventPayload,
        SessionTransition, TranscriptQuery, TranscriptState, WorktreeSnapshot,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{FileSessionRecoveryHandler, FileSessionRuntime};
    use crate::local_runtime::{FileSessionStores, LocalSessionCoordinator};

    fn test_record(session_id: Uuid, status: SessionStatus) -> SessionRecord {
        let now = Utc::now();
        SessionRecord {
            session_id,
            kind: SessionKind::Task,
            status,
            project_id: None,
            agent_id: None,
            task_id: Some(session_id),
            routine_id: None,
            execution_run_id: None,
            parent_session_id: None,
            version: 0,
            refs: SessionRefs {
                transcript_ref: Some(format!("transcripts/{session_id}.jsonl")),
                trace_ref: None,
                checkpoint_ref: Some(format!("checkpoints/{session_id}.jsonl")),
                memory_namespace: Some("agent_tester_core".to_string()),
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

    #[tokio::test]
    async fn checkpoint_updates_advance_sequence_and_preserve_worktree() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let records = stores.records.clone();
        let checkpoints = stores.checkpoints.clone();
        let runtime =
            FileSessionRuntime::with_coordinator(stores, LocalSessionCoordinator::new(), "test");
        let session_id = Uuid::new_v4();

        records
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        assert!(
            runtime
                .update_checkpoint(SessionCheckpointUpdate {
                    session_id,
                    phase: ExecutionPhase::Preparing,
                    active_tool_name: None,
                    worktree: None,
                    scheduler_runtime: None,
                })
                .await
                .unwrap()
        );

        let worktree = WorktreeSnapshot {
            repo_dir: "/repo".to_string(),
            work_dir: "/repo/worktree".to_string(),
            branch: "feature/test".to_string(),
            target_branch: Some("main".to_string()),
        };
        assert!(
            runtime
                .update_checkpoint(SessionCheckpointUpdate {
                    session_id,
                    phase: ExecutionPhase::Finalizing,
                    active_tool_name: None,
                    worktree: Some(worktree.clone()),
                    scheduler_runtime: None,
                })
                .await
                .unwrap()
        );

        let checkpoint = checkpoints
            .load_latest(session_id, Default::default())
            .await
            .unwrap()
            .expect("checkpoint should exist");
        assert_eq!(checkpoint.seq, 2);
        assert_eq!(checkpoint.current_phase, Some(ExecutionPhase::Finalizing));
        assert_eq!(checkpoint.worktree.unwrap().branch, worktree.branch);

        let record = records.get(session_id).unwrap().unwrap();
        assert_eq!(record.summary.last_checkpoint_seq, 2);
    }

    #[tokio::test]
    async fn transition_session_updates_phase_and_terminal_status() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let records = stores.records.clone();
        let checkpoints = stores.checkpoints.clone();
        let runtime =
            FileSessionRuntime::with_coordinator(stores, LocalSessionCoordinator::new(), "test");
        let session_id = Uuid::new_v4();

        records
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        assert!(
            runtime
                .transition_session(SessionTransition {
                    session_id,
                    status: SessionStatus::Cancelled,
                    worker_id: "test".to_string(),
                    phase: Some(ExecutionPhase::Waiting),
                })
                .await
                .unwrap()
        );

        let record = records.get(session_id).unwrap().unwrap();
        assert_eq!(record.status, SessionStatus::Cancelled);
        assert!(record.completed_at.is_some());
        assert!(record.lease.lease_token.is_none());

        let checkpoint = checkpoints
            .load_latest(session_id, Default::default())
            .await
            .unwrap()
            .expect("checkpoint should exist");
        assert_eq!(checkpoint.current_phase, Some(ExecutionPhase::Waiting));
    }

    #[tokio::test]
    async fn recover_reconcilable_sessions_moves_task_to_waiting() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let records = stores.records.clone();
        let checkpoints = stores.checkpoints.clone();
        let runtime =
            FileSessionRuntime::with_coordinator(stores, LocalSessionCoordinator::new(), "test");
        let session_id = Uuid::new_v4();

        records
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();
        checkpoints
            .save(nenjo_sessions::SessionCheckpoint {
                session_id,
                seq: 1,
                saved_at: Utc::now(),
                current_phase: Some(ExecutionPhase::ExecutingTools),
                active_tool_name: None,
                worktree: None,
                scheduler_runtime: None,
            })
            .await
            .unwrap();

        struct NoopRecovery;
        #[async_trait::async_trait]
        impl FileSessionRecoveryHandler for NoopRecovery {}

        runtime
            .recover_reconcilable_sessions(&NoopRecovery)
            .await
            .unwrap();

        let updated = records.get(session_id).unwrap().unwrap();
        assert_eq!(updated.status, SessionStatus::Waiting);
        assert_eq!(
            updated.summary.last_progress_message.as_deref(),
            Some("recoverable from tool execution checkpoint")
        );
        assert!(updated.completed_at.is_none());
    }

    #[tokio::test]
    async fn transcript_events_persist_and_read_back() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let records = stores.records.clone();
        let runtime =
            FileSessionRuntime::with_coordinator(stores, LocalSessionCoordinator::new(), "test");
        let session_id = Uuid::new_v4();

        records
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        runtime
            .append_transcript(SessionTranscriptAppend {
                session_id,
                turn_id: None,
                payload: SessionTranscriptEventPayload::ChatMessage {
                    message: SessionTranscriptChatMessage {
                        role: "user".to_string(),
                        content: "first".to_string(),
                    },
                },
                transcript_state: TranscriptState::MidTurn,
            })
            .await
            .unwrap();
        runtime
            .append_transcript(SessionTranscriptAppend {
                session_id,
                turn_id: None,
                payload: SessionTranscriptEventPayload::ChatMessage {
                    message: SessionTranscriptChatMessage {
                        role: "assistant".to_string(),
                        content: "second".to_string(),
                    },
                },
                transcript_state: TranscriptState::Clean,
            })
            .await
            .unwrap();

        let events = runtime
            .read_transcript(session_id, TranscriptQuery::default())
            .await
            .unwrap();
        assert_eq!(events.len(), 2);

        let record = records.get(session_id).unwrap().unwrap();
        assert_eq!(
            record.refs.transcript_ref.as_deref(),
            Some(format!("transcripts/{session_id}.jsonl").as_str())
        );
        assert_eq!(record.summary.last_transcript_seq, 2);
        assert_eq!(record.summary.transcript_state, TranscriptState::Clean);
    }
}
