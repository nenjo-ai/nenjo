use anyhow::Result;
use chrono::{DateTime, Utc};
use nenjo_models::ChatMessage;
use nenjo_sessions::{
    CheckpointStore, ExecutionPhase, SessionCheckpoint, SessionCoordinator, SessionLease,
    SessionStatus, SessionStore, SessionTranscriptChatMessage, SessionTranscriptEvent,
    SessionTranscriptEventPayload, TranscriptState, TranscriptStore, WorktreeSnapshot,
};
use uuid::Uuid;

pub fn transcript_ref(session_id: Uuid) -> String {
    format!("transcripts/{session_id}.jsonl")
}

pub fn chat_message_to_transcript(message: &ChatMessage) -> SessionTranscriptChatMessage {
    SessionTranscriptChatMessage {
        role: message.role.clone(),
        content: message.content.clone(),
    }
}

fn transcript_message_to_chat(message: SessionTranscriptChatMessage) -> ChatMessage {
    ChatMessage {
        role: message.role,
        content: message.content,
    }
}

fn replay_transcript_history(events: &[SessionTranscriptEvent]) -> Vec<ChatMessage> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            SessionTranscriptEventPayload::ChatMessage { message } => {
                Some(transcript_message_to_chat(message.clone()))
            }
            _ => None,
        })
        .collect()
}

pub async fn load_chat_history(
    store: &dyn SessionStore,
    transcripts: &dyn TranscriptStore,
    session_id: Uuid,
) -> Result<Vec<ChatMessage>> {
    if store.get(session_id)?.is_none() {
        return Ok(Vec::new());
    }
    let events = transcripts
        .read(session_id, nenjo_sessions::TranscriptQuery::default())
        .await?;
    Ok(replay_transcript_history(&events))
}

pub async fn append_transcript_event(
    store: &dyn SessionStore,
    transcripts: &dyn TranscriptStore,
    session_id: Uuid,
    turn_id: Option<Uuid>,
    payload: SessionTranscriptEventPayload,
    transcript_state: TranscriptState,
) -> Result<Option<SessionTranscriptEvent>> {
    const MAX_CAS_RETRIES: usize = 8;

    for _ in 0..MAX_CAS_RETRIES {
        let Some(record) = store.get(session_id)? else {
            return Ok(None);
        };

        let transcript_ref = record
            .refs
            .transcript_ref
            .clone()
            .unwrap_or_else(|| transcript_ref(session_id));
        let event = SessionTranscriptEvent {
            session_id,
            seq: record.summary.last_transcript_seq + 1,
            recorded_at: Utc::now(),
            turn_id,
            payload: payload.clone(),
        };

        let mut next = record.clone();
        next.refs.transcript_ref = Some(transcript_ref);
        next.summary.last_transcript_seq = event.seq;
        next.summary.transcript_state = transcript_state;
        next.version += 1;
        next.updated_at = event.recorded_at;

        if !store.compare_and_swap(session_id, record.version, &next)? {
            continue;
        }

        transcripts.append(event.clone()).await?;
        return Ok(Some(event));
    }

    anyhow::bail!("failed to append transcript event after compare-and-swap retries")
}

pub async fn update_session_checkpoint<F>(
    store: &dyn SessionStore,
    checkpoints: &dyn CheckpointStore,
    session_id: Uuid,
    mutator: F,
) -> Result<bool>
where
    F: FnOnce(SessionCheckpoint, u64, DateTime<Utc>) -> SessionCheckpoint,
{
    let Some(mut record) = store.get(session_id)? else {
        return Ok(false);
    };

    let saved_at = Utc::now();
    let seq = record.summary.last_checkpoint_seq + 1;
    let base = checkpoints
        .load_latest(session_id, Default::default())
        .await?
        .unwrap_or(SessionCheckpoint {
            session_id,
            seq,
            saved_at,
            current_phase: None,
            active_tool_name: None,
            worktree: None,
            scheduler_runtime: None,
        });
    let mut checkpoint = mutator(base, seq, saved_at);
    checkpoint.session_id = session_id;
    checkpoint.seq = seq;
    checkpoint.saved_at = saved_at;

    checkpoints.save(checkpoint).await?;
    record.summary.last_checkpoint_seq = seq;
    record.refs.checkpoint_ref = Some(format!("checkpoints/{session_id}.jsonl"));
    record.updated_at = saved_at;
    store.put(&record)?;
    Ok(true)
}

fn is_terminal_status(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
    )
}

fn lease_for_status(
    coordinator: &dyn SessionCoordinator,
    session_id: Uuid,
    worker_id: &str,
    status: SessionStatus,
    existing: &SessionLease,
) -> SessionLease {
    if is_terminal_status(status) {
        if let Some(lease_token) = existing.lease_token {
            let _ = coordinator.release_lease(session_id, lease_token);
        }
        SessionLease::default()
    } else {
        coordinator
            .acquire_lease(session_id, worker_id, std::time::Duration::from_secs(30))
            .map(|grant| SessionLease {
                worker_id: Some(grant.worker_id),
                lease_token: Some(grant.lease_token),
                lease_expires_at: Some(grant.lease_expires_at),
            })
            .unwrap_or_else(|_| existing.clone())
    }
}

pub fn update_session_status(
    store: &dyn SessionStore,
    coordinator: &dyn SessionCoordinator,
    session_id: Uuid,
    worker_id: &str,
    status: SessionStatus,
) -> Result<bool> {
    let Some(mut record) = store.get(session_id)? else {
        return Ok(false);
    };
    let now = Utc::now();
    record.lease = lease_for_status(coordinator, session_id, worker_id, status, &record.lease);
    record.status = status;
    record.version += 1;
    record.updated_at = now;
    record.completed_at = if is_terminal_status(status) {
        Some(now)
    } else {
        None
    };
    store.put(&record)?;
    Ok(true)
}

pub async fn update_checkpoint_phase(
    store: &dyn SessionStore,
    checkpoints: &dyn CheckpointStore,
    session_id: Uuid,
    phase: ExecutionPhase,
) -> Result<bool> {
    update_session_checkpoint(store, checkpoints, session_id, |mut checkpoint, _, _| {
        checkpoint.current_phase = Some(phase);
        checkpoint
    })
    .await
}

pub async fn update_checkpoint_with_worktree(
    store: &dyn SessionStore,
    checkpoints: &dyn CheckpointStore,
    session_id: Uuid,
    phase: ExecutionPhase,
    worktree: Option<WorktreeSnapshot>,
) -> Result<bool> {
    update_session_checkpoint(store, checkpoints, session_id, |mut checkpoint, _, _| {
        checkpoint.current_phase = Some(phase);
        checkpoint.worktree = worktree;
        checkpoint
    })
    .await
}

pub async fn transition_session_state(
    store: &dyn SessionStore,
    checkpoints: &dyn CheckpointStore,
    coordinator: &dyn SessionCoordinator,
    session_id: Uuid,
    worker_id: &str,
    phase: Option<ExecutionPhase>,
    status: SessionStatus,
) -> Result<bool> {
    if let Some(phase) = phase {
        let _ = update_checkpoint_phase(store, checkpoints, session_id, phase).await?;
    }
    update_session_status(store, coordinator, session_id, worker_id, status)
}
