use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use uuid::Uuid;

use crate::{
    CheckpointQuery, DomainState, ExecutionPhase, ScheduleState, SchedulerRuntimeSnapshot,
    SessionCheckpoint, SessionKind, SessionLeaseGrant, SessionRecord, SessionRefs, SessionStatus,
    SessionTranscriptEvent, SessionTranscriptEventPayload, TraceEvent, TranscriptQuery,
    TranscriptState, WorktreeSnapshot,
};

/// Host-facing session persistence abstraction.
///
/// The harness owns turn orchestration and must acquire a session lease before
/// running an agent turn. Runtimes own storage, lease enforcement, and ordered
/// application of session events.
#[async_trait]
pub trait SessionRuntime: Send + Sync {
    /// Acquire exclusive ownership for a session before an agent turn starts.
    async fn acquire_session_lease(
        &self,
        request: SessionLeaseRequest,
    ) -> Result<SessionLeaseGrant>;

    /// Extend an active lease. Returns `false` when the lease no longer owns
    /// the session.
    async fn renew_session_lease(&self, grant: &SessionLeaseGrant, ttl: Duration) -> Result<bool>;

    /// Release an active lease. Runtimes should ignore stale lease tokens.
    async fn release_session_lease(&self, grant: SessionLeaseGrant) -> Result<()>;

    /// Record one normalized session event under an active lease.
    async fn record(&self, grant: &SessionLeaseGrant, event: SessionRuntimeEvent) -> Result<()> {
        self.record_batch(grant, vec![event]).await?;
        Ok(())
    }

    /// Record an ordered batch of normalized session events under an active lease.
    async fn record_batch(
        &self,
        grant: &SessionLeaseGrant,
        events: Vec<SessionRuntimeEvent>,
    ) -> Result<SessionWriteOutcome>;

    /// Load the canonical session record for `session_id`, if it exists.
    async fn get_session(&self, _session_id: Uuid) -> Result<Option<SessionRecord>> {
        Ok(None)
    }

    /// List known sessions in runtime-defined order.
    async fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        Ok(Vec::new())
    }

    /// Delete a session and any runtime-owned session artifacts.
    async fn delete_session(&self, _session_id: Uuid) -> Result<()> {
        Ok(())
    }

    /// Read ordered transcript evidence for a session.
    async fn read_transcript(
        &self,
        _session_id: Uuid,
        _query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>> {
        Ok(Vec::new())
    }

    /// Load the newest checkpoint for a session matching `query`.
    async fn load_latest_checkpoint(
        &self,
        _session_id: Uuid,
        _query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        Ok(None)
    }

    /// Resolve the memory namespace currently bound to a session.
    async fn session_memory_namespace(&self, session_id: Uuid) -> Result<Option<String>> {
        Ok(self
            .get_session(session_id)
            .await?
            .and_then(|record| record.refs.memory_namespace))
    }
}

#[async_trait]
impl<T> SessionRuntime for std::sync::Arc<T>
where
    T: SessionRuntime + ?Sized,
{
    async fn acquire_session_lease(
        &self,
        request: SessionLeaseRequest,
    ) -> Result<SessionLeaseGrant> {
        (**self).acquire_session_lease(request).await
    }

    async fn renew_session_lease(&self, grant: &SessionLeaseGrant, ttl: Duration) -> Result<bool> {
        (**self).renew_session_lease(grant, ttl).await
    }

    async fn release_session_lease(&self, grant: SessionLeaseGrant) -> Result<()> {
        (**self).release_session_lease(grant).await
    }

    async fn record_batch(
        &self,
        grant: &SessionLeaseGrant,
        events: Vec<SessionRuntimeEvent>,
    ) -> Result<SessionWriteOutcome> {
        (**self).record_batch(grant, events).await
    }

    async fn get_session(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        (**self).get_session(session_id).await
    }

    async fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        (**self).list_sessions().await
    }

    async fn delete_session(&self, session_id: Uuid) -> Result<()> {
        (**self).delete_session(session_id).await
    }

    async fn read_transcript(
        &self,
        session_id: Uuid,
        query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>> {
        (**self).read_transcript(session_id, query).await
    }

    async fn load_latest_checkpoint(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        (**self).load_latest_checkpoint(session_id, query).await
    }

    async fn session_memory_namespace(&self, session_id: Uuid) -> Result<Option<String>> {
        (**self).session_memory_namespace(session_id).await
    }
}

#[derive(Debug, Clone)]
pub enum SessionRuntimeEvent {
    SessionUpsert(SessionUpsert),
    SchedulerUpsert(SchedulerSessionUpsert),
    DomainUpsert(DomainSessionUpsert),
    Transcript(SessionTranscriptRecord),
    TranscriptAppend(SessionTranscriptAppend),
    Trace(TraceEvent),
    Checkpoint(CheckpointRecord),
    CheckpointUpdate(SessionCheckpointUpdate),
    Transition(SessionTransition),
}

impl SessionRuntimeEvent {
    pub fn session_id(&self) -> Uuid {
        match self {
            Self::SessionUpsert(event) => event.session_id,
            Self::SchedulerUpsert(event) => event.session_id,
            Self::DomainUpsert(event) => event.session_id,
            Self::Transcript(event) => event.session_id,
            Self::TranscriptAppend(event) => event.session_id,
            Self::Trace(event) => event.session_id,
            Self::Checkpoint(event) => event.session_id,
            Self::CheckpointUpdate(event) => event.session_id,
            Self::Transition(event) => event.session_id,
        }
    }

