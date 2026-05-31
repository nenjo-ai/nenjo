use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nenjo_sessions::{
    CheckpointQuery, CheckpointStore, DomainSessionUpsert, SchedulerSessionUpsert,
    SessionCheckpoint, SessionCheckpointUpdate, SessionKind, SessionLease, SessionLeaseGrant,
    SessionLeaseRequest, SessionRecord, SessionRefs, SessionRuntime, SessionRuntimeEvent,
    SessionStatus, SessionStore, SessionSummary, SessionTranscriptAppend, SessionTranscriptEvent,
    SessionTransition, SessionUpsert, SessionWriteOutcome, TraceEvent, TraceStore, TranscriptQuery,
    TranscriptStore,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::warn;
use uuid::Uuid;

use super::event_store::FileSessionStores;
use super::lease_store::SessionLeaseStore;

#[async_trait]
/// Restores durable local sessions that were active when a process stopped.
///
/// `FileSessionRuntime` calls this hook during recovery for session kinds that
/// require host integration, such as domain runners, cron schedules, and agent
/// heartbeats. Embedded users can keep the default no-op methods when they only
/// need persisted records/transcripts/traces.
pub trait SessionRecoveryHandler: Send + Sync {
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
    pub project: Option<String>,
    pub agent: String,
    pub domain_command: String,
}

#[derive(Debug, Clone)]
pub struct CronSessionRecovery {
    pub session_id: Uuid,
    pub project: Option<String>,
    pub routine: Option<String>,
    pub schedule_expr: String,
    pub timezone: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct HeartbeatSessionRecovery {
    pub session_id: Uuid,
    pub agent: String,
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
    lease_store: SessionLeaseStore,
    record_locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
    worker_id: String,
}

impl FileSessionRuntime {
    /// Create a local file-backed session runtime.
    ///
    /// This uses the default host id `"local"`. Use
    /// [`with_host`](Self::with_host) when persisted leases should identify a
    /// specific host or process.
    pub fn new(stores: FileSessionStores) -> Self {
        Self::with_host(stores, "local")
    }

    /// Create a local file-backed session runtime with an explicit host id.
    ///
    /// The host id is written into active session leases. A single-process app
    /// can usually use [`new`](Self::new); named services and workers should
    /// use this constructor.
    pub fn with_host(stores: FileSessionStores, host_id: impl Into<String>) -> Self {
        Self {
            records: Arc::new(stores.records),
            transcripts: Arc::new(stores.transcripts),
            traces: Arc::new(stores.traces),
            checkpoints: Arc::new(stores.checkpoints),
            lease_store: SessionLeaseStore::new(),
            record_locks: Arc::new(DashMap::new()),
            worker_id: host_id.into(),
        }
    }

    /// Stable identifier written into active session leases.
    pub fn host_id(&self) -> &str {
        &self.worker_id
    }

    #[cfg(test)]
    async fn record_guard(&self, session_id: Uuid) -> OwnedMutexGuard<()> {
        self.record_locks
            .entry(session_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
            .lock_owned()
            .await
    }

    fn try_record_guard(&self, session_id: Uuid) -> Option<OwnedMutexGuard<()>> {
        self.record_locks
            .entry(session_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
            .try_lock_owned()
            .ok()
    }

    fn handle_session_upsert(
        &self,
        grant: &SessionLeaseGrant,
        upsert: SessionUpsert,
    ) -> Result<()> {
        let now = Utc::now();
        let mut record = self
            .records
            .get(upsert.session_id)?
            .unwrap_or(SessionRecord {
                session_id: upsert.session_id,
                kind: upsert.kind,
                status: upsert.status,
                project: upsert.project.clone(),
                agent: upsert.agent.clone(),
                task_id: upsert.task_id,
                routine: upsert.routine.clone(),
                execution_run_id: upsert.execution_run_id,
                parent_session_id: upsert.parent_session_id,
                version: 0,
                refs: SessionRefs::default(),
                lease: Default::default(),
                scheduler: None,
                domain: None,
                summary: SessionSummary::default(),
                metadata: serde_json::Value::Null,
                created_at: now,
                updated_at: now,
                completed_at: None,
            });

        record.kind = upsert.kind;
        record.status = upsert.status;
        record.agent = upsert.agent;
        record.project = upsert.project;
        record.task_id = upsert.task_id;
        record.routine = upsert.routine;
        record.execution_run_id = upsert.execution_run_id;
        record.parent_session_id = upsert.parent_session_id;
        record.metadata = upsert.metadata.clone();
        if let Some(lease) = upsert.lease {
            record.lease = lease;
        } else {
            record.lease = self.lease_from_grant_for_status(grant, upsert.status, &record.lease);
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

    fn lease_from_grant_for_status(
        &self,
        grant: &SessionLeaseGrant,
        status: SessionStatus,
        existing: &SessionLease,
    ) -> SessionLease {
        if Self::is_terminal_status(status) {
            if let Some(lease_token) = existing.lease_token {
                let _ = self.lease_store.release(grant.session_id, lease_token);
            }
            let _ = self
                .lease_store
                .release(grant.session_id, grant.lease_token);
            SessionLease::default()
        } else {
            SessionLease {
                worker_id: Some(grant.worker_id.clone()),
                lease_token: Some(grant.lease_token),
                lease_expires_at: Some(grant.lease_expires_at),
            }
        }
    }

    fn update_session_status(
        &self,
        session_id: Uuid,
        grant: &SessionLeaseGrant,
        status: SessionStatus,
    ) -> Result<bool> {
        let Some(mut record) = self.records.get(session_id)? else {
            return Ok(false);
        };
        let now = Utc::now();
        record.lease = self.lease_from_grant_for_status(grant, status, &record.lease);
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

    async fn handle_trace(&self, event: TraceEvent) -> Result<()> {
        if let Some(mut record) = self.records.get(event.session_id)?
            && record.refs.trace_ref.is_none()
        {
            record.refs.trace_ref = Some(format!("traces/{}.jsonl", event.session_id));
            record.version += 1;
            record.updated_at = event.recorded_at;
            self.records.put(&record)?;
        }

        self.traces.append(event).await
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

    async fn transition_session_record(
        &self,
        grant: &SessionLeaseGrant,
        transition: SessionTransition,
    ) -> Result<bool> {
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
        self.update_session_status(transition.session_id, grant, transition.status)
    }

    async fn handle_checkpoint(&self, record: nenjo_sessions::CheckpointRecord) -> Result<()> {
        self.save_checkpoint_record(record).await
    }

    async fn upsert_scheduler_session_record(
        &self,
        grant: &SessionLeaseGrant,
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
                project: upsert.project.clone(),
                agent: upsert.agent.clone(),
                task_id: None,
                routine: upsert.routine.clone(),
                execution_run_id: None,
                parent_session_id: None,
                version: 0,
                refs: SessionRefs::default(),
                lease: Default::default(),
                scheduler: None,
                domain: None,
                summary: SessionSummary::default(),
                metadata: serde_json::Value::Null,
                created_at: now,
                updated_at: now,
                completed_at: None,
            });

        record.kind = upsert.kind;
        record.status = upsert.status;
        record.project = upsert.project;
        record.agent = upsert.agent;
        record.routine = upsert.routine;
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
        record.lease = self.lease_from_grant_for_status(grant, upsert.status, &record.lease);
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
            let _ = self.lease_store.release(record.session_id, lease_token);
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
        handler: &(dyn SessionRecoveryHandler + Send + Sync),
    ) -> Result<()> {
        if !matches!(record.status, SessionStatus::Active | SessionStatus::Paused) {
            return Ok(());
        }

        match record.kind {
            SessionKind::Domain => {
                let Some(domain) = record.domain.clone() else {
                    return Ok(());
                };
                let Some(agent) = record.agent.clone() else {
                    return Ok(());
                };
                let request = DomainSessionRecovery {
                    session_id: record.session_id,
                    project: record.project.clone(),
                    agent,
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
                        project: record.project.clone(),
                        routine: record.routine.clone(),
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
                let Some(agent) = record.agent.clone() else {
                    return Ok(());
                };
                handler
                    .restore_heartbeat_session(HeartbeatSessionRecovery {
                        session_id: record.session_id,
                        agent,
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

    async fn upsert_domain_session_record(
        &self,
        grant: &SessionLeaseGrant,
        upsert: DomainSessionUpsert,
    ) -> Result<bool> {
        let now = Utc::now();
        let mut record = self
            .records
            .get(upsert.session_id)?
            .unwrap_or(SessionRecord {
                session_id: upsert.session_id,
                kind: SessionKind::Domain,
                status: upsert.status,
                project: upsert.project.clone(),
                agent: Some(upsert.agent.clone()),
                task_id: None,
                routine: None,
                execution_run_id: None,
                parent_session_id: None,
                version: 0,
                refs: SessionRefs::default(),
                lease: Default::default(),
                scheduler: None,
                domain: None,
                summary: SessionSummary::default(),
                metadata: serde_json::Value::Null,
                created_at: now,
                updated_at: now,
                completed_at: None,
            });

        record.kind = SessionKind::Domain;
        record.status = upsert.status;
        record.project = upsert.project;
        record.agent = Some(upsert.agent);
        record.metadata = upsert.metadata;
        record.refs.memory_namespace = upsert.memory_namespace;
        record.domain = upsert.domain;
        record.version += 1;
        record.updated_at = now;
        record.completed_at = if Self::is_terminal_status(upsert.status) {
            Some(now)
        } else {
            None
        };
        record.lease = self.lease_from_grant_for_status(grant, upsert.status, &record.lease);
        self.records.put(&record)?;
        Ok(true)
    }

    async fn apply_session_event(
        &self,
        grant: &SessionLeaseGrant,
        event: SessionRuntimeEvent,
    ) -> Result<()> {
        match event {
            SessionRuntimeEvent::SessionUpsert(upsert) => self.handle_session_upsert(grant, upsert),
            SessionRuntimeEvent::SchedulerUpsert(upsert) => {
                self.upsert_scheduler_session_record(grant, upsert).await?;
                Ok(())
            }
            SessionRuntimeEvent::DomainUpsert(upsert) => {
                self.upsert_domain_session_record(grant, upsert).await?;
                Ok(())
            }
            SessionRuntimeEvent::Transcript(record) => self.handle_transcript(record).await,
            SessionRuntimeEvent::TranscriptAppend(append) => {
                let _ = self.append_transcript_record(append).await?;
                Ok(())
            }
            SessionRuntimeEvent::Trace(event) => self.handle_trace(event).await,
            SessionRuntimeEvent::Checkpoint(record) => self.handle_checkpoint(record).await,
            SessionRuntimeEvent::CheckpointUpdate(update) => {
                self.update_checkpoint_record(update).await?;
                Ok(())
            }
            SessionRuntimeEvent::Transition(transition) => {
                self.transition_session_record(grant, transition).await?;
                Ok(())
            }
        }
    }

    pub async fn record(&self, event: SessionRuntimeEvent) -> Result<()> {
        let session_id = event.session_id();
        let grant =
            self.lease_store
                .acquire(session_id, &self.worker_id, Duration::from_secs(120))?;
        let result = <Self as SessionRuntime>::record(self, &grant, event).await;
        let release_result = self
            .lease_store
            .release(grant.session_id, grant.lease_token);
        release_result?;
        result
    }

    pub async fn append_transcript(
        &self,
        append: SessionTranscriptAppend,
    ) -> Result<Option<SessionTranscriptEvent>> {
        let session_id = append.session_id;
        let grant =
            self.lease_store
                .acquire(session_id, &self.worker_id, Duration::from_secs(120))?;
        let result = self.append_transcript_record(append).await;
        let release_result = self
            .lease_store
            .release(grant.session_id, grant.lease_token);
        release_result?;
        result
    }

    pub async fn update_checkpoint(&self, update: SessionCheckpointUpdate) -> Result<bool> {
        let session_id = update.session_id;
        let grant =
            self.lease_store
                .acquire(session_id, &self.worker_id, Duration::from_secs(120))?;
        let result = self.update_checkpoint_record(update).await;
        let release_result = self
            .lease_store
            .release(grant.session_id, grant.lease_token);
        release_result?;
        result
    }

    pub async fn transition_session(&self, transition: SessionTransition) -> Result<bool> {
        let session_id = transition.session_id;
        let grant =
            self.lease_store
                .acquire(session_id, &self.worker_id, Duration::from_secs(120))?;
        let result = self.transition_session_record(&grant, transition).await;
        let release_result = self
            .lease_store
            .release(grant.session_id, grant.lease_token);
        release_result?;
        result
    }

    pub async fn recover_reconcilable_sessions(
        &self,
        handler: &(dyn SessionRecoveryHandler + Send + Sync),
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
    async fn acquire_session_lease(
        &self,
        request: SessionLeaseRequest,
    ) -> Result<SessionLeaseGrant> {
        self.lease_store
            .acquire(request.session_id, &request.worker_id, request.ttl)
    }

    async fn renew_session_lease(&self, grant: &SessionLeaseGrant, ttl: Duration) -> Result<bool> {
        Ok(self
            .lease_store
            .renew(grant.session_id, grant.lease_token, ttl)?
            .is_some())
    }

    async fn release_session_lease(&self, grant: SessionLeaseGrant) -> Result<()> {
        self.lease_store
            .release(grant.session_id, grant.lease_token)
    }

    async fn record_batch(
        &self,
        grant: &SessionLeaseGrant,
        events: Vec<SessionRuntimeEvent>,
    ) -> Result<SessionWriteOutcome> {
        if events.is_empty() {
            return Ok(SessionWriteOutcome { applied: 0 });
        }
        if events
            .iter()
            .any(|event| event.session_id() != grant.session_id)
        {
            anyhow::bail!(
                "session write batch contains event for a different session than lease {}",
                grant.session_id
            );
        }

        let Some(_guard) = self.try_record_guard(grant.session_id) else {
            warn!(
                session_id = %grant.session_id,
                worker_id = %grant.worker_id,
                lease_token = %grant.lease_token,
                "Skipping session write batch because session lock is busy"
            );
            anyhow::bail!("session {} lock is busy", grant.session_id);
        };
        let applied = events.len();
        for event in events {
            self.apply_session_event(grant, event).await?;
        }
        Ok(SessionWriteOutcome { applied })
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

    async fn load_latest_checkpoint(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        self.checkpoints.load_latest(session_id, query).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;
    use nenjo_sessions::{
        CheckpointStore, ExecutionPhase, SessionCheckpointUpdate, SessionKind, SessionLeaseRequest,
        SessionOwnerKind, SessionRecord, SessionRefs, SessionRuntime, SessionRuntimeEvent,
        SessionStatus, SessionStore, SessionSummary, SessionTranscriptAppend,
        SessionTranscriptChatMessage, SessionTranscriptEventPayload, SessionTransition,
        SessionUpsert, TokenUsage, TraceEvent, TracePhase, TranscriptQuery, TranscriptState,
        WorktreeSnapshot,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{FileSessionRuntime, SessionRecoveryHandler};
    use crate::local_runtime::FileSessionStores;

    fn test_record(session_id: Uuid, status: SessionStatus) -> SessionRecord {
        let now = Utc::now();
        SessionRecord {
            session_id,
            kind: SessionKind::Task,
            status,
            project: None,
            agent: None,
            task_id: Some(session_id),
            routine: None,
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
            metadata: serde_json::Value::Null,
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }

    fn lease_request(session_id: Uuid, worker_id: &str) -> SessionLeaseRequest {
        SessionLeaseRequest {
            session_id,
            worker_id: worker_id.to_string(),
            owner_kind: SessionOwnerKind::Chat,
            ttl: Duration::from_secs(30),
        }
    }

    fn trace_event(session_id: Uuid) -> SessionRuntimeEvent {
        SessionRuntimeEvent::Trace(TraceEvent {
            session_id,
            turn_id: None,
            recorded_at: Utc::now(),
            phase: TracePhase::Completed,
            agent_id: None,
            agent_name: None,
            tool_name: None,
            parent_tool_name: None,
            ability_name: None,
            target_agent_id: None,
            target_agent_name: None,
            success: Some(true),
            usage: TokenUsage::default(),
            preview: Some("done".to_string()),
            task_input: None,
            final_output: Some("done".to_string()),
            tool_args: None,
            error_preview: None,
            metadata: serde_json::Value::Null,
        })
    }

    #[tokio::test]
    async fn acquire_session_lease_rejects_different_active_worker() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let runtime = FileSessionRuntime::with_host(stores, "test");
        let session_id = Uuid::new_v4();

        runtime
            .acquire_session_lease(lease_request(session_id, "worker-a"))
            .await
            .unwrap();

        let error = runtime
            .acquire_session_lease(lease_request(session_id, "worker-b"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already leased by worker"));

        runtime
            .acquire_session_lease(lease_request(session_id, "worker-a"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn record_batch_rejects_events_outside_lease_session() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let runtime = FileSessionRuntime::with_host(stores, "test");
        let session_id = Uuid::new_v4();
        let other_session_id = Uuid::new_v4();
        let grant = runtime
            .acquire_session_lease(lease_request(session_id, "worker-a"))
            .await
            .unwrap();

        let error = runtime
            .record_batch(&grant, vec![trace_event(other_session_id)])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("different session"));
    }

    #[tokio::test]
    async fn record_batch_fails_fast_when_session_write_lock_is_busy() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let runtime = FileSessionRuntime::with_host(stores, "test");
        let session_id = Uuid::new_v4();
        let _guard = runtime.record_guard(session_id).await;
        let grant = runtime
            .acquire_session_lease(lease_request(session_id, "worker-a"))
            .await
            .unwrap();

        let error = runtime
            .record_batch(&grant, vec![trace_event(session_id)])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("lock is busy"));
    }

    #[tokio::test]
    async fn session_writes_preserve_existing_lease_grant() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let records = stores.records.clone();
        let runtime = FileSessionRuntime::with_host(stores, "test");
        let session_id = Uuid::new_v4();
        let grant = runtime
            .acquire_session_lease(lease_request(session_id, "worker-a"))
            .await
            .unwrap();

        runtime
            .record_batch(
                &grant,
                vec![
                    SessionRuntimeEvent::SessionUpsert(SessionUpsert {
                        session_id,
                        kind: SessionKind::Task,
                        status: SessionStatus::Active,
                        agent: Some("tester".to_string()),
                        project: Some("core".to_string()),
                        task_id: Some(session_id),
                        routine: None,
                        execution_run_id: Some(Uuid::new_v4()),
                        parent_session_id: None,
                        lease: None,
                        memory_namespace: None,
                        refs: SessionRefs::default(),
                        metadata: serde_json::Value::Null,
                    }),
                    SessionRuntimeEvent::Transition(SessionTransition {
                        session_id,
                        worker_id: "worker-a".to_string(),
                        phase: Some(ExecutionPhase::CallingModel),
                        status: SessionStatus::Active,
                    }),
                ],
            )
            .await
            .unwrap();

        assert!(
            runtime
                .renew_session_lease(&grant, Duration::from_secs(30))
                .await
                .unwrap()
        );
        let record = records.get(session_id).unwrap().unwrap();
        assert_eq!(record.lease.lease_token, Some(grant.lease_token));
    }

    #[tokio::test]
    async fn checkpoint_updates_advance_sequence_and_preserve_worktree() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let records = stores.records.clone();
        let checkpoints = stores.checkpoints.clone();
        let runtime = FileSessionRuntime::with_host(stores, "test");
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
        let runtime = FileSessionRuntime::with_host(stores, "test");
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
        let runtime = FileSessionRuntime::with_host(stores, "test");
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
        impl SessionRecoveryHandler for NoopRecovery {}

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
        let runtime = FileSessionRuntime::with_host(stores, "test");
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

    #[tokio::test]
    async fn trace_events_create_trace_ref_when_missing() {
        let dir = tempdir().unwrap();
        let stores = FileSessionStores::new(dir.path());
        let records = stores.records.clone();
        let runtime = FileSessionRuntime::with_host(stores, "test");
        let session_id = Uuid::new_v4();

        records
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        runtime
            .record(SessionRuntimeEvent::Trace(TraceEvent {
                session_id,
                turn_id: None,
                recorded_at: Utc::now(),
                phase: TracePhase::Completed,
                agent_id: None,
                agent_name: None,
                tool_name: None,
                parent_tool_name: None,
                ability_name: None,
                target_agent_id: None,
                target_agent_name: None,
                success: Some(true),
                usage: TokenUsage::default(),
                preview: Some("done".to_string()),
                task_input: None,
                final_output: Some("done".to_string()),
                tool_args: None,
                error_preview: None,
                metadata: serde_json::Value::Null,
            }))
            .await
            .unwrap();

        let record = records.get(session_id).unwrap().unwrap();
        assert_eq!(
            record.refs.trace_ref.as_deref(),
            Some(format!("traces/{session_id}.jsonl").as_str())
        );
        assert!(
            dir.path()
                .join("events")
                .join("traces")
                .join(format!("{session_id}.jsonl"))
                .exists()
        );
    }
}
