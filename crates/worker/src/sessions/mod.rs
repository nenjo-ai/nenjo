//! Worker-local implementations of session runtime traits.
//!
//! These stores are filesystem-backed defaults for embedded/local hosts. The
//! generic execution session helpers remain in the `nenjo-harness` crate.

mod coordinator;
mod event_store;
#[allow(dead_code)]
mod helpers;
mod record_store;
mod runtime;

pub use coordinator::LocalSessionCoordinator;
pub use event_store::{
    FileCheckpointStore, FileTraceStore, FileTranscriptStore, WorkerSessionStores,
};
pub use record_store::FileSessionStore;
pub use runtime::{
    CronSessionRecovery, DomainSessionRecovery, HeartbeatSessionRecovery,
    WorkerSessionRecoveryHandler, WorkerSessionRuntime,
};

#[cfg(test)]
mod tests {
    use crate::sessions::helpers::{
        append_transcript_event, chat_message_to_transcript, load_chat_history, transcript_ref,
        transition_session_state, update_checkpoint_phase, update_checkpoint_with_worktree,
    };
    use crate::sessions::{
        FileCheckpointStore, FileSessionStore, FileTranscriptStore, LocalSessionCoordinator,
        WorkerSessionRuntime, WorkerSessionStores,
    };
    use chrono::Utc;
    use nenjo_models::ChatMessage;
    use nenjo_sessions::{
        CheckpointStore, ExecutionPhase, SessionCheckpoint, SessionKind, SessionRecord,
        SessionRefs, SessionStatus, SessionStore, SessionSummary, SessionTranscriptEventPayload,
        TranscriptState, WorktreeSnapshot,
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
                transcript_ref: Some(transcript_ref(session_id)),
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
        let store = FileSessionStore::new(dir.path().join("sessions").as_path());
        let checkpoints = FileCheckpointStore::new(dir.path().join("checkpoints"));
        let session_id = Uuid::new_v4();

        store
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        assert!(
            update_checkpoint_phase(&store, &checkpoints, session_id, ExecutionPhase::Preparing)
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
            update_checkpoint_with_worktree(
                &store,
                &checkpoints,
                session_id,
                ExecutionPhase::Finalizing,
                Some(worktree.clone()),
            )
            .await
            .unwrap()
        );

        let checkpoint: SessionCheckpoint = checkpoints
            .load_latest(session_id, Default::default())
            .await
            .unwrap()
            .expect("checkpoint should exist");
        assert_eq!(checkpoint.seq, 2);
        assert_eq!(checkpoint.current_phase, Some(ExecutionPhase::Finalizing));
        assert_eq!(checkpoint.worktree.unwrap().branch, worktree.branch);

        let record = store.get(session_id).unwrap().unwrap();
        assert_eq!(record.summary.last_checkpoint_seq, 2);
    }

