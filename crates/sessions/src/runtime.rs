use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    CheckpointQuery, DomainState, ExecutionPhase, ScheduleState, SchedulerRuntimeSnapshot,
    SessionCheckpoint, SessionKind, SessionRecord, SessionRefs, SessionStatus,
    SessionTranscriptEvent, SessionTranscriptEventPayload, TraceEvent, TranscriptQuery,
    TranscriptState, WorktreeSnapshot,
};

/// Host-facing session persistence abstraction.
///
/// Harnesses emit normalized session events through this trait. Concrete hosts
/// decide whether those events go to local files, a platform API, a database, or
/// nowhere.
#[async_trait]
pub trait SessionRuntime: Send + Sync {
    async fn record(&self, event: SessionRuntimeEvent) -> Result<()>;

    async fn get_session(&self, _session_id: Uuid) -> Result<Option<SessionRecord>> {
        Ok(None)
    }

    async fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        Ok(Vec::new())
    }

    async fn delete_session(&self, _session_id: Uuid) -> Result<()> {
        Ok(())
    }

    async fn read_transcript(
        &self,
        _session_id: Uuid,
        _query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>> {
        Ok(Vec::new())
    }

    async fn append_transcript(
        &self,
        _append: SessionTranscriptAppend,
    ) -> Result<Option<SessionTranscriptEvent>> {
        Ok(None)
    }

    async fn load_latest_checkpoint(
        &self,
        _session_id: Uuid,
        _query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        Ok(None)
    }

    async fn update_checkpoint(&self, _update: SessionCheckpointUpdate) -> Result<bool> {
        Ok(false)
    }

    async fn transition_session(&self, _transition: SessionTransition) -> Result<bool> {
        Ok(false)
    }

    async fn upsert_scheduler_session(&self, _upsert: SchedulerSessionUpsert) -> Result<bool> {
        Ok(false)
    }

    async fn upsert_chat_session(&self, upsert: ChatSessionUpsert) -> Result<bool> {
        self.record(SessionRuntimeEvent::SessionUpsert(SessionUpsert {
            session_id: upsert.session_id,
            kind: SessionKind::Chat,
            status: upsert.status,
            agent_id: Some(upsert.agent_id),
            project_id: upsert.project_id,
            task_id: None,
            routine_id: None,
            execution_run_id: None,
            parent_session_id: None,
            lease: None,
            memory_namespace: upsert.memory_namespace.clone(),
            refs: SessionRefs {
                trace_ref: upsert.trace_ref,
                memory_namespace: upsert.memory_namespace,
                ..Default::default()
            },
            metadata: upsert.metadata,
        }))
        .await?;
        Ok(true)
    }

    async fn upsert_task_session(&self, upsert: TaskSessionUpsert) -> Result<bool> {
        self.record(SessionRuntimeEvent::SessionUpsert(SessionUpsert {
            session_id: upsert.task_id,
            kind: SessionKind::Task,
            status: upsert.status,
            agent_id: upsert.agent_id,
            project_id: Some(upsert.project_id),
            task_id: Some(upsert.task_id),
            routine_id: upsert.routine_id,
            execution_run_id: Some(upsert.execution_run_id),
            parent_session_id: None,
            lease: None,
            memory_namespace: upsert.memory_namespace.clone(),
            refs: SessionRefs {
                trace_ref: upsert.trace_ref,
                memory_namespace: upsert.memory_namespace,
                ..Default::default()
            },
            metadata: upsert.metadata,
        }))
        .await?;
        Ok(true)
    }

    async fn upsert_domain_session(&self, _upsert: DomainSessionUpsert) -> Result<bool> {
        Ok(false)
    }

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
    async fn record(&self, event: SessionRuntimeEvent) -> Result<()> {
        (**self).record(event).await
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

    async fn append_transcript(
        &self,
        append: SessionTranscriptAppend,
    ) -> Result<Option<SessionTranscriptEvent>> {
        (**self).append_transcript(append).await
    }

    async fn load_latest_checkpoint(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        (**self).load_latest_checkpoint(session_id, query).await
    }

    async fn update_checkpoint(&self, update: SessionCheckpointUpdate) -> Result<bool> {
        (**self).update_checkpoint(update).await
    }

    async fn transition_session(&self, transition: SessionTransition) -> Result<bool> {
        (**self).transition_session(transition).await
    }

    async fn upsert_scheduler_session(&self, upsert: SchedulerSessionUpsert) -> Result<bool> {
        (**self).upsert_scheduler_session(upsert).await
    }

    async fn upsert_chat_session(&self, upsert: ChatSessionUpsert) -> Result<bool> {
        (**self).upsert_chat_session(upsert).await
    }

    async fn upsert_task_session(&self, upsert: TaskSessionUpsert) -> Result<bool> {
        (**self).upsert_task_session(upsert).await
    }

    async fn upsert_domain_session(&self, upsert: DomainSessionUpsert) -> Result<bool> {
        (**self).upsert_domain_session(upsert).await
    }

    async fn session_memory_namespace(&self, session_id: Uuid) -> Result<Option<String>> {
        (**self).session_memory_namespace(session_id).await
    }
}

#[derive(Debug, Clone)]
pub enum SessionRuntimeEvent {
    SessionUpsert(SessionUpsert),
    Transcript(SessionTranscriptRecord),
    Trace(TraceEvent),
    Checkpoint(CheckpointRecord),
}

#[derive(Debug, Clone)]
pub struct SessionUpsert {
    pub session_id: Uuid,
    pub kind: SessionKind,
    pub status: SessionStatus,
    pub agent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub routine_id: Option<Uuid>,
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
    pub project_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub routine_id: Option<Uuid>,
    pub worker_id: String,
    pub memory_namespace: Option<String>,
    pub scheduler: ScheduleState,
    pub progress_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChatSessionUpsert {
    pub session_id: Uuid,
    pub status: SessionStatus,
    pub project_id: Option<Uuid>,
    pub agent_id: Uuid,
    pub memory_namespace: Option<String>,
    pub trace_ref: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct TaskSessionUpsert {
    pub task_id: Uuid,
    pub status: SessionStatus,
    pub project_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub routine_id: Option<Uuid>,
    pub execution_run_id: Uuid,
    pub memory_namespace: Option<String>,
    pub trace_ref: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct DomainSessionUpsert {
    pub session_id: Uuid,
    pub status: SessionStatus,
    pub project_id: Option<Uuid>,
    pub agent_id: Uuid,
    pub worker_id: String,
    pub memory_namespace: Option<String>,
    pub domain: Option<DomainState>,
}

pub struct NoopSessionRuntime;

#[async_trait]
impl SessionRuntime for NoopSessionRuntime {
    async fn record(&self, _event: SessionRuntimeEvent) -> Result<()> {
        Ok(())
    }
}
