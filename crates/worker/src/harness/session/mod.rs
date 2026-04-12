use anyhow::Result;
use chrono::{DateTime, Utc};
use nenjo::AgentBuilder;
use nenjo_sessions::{
    ExecutionPhase, SessionCheckpoint, SessionContentStore, SessionCoordinator, SessionKind,
    SessionLease, SessionStatus, SessionStore, WorktreeSnapshot,
};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

pub mod local_content;
pub mod local_coordinator;
pub mod local_store;

pub fn session_memory_scope(
    store: &dyn SessionStore,
    session_id: Uuid,
) -> Option<nenjo::memory::MemoryScope> {
    let record = store.get(session_id).ok().flatten()?;
    let namespace = record.refs.memory_namespace?;
    nenjo::memory::MemoryScope::from_namespace(&namespace)
}

pub fn session_memory_namespace(store: &dyn SessionStore, session_id: Uuid) -> Option<String> {
    let record = store.get(session_id).ok().flatten()?;
    record.refs.memory_namespace
}

pub fn apply_session_memory_scope(
    builder: AgentBuilder,
    store: &dyn SessionStore,
    session_id: Uuid,
) -> AgentBuilder {
    match session_memory_scope(store, session_id) {
        Some(scope) => builder.with_memory_scope(scope),
        None => builder,
    }
}

pub fn read_json_blob<T>(store: &dyn SessionContentStore, key: &str) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let Some(bytes) = store.read_blob(key)? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub fn write_json_blob<T>(store: &dyn SessionContentStore, key: &str, value: &T) -> Result<()>
where
    T: Serialize,
{
    let body = serde_json::to_vec(value)?;
    store.write_blob(key, &body)
}

pub fn write_checkpoint(
    store: &dyn SessionContentStore,
    key: &str,
    checkpoint: &SessionCheckpoint,
) -> Result<()> {
    write_json_blob(store, key, checkpoint)
}

pub fn update_session_checkpoint<F>(
    store: &dyn SessionStore,
    content: &dyn SessionContentStore,
    session_id: Uuid,
    mutator: F,
) -> Result<bool>
where
    F: FnOnce(SessionCheckpoint, u64, DateTime<Utc>) -> SessionCheckpoint,
{
    let Some(mut record) = store.get(session_id)? else {
        return Ok(false);
    };
    let Some(checkpoint_ref) = record.refs.checkpoint_ref.clone() else {
        return Ok(false);
    };

    let saved_at = Utc::now();
    let seq = record.summary.last_checkpoint_seq + 1;
    let base = read_json_blob::<SessionCheckpoint>(content, &checkpoint_ref)?.unwrap_or(
        SessionCheckpoint {
            session_id,
            seq,
            saved_at,
            current_phase: None,
            active_tool_name: None,
            worktree: None,
            scheduler_runtime: None,
        },
    );
    let mut checkpoint = mutator(base, seq, saved_at);
    checkpoint.session_id = session_id;
    checkpoint.seq = seq;
    checkpoint.saved_at = saved_at;

    write_checkpoint(content, &checkpoint_ref, &checkpoint)?;
    record.summary.last_checkpoint_seq = seq;
    record.updated_at = saved_at;
    store.put(&record)?;
    Ok(true)
}

pub fn is_terminal_status(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
    )
}

pub fn lease_for_status(
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
    if is_terminal_status(status) {
        record.completed_at = Some(now);
    } else {
        record.completed_at = None;
    }
    store.put(&record)?;
    Ok(true)
}

pub fn update_checkpoint_phase(
    store: &dyn SessionStore,
    content: &dyn SessionContentStore,
    session_id: Uuid,
    phase: ExecutionPhase,
) -> Result<bool> {
    update_session_checkpoint(store, content, session_id, |mut checkpoint, _, _| {
        checkpoint.current_phase = Some(phase);
        checkpoint
    })
}

pub fn update_checkpoint_with_worktree(
    store: &dyn SessionStore,
    content: &dyn SessionContentStore,
    session_id: Uuid,
    phase: ExecutionPhase,
    worktree: Option<WorktreeSnapshot>,
) -> Result<bool> {
    update_session_checkpoint(store, content, session_id, |mut checkpoint, _, _| {
        checkpoint.current_phase = Some(phase);
        checkpoint.worktree = worktree;
        checkpoint
    })
}

pub fn transition_session_state(
    store: &dyn SessionStore,
    content: &dyn SessionContentStore,
    coordinator: &dyn SessionCoordinator,
    session_id: Uuid,
    worker_id: &str,
    phase: Option<ExecutionPhase>,
    status: SessionStatus,
) -> Result<bool> {
    if let Some(phase) = phase {
        let _ = update_checkpoint_phase(store, content, session_id, phase)?;
    }
    update_session_status(store, coordinator, session_id, worker_id, status)
}