    #[tokio::test]
    async fn transition_session_state_updates_phase_and_terminal_status() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("sessions").as_path());
        let checkpoints = FileCheckpointStore::new(dir.path().join("checkpoints"));
        let coordinator = LocalSessionCoordinator::new();
        let session_id = Uuid::new_v4();

        store
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        assert!(
            transition_session_state(
                &store,
                &checkpoints,
                &coordinator,
                session_id,
                "worker-1",
                Some(ExecutionPhase::Waiting),
                SessionStatus::Cancelled,
            )
            .await
            .unwrap()
        );

        let record = store.get(session_id).unwrap().unwrap();
        assert_eq!(record.status, SessionStatus::Cancelled);
        assert!(record.completed_at.is_some());
        assert!(record.lease.lease_token.is_none());

        let checkpoint: SessionCheckpoint = checkpoints
            .load_latest(session_id, Default::default())
            .await
            .unwrap()
            .expect("checkpoint should exist");
        assert_eq!(checkpoint.current_phase, Some(ExecutionPhase::Waiting));
    }

    #[tokio::test]
    async fn recover_reconcilable_sessions_moves_task_to_waiting() {
        let dir = tempdir().unwrap();
        let stores = WorkerSessionStores::new(dir.path());
        let store = stores.records.clone();
        let checkpoints = stores.checkpoints.clone();
        let coordinator = LocalSessionCoordinator::new();
        let session_id = Uuid::new_v4();
        let record = test_record(session_id, SessionStatus::Active);

        store.put(&record).unwrap();
        checkpoints
            .save(SessionCheckpoint {
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

        let runtime = WorkerSessionRuntime::new(stores, coordinator, "worker-1");
        struct NoopRecovery;
        #[async_trait::async_trait]
        impl crate::sessions::WorkerSessionRecoveryHandler for NoopRecovery {}

        runtime
            .recover_reconcilable_sessions(&NoopRecovery)
            .await
            .unwrap();

        let updated = store.get(session_id).unwrap().unwrap();
        assert_eq!(updated.status, SessionStatus::Waiting);
        assert_eq!(
            updated.summary.last_progress_message.as_deref(),
            Some("recoverable from tool execution checkpoint")
        );
        assert!(updated.completed_at.is_none());
    }

    #[tokio::test]
    async fn transcript_events_replay_into_chat_history() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("sessions").as_path());
        let transcripts = FileTranscriptStore::new(dir.path().join("transcripts"));
        let session_id = Uuid::new_v4();

        store
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        append_transcript_event(
            &store,
            &transcripts,
            session_id,
            None,
            SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(&ChatMessage::user("first")),
            },
            TranscriptState::MidTurn,
        )
        .await
        .unwrap();

        append_transcript_event(
            &store,
            &transcripts,
            session_id,
            None,
            SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(&ChatMessage::assistant("second")),
            },
            TranscriptState::Clean,
        )
        .await
        .unwrap();

        let history = load_chat_history(&store, &transcripts, session_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "first");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].content, "second");

        let record = store.get(session_id).unwrap().unwrap();
        assert_eq!(
            record.refs.transcript_ref.as_deref(),
            Some(transcript_ref(session_id).as_str())
        );
        assert_eq!(record.summary.last_transcript_seq, 2);
        assert_eq!(record.summary.transcript_state, TranscriptState::Clean);
    }

    #[tokio::test]
    async fn transcript_replay_ignores_non_chat_events_and_preserves_mid_turn_state() {
        let dir = tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("sessions").as_path());
        let transcripts = FileTranscriptStore::new(dir.path().join("transcripts"));
        let session_id = Uuid::new_v4();

        store
            .put(&test_record(session_id, SessionStatus::Active))
            .unwrap();

        append_transcript_event(
            &store,
            &transcripts,
            session_id,
            None,
            SessionTranscriptEventPayload::ChatMessage {
                message: chat_message_to_transcript(&ChatMessage::user("question")),
            },
            TranscriptState::MidTurn,
        )
        .await
        .unwrap();

        append_transcript_event(
            &store,
            &transcripts,
            session_id,
            None,
            SessionTranscriptEventPayload::ToolCalls {
                parent_tool_name: None,
                tool_names: vec!["search_docs".to_string()],
                text_preview: Some("checking docs".to_string()),
            },
            TranscriptState::MidTurn,
        )
        .await
        .unwrap();

        append_transcript_event(
            &store,
            &transcripts,
            session_id,
            None,
            SessionTranscriptEventPayload::ToolResult {
                parent_tool_name: None,
                tool_name: "search_docs".to_string(),
                success: true,
                output_preview: Some("found 3 matches".to_string()),
                error_preview: None,
            },
            TranscriptState::MidTurn,
        )
        .await
        .unwrap();

        let history = load_chat_history(&store, &transcripts, session_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "question");

        let record = store.get(session_id).unwrap().unwrap();
        assert_eq!(record.summary.last_transcript_seq, 3);
        assert_eq!(record.summary.transcript_state, TranscriptState::MidTurn);
    }
}
