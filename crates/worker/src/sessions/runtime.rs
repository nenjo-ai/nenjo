use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nenjo_sessions::{
    CheckpointQuery, CheckpointStore, DomainSessionUpsert, SchedulerSessionUpsert,
    SessionCheckpoint, SessionCheckpointUpdate, SessionCoordinator, SessionKind, SessionLease,
    SessionRecord, SessionRefs, SessionRuntime, SessionRuntimeEvent, SessionStatus, SessionStore,
    SessionSummary, SessionTranscriptAppend, SessionTranscriptEvent, SessionTransition,
    SessionUpsert, TraceStore, TranscriptQuery, TranscriptStore,
};
use tracing::warn;
use uuid::Uuid;

use super::WorkerSessionStores;

#[async_trait]
pub trait WorkerSessionRecoveryHandler: Send + Sync {
    async fn restore_domain_session(&self, _request: DomainSessionRecovery) -> Result<()> {
        Ok(())
    }

    async fn restore_cron_session(&self, _request: CronSessionRecovery) -> Result<()> {
        Ok(())
    }

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
pub struct WorkerSessionRuntime {
    records: Arc<dyn SessionStore>,
    transcripts: Arc<dyn TranscriptStore>,
    traces: Arc<dyn TraceStore>,
    checkpoints: Arc<dyn CheckpointStore>,
    coordinator: Arc<dyn SessionCoordinator>,
    worker_id: String,
}

impl WorkerSessionRuntime {
    pub fn new<Coordinator>(
        stores: WorkerSessionStores,
        coordinator: Coordinator,
        worker_id: impl Into<String>,
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
            worker_id: worker_id.into(),
        }
    }

    pub(crate) fn worker_name(&self) -> &str {
        &self.worker_id
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
        handler: &(dyn WorkerSessionRecoveryHandler + Send + Sync),
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
        handler: &(dyn WorkerSessionRecoveryHandler + Send + Sync),
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
impl SessionRuntime for WorkerSessionRuntime {
    async fn record(&self, event: SessionRuntimeEvent) -> Result<()> {
        match event {
            SessionRuntimeEvent::SessionUpsert(upsert) => self.handle_session_upsert(upsert),
            SessionRuntimeEvent::Transcript(record) => self.handle_transcript(record).await,
            SessionRuntimeEvent::Trace(event) => self.traces.append(event).await,
            SessionRuntimeEvent::Checkpoint(record) => self.handle_checkpoint(record).await,
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
        self.update_checkpoint_record(update).await
    }

    async fn upsert_scheduler_session(&self, upsert: SchedulerSessionUpsert) -> Result<bool> {
        self.upsert_scheduler_session_record(upsert).await
    }

    async fn upsert_domain_session(&self, upsert: DomainSessionUpsert) -> Result<bool> {
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
        self.transition_session_record(transition).await
    }
}