pub fn reconcile_recoverable_session(
    store: &dyn SessionStore,
    content: &dyn SessionContentStore,
    coordinator: &dyn SessionCoordinator,
    session_id: Uuid,
) -> Result<bool> {
    let Some(mut record) = store.get(session_id)? else {
        return Ok(false);
    };
    if !matches!(record.kind, SessionKind::Chat | SessionKind::Task)
        || !matches!(record.status, SessionStatus::Active | SessionStatus::Paused)
    {
        return Ok(false);
    }

    let checkpoint_phase = record
        .refs
        .checkpoint_ref
        .as_deref()
        .map(|checkpoint_ref| read_json_blob::<SessionCheckpoint>(content, checkpoint_ref))
        .transpose()?
        .flatten()
        .and_then(|checkpoint| checkpoint.current_phase);

    if let Some(lease_token) = record.lease.lease_token {
        let _ = coordinator.release_lease(session_id, lease_token);
    }

    record.status = SessionStatus::Waiting;
    record.lease = SessionLease::default();
    record.version += 1;
    record.updated_at = Utc::now();
    record.completed_at = None;
    record.summary.last_progress_message = Some(match checkpoint_phase {
        Some(ExecutionPhase::Preparing) => "recoverable from preparing checkpoint".to_string(),
        Some(ExecutionPhase::CallingModel) => "recoverable from model call checkpoint".to_string(),
        Some(ExecutionPhase::ExecutingTools) => {
            "recoverable from tool execution checkpoint".to_string()
        }
        Some(ExecutionPhase::Waiting) => "recoverable from waiting checkpoint".to_string(),
        Some(ExecutionPhase::Finalizing) => "recoverable from finalizing checkpoint".to_string(),
        None => "recoverable from persisted session state".to_string(),
    });
    store.put(&record)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        read_json_blob, reconcile_recoverable_session, transition_session_state,
        update_checkpoint_phase, update_checkpoint_with_worktree, write_json_blob,
    };
    use crate::harness::session::local_content::FileSessionContentStore;
    use crate::harness::session::local_coordinator::LocalSessionCoordinator;
    use crate::harness::session::local_store::FileSessionStore;
    use chrono::Utc;
    use nenjo_sessions::{
        ExecutionPhase, SessionCheckpoint, SessionKind, SessionRecord, SessionRefs, SessionStatus,
        SessionStore, SessionSummary, WorktreeSnapshot,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

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
                history_ref: None,
                trace_ref: None,
                checkpoint_ref: Some(format!("checkpoints/{session_id}.json")),
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

    #[test]
    fn checkpoint_updates_advance_sequence_and_preserve_worktree() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("sessions").as_path());
        let content = FileSessionContentStore::new(dir.path().join("content").as_path());
        let session_id = Uuid::new_v4();

        store
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        assert!(
            update_checkpoint_phase(&store, &content, session_id, ExecutionPhase::Preparing)
                .unwrap()
        );

        let worktree = WorktreeSnapshot {
            repo_dir: "/repo".to_string(),
            work_dir: "/repo/worktree".to_string(),
            branch: "feature/test".to_string(),
            target_branch: Some("main".to_string()),
        };
        assert!(
            update_checkpoint_with_worktree(
                &store,
                &content,
                session_id,
                ExecutionPhase::Finalizing,
                Some(worktree.clone()),
            )
            .unwrap()
        );

        let key = format!("checkpoints/{session_id}.json");
        let checkpoint: SessionCheckpoint = read_json_blob(&content, &key)
            .unwrap()
            .expect("checkpoint should exist");
        assert_eq!(checkpoint.seq, 2);
        assert_eq!(checkpoint.current_phase, Some(ExecutionPhase::Finalizing));
        assert_eq!(checkpoint.worktree.unwrap().branch, worktree.branch);

        let record = store.get(session_id).unwrap().unwrap();
        assert_eq!(record.summary.last_checkpoint_seq, 2);
    }

    #[test]
    fn transition_session_state_updates_phase_and_terminal_status() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("sessions").as_path());
        let content = FileSessionContentStore::new(dir.path().join("content").as_path());
        let coordinator = LocalSessionCoordinator::new();
        let session_id = Uuid::new_v4();

        store
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        assert!(
            transition_session_state(
                &store,
                &content,
                &coordinator,
                session_id,
                "worker-1",
                Some(ExecutionPhase::Waiting),
                SessionStatus::Cancelled,
            )
            .unwrap()
        );

        let record = store.get(session_id).unwrap().unwrap();
        assert_eq!(record.status, SessionStatus::Cancelled);
        assert!(record.completed_at.is_some());
        assert!(record.lease.lease_token.is_none());

        let key = format!("checkpoints/{session_id}.json");
        let checkpoint: SessionCheckpoint = read_json_blob(&content, &key)
            .unwrap()
            .expect("checkpoint should exist");
        assert_eq!(checkpoint.current_phase, Some(ExecutionPhase::Waiting));
    }

    #[test]
    fn reconcile_recoverable_session_moves_task_to_waiting() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("sessions").as_path());
        let content = FileSessionContentStore::new(dir.path().join("content").as_path());
        let coordinator = LocalSessionCoordinator::new();
        let session_id = Uuid::new_v4();
        let record = test_record(session_id, SessionStatus::Active);
        let checkpoint_key = record.refs.checkpoint_ref.clone().unwrap();

        store.put(&record).unwrap();
        write_json_blob(
            &content,
            &checkpoint_key,
            &SessionCheckpoint {
                session_id,
                seq: 1,
                saved_at: Utc::now(),
                current_phase: Some(ExecutionPhase::ExecutingTools),
                active_tool_name: None,
                worktree: None,
                scheduler_runtime: None,
            },
        )
        .unwrap();

        assert!(reconcile_recoverable_session(&store, &content, &coordinator, session_id).unwrap());

        let updated = store.get(session_id).unwrap().unwrap();
        assert_eq!(updated.status, SessionStatus::Waiting);
        assert_eq!(
            updated.summary.last_progress_message.as_deref(),
            Some("recoverable from tool execution checkpoint")
        );
        assert!(updated.completed_at.is_none());
    }
}