    pub fn event_type(&self) -> SessionRuntimeEventType {
        match self {
            Self::SessionUpsert(_) => SessionRuntimeEventType::SessionUpsert,
            Self::SchedulerUpsert(_) => SessionRuntimeEventType::SchedulerUpsert,
            Self::DomainUpsert(_) => SessionRuntimeEventType::DomainUpsert,
            Self::Transcript(_) => SessionRuntimeEventType::Transcript,
            Self::TranscriptAppend(_) => SessionRuntimeEventType::TranscriptAppend,
            Self::Trace(_) => SessionRuntimeEventType::Trace,
            Self::Checkpoint(_) => SessionRuntimeEventType::Checkpoint,
            Self::CheckpointUpdate(_) => SessionRuntimeEventType::CheckpointUpdate,
            Self::Transition(_) => SessionRuntimeEventType::Transition,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRuntimeEventType {
    SessionUpsert,
    SchedulerUpsert,
    DomainUpsert,
    Transcript,
    TranscriptAppend,
    Trace,
    Checkpoint,
    CheckpointUpdate,
    Transition,
}

#[derive(Debug, Clone)]
pub struct SessionLeaseRequest {
    pub session_id: Uuid,
    pub worker_id: String,
    pub owner_kind: SessionOwnerKind,
    pub ttl: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOwnerKind {
    Chat,
    Task,
    Cron,
    Heartbeat,
    Domain,
}

#[derive(Debug, Clone, Default)]
pub struct SessionWriteOutcome {
    pub applied: usize,
}

#[derive(Debug, Clone)]
pub struct SessionUpsert {
    pub session_id: Uuid,
    pub kind: SessionKind,
    pub status: SessionStatus,
    pub agent: Option<String>,
    pub project: Option<String>,
    pub task_id: Option<Uuid>,
    pub routine: Option<String>,
    pub execution_run_id: Option<Uuid>,
    pub parent_session_id: Option<Uuid>,
    pub lease: Option<crate::SessionLease>,
    pub memory_namespace: Option<String>,
    pub refs: SessionRefs,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct SessionTranscriptRecord {
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
    pub payload: SessionTranscriptEventPayload,
}

#[derive(Debug, Clone)]
pub struct CheckpointRecord {
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
    pub checkpoint: SessionCheckpoint,
}

#[derive(Debug, Clone)]
pub struct SessionTranscriptAppend {
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
    pub payload: SessionTranscriptEventPayload,
    pub transcript_state: TranscriptState,
}

#[derive(Debug, Clone)]
pub struct SessionCheckpointUpdate {
    pub session_id: Uuid,
    pub phase: ExecutionPhase,
    pub worktree: Option<WorktreeSnapshot>,
    pub active_tool_name: Option<String>,
    pub scheduler_runtime: Option<SchedulerRuntimeSnapshot>,
}

#[derive(Debug, Clone)]
pub struct SessionTransition {
    pub session_id: Uuid,
    pub worker_id: String,
    pub phase: Option<ExecutionPhase>,
    pub status: SessionStatus,
}

#[derive(Debug, Clone)]
pub struct SchedulerSessionUpsert {
    pub session_id: Uuid,
    pub kind: SessionKind,
    pub status: SessionStatus,
    pub project: Option<String>,
    pub agent: Option<String>,
    pub routine: Option<String>,
    pub worker_id: String,
    pub memory_namespace: Option<String>,
    pub scheduler: ScheduleState,
    pub progress_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChatSessionUpsert {
    pub session_id: Uuid,
    pub status: SessionStatus,
    pub project: Option<String>,
    pub agent: String,
    pub memory_namespace: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct TaskSessionUpsert {
    pub task_id: Uuid,
    pub status: SessionStatus,
    pub project: String,
    pub agent: Option<String>,
    pub routine: Option<String>,
    pub execution_run_id: Uuid,
    pub memory_namespace: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct DomainSessionUpsert {
    pub session_id: Uuid,
    pub status: SessionStatus,
    pub project: Option<String>,
    pub agent: String,
    pub worker_id: String,
    pub memory_namespace: Option<String>,
    pub metadata: serde_json::Value,
    pub domain: Option<DomainState>,
}

pub struct NoopSessionRuntime;

#[async_trait]
impl SessionRuntime for NoopSessionRuntime {
    async fn acquire_session_lease(
        &self,
        request: SessionLeaseRequest,
    ) -> Result<SessionLeaseGrant> {
        Ok(SessionLeaseGrant {
            session_id: request.session_id,
            worker_id: request.worker_id,
            lease_token: Uuid::new_v4(),
            lease_expires_at: chrono::Utc::now()
                + chrono::Duration::from_std(request.ttl)
                    .unwrap_or_else(|_| chrono::Duration::seconds(30)),
        })
    }

    async fn renew_session_lease(
        &self,
        _grant: &SessionLeaseGrant,
        _ttl: Duration,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn release_session_lease(&self, _grant: SessionLeaseGrant) -> Result<()> {
        Ok(())
    }

    async fn record_batch(
        &self,
        _grant: &SessionLeaseGrant,
        events: Vec<SessionRuntimeEvent>,
    ) -> Result<SessionWriteOutcome> {
        Ok(SessionWriteOutcome {
            applied: events.len(),
        })
    }
}
