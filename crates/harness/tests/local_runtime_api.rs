#![cfg(feature = "local-runtime")]

use std::sync::Arc;

use chrono::Utc;
use nenjo_harness::{
    Buffered, FileSessionRuntime, FileSessionStores, Harness, HarnessChatEvent, Manifest,
    ModelProviderFactory, NoopToolFactory, Provider, Streaming,
};
use nenjo_sessions::{
    SessionKind, SessionOwnerKind, SessionRefs, SessionRuntime, SessionRuntimeEvent, SessionStatus,
    SessionStore, SessionTranscriptAppend, SessionTranscriptEvent, SessionTranscriptEventPayload,
    SessionTranscriptRecord, SessionUpsert, TokenUsage, TraceEvent, TracePhase, TranscriptQuery,
    TranscriptStore,
};
use tempfile::tempdir;
use uuid::Uuid;

struct TestModelProvider;

#[async_trait::async_trait]
impl nenjo_harness::ModelProvider for TestModelProvider {
    async fn chat(
        &self,
        _request: nenjo_models::ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> anyhow::Result<nenjo_models::ChatResponse> {
        Ok(nenjo_models::ChatResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
            provider_tool_calls: vec![],
            usage: nenjo_models::TokenUsage::default(),
            finish_reason: nenjo_models::FinishReason::Stop,
        })
    }
}

struct TestModelFactory;

impl ModelProviderFactory for TestModelFactory {
    fn create(
        &self,
        _provider_name: &str,
    ) -> anyhow::Result<Arc<dyn nenjo_harness::ModelProvider>> {
        Ok(Arc::new(TestModelProvider))
    }
}

async fn test_provider()
-> Provider<TestModelFactory, NoopToolFactory, nenjo::provider::builder::NoMemory> {
    Provider::builder()
        .with_manifest(Manifest::default())
        .with_model_factory(TestModelFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap()
}

async fn chat_test_provider()
-> Provider<TestModelFactory, NoopToolFactory, nenjo::provider::builder::NoMemory> {
    let model = nenjo::manifest::ModelManifest {
        slug: nenjo::manifest::model_manifest_slug("mock", "mock"),
        name: "mock".into(),
        description: None,
        model: "mock".into(),
        model_provider: "mock".into(),
        temperature: Some(0.0),
        context_window: None,
        estimated_bytes_per_token: None,
        base_url: None,
        native_tools: Vec::new(),
        capabilities: Vec::new(),
        input_modalities: Vec::new(),
        output_modalities: Vec::new(),
        execution_modes: Vec::new(),
    };
    let agent = nenjo::manifest::AgentManifest {
        name: "system".into(),
        slug: nenjo::Slug::derive("system"),
        description: None,
        prompt_config: nenjo::manifest::PromptConfig::default(),
        color: None,
        model: Some(nenjo::manifest::model_manifest_slug(
            &model.model_provider,
            &model.model,
        )),
        domains: Vec::new(),
        platform_scopes: Vec::new(),
        mcp_servers: Vec::new(),
        abilities: Vec::new(),
        script_tools: Vec::new(),
        media: Vec::new(),
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };
    Provider::builder()
        .with_manifest(Manifest {
            agents: vec![agent],
            models: vec![model],
            ..Default::default()
        })
        .with_model_factory(TestModelFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn file_session_runtime_persists_sessions_transcripts_and_traces() {
    let dir = tempdir().unwrap();
    let stores = FileSessionStores::new(dir.path());
    let records = stores.records.clone();
    let harness = Harness::builder(test_provider().await)
        .with_session_runtime(FileSessionRuntime::new(stores))
        .build();
    let session_id = Uuid::new_v4();
    let lease = harness
        .sessions()
        .acquire_lease(session_id, "test", SessionOwnerKind::Chat)
        .await
        .unwrap();

    harness
        .sessions()
        .record_batch(
            &lease,
            vec![SessionRuntimeEvent::SessionUpsert(SessionUpsert {
                session_id,
                kind: SessionKind::Chat,
                status: SessionStatus::Active,
                agent: Some("test_agent".to_string()),
                project: Some("demo_project".to_string()),
                task_id: None,
                routine: None,
                execution_run_id: None,
                parent_session_id: None,
                lease: None,
                memory_namespace: Some("agent_test_core".to_string()),
                refs: SessionRefs::default(),
                metadata: serde_json::Value::Null,
            })],
        )
        .await
        .unwrap();

    harness
        .sessions()
        .record_batch(
            &lease,
            vec![SessionRuntimeEvent::Transcript(SessionTranscriptRecord {
                session_id,
                turn_id: None,
                payload: SessionTranscriptEventPayload::TurnCompleted {
                    final_output: "done".to_string(),
                },
            })],
        )
        .await
        .unwrap();

    harness
        .sessions()
        .record_batch(
            &lease,
            vec![SessionRuntimeEvent::Trace(TraceEvent {
                session_id,
                turn_id: None,
                recorded_at: Utc::now(),
                phase: TracePhase::Completed,
                agent_id: Some(Uuid::new_v4()),
                agent_name: Some("agent".to_string()),
                tool_name: None,
                parent_tool_name: None,
                ability_name: None,
                target_agent_id: None,
                target_agent_name: None,
                success: Some(true),
                usage: TokenUsage {
                    input_tokens: 3,
                    output_tokens: 4,
                },
                preview: Some("done".to_string()),
                task_input: None,
                final_output: Some("done".to_string()),
                tool_args: None,
                error_preview: None,
                metadata: serde_json::Value::Null,
            })],
        )
        .await
        .unwrap();

    let record = records.get(session_id).unwrap().expect("session persisted");
    let transcript_ref = format!("transcripts/{session_id}.jsonl");
    let trace_ref = format!("traces/{session_id}.jsonl");
    assert_eq!(
        record.refs.transcript_ref.as_deref(),
        Some(transcript_ref.as_str())
    );
    assert_eq!(record.refs.trace_ref.as_deref(), Some(trace_ref.as_str()));
    assert_eq!(record.project.as_deref(), Some("demo_project"));
    assert_eq!(record.agent.as_deref(), Some("test_agent"));

    let transcript = harness
        .sessions()
        .read_transcript(session_id, TranscriptQuery::default())
        .await
        .unwrap();
    assert_eq!(transcript.len(), 1);
}

#[tokio::test]
async fn file_session_runtime_allows_sequential_chat_turns_in_same_session() {
    let dir = tempdir().unwrap();
    let stores = FileSessionStores::new(dir.path());
    let harness = Harness::builder(chat_test_provider().await)
        .with_session_runtime(FileSessionRuntime::new(stores))
        .build();
    let session_id = Uuid::new_v4();

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        harness
            .chat(
                nenjo_harness::ChatRequest::new("system", "first").with_session(session_id),
                Buffered,
            )
            .await?
            .output()
            .await
    })
    .await
    .expect("first chat turn should not hang")
    .expect("first chat turn succeeds");
    assert_eq!(first.text, "ok");

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        harness
            .chat(
                nenjo_harness::ChatRequest::new("system", "second").with_session(session_id),
                Buffered,
            )
            .await?
            .output()
            .await
    })
    .await
    .expect("second chat turn should not hang")
    .expect("second chat turn succeeds");
    assert_eq!(second.text, "ok");
}

#[tokio::test]
async fn file_session_runtime_allows_sequential_streamed_chat_turns_in_same_session() {
    let dir = tempdir().unwrap();
    let stores = FileSessionStores::new(dir.path());
    let harness = Harness::builder(chat_test_provider().await)
        .with_session_runtime(FileSessionRuntime::new(stores))
        .build();
    let session_id = Uuid::new_v4();

    for message in ["first", "second"] {
        let mut stream = harness
            .chat(
                nenjo_harness::ChatRequest::new("system", message).with_session(session_id),
                Streaming,
            )
            .await
            .expect("chat stream starts");
        let mut saw_done = false;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(event) = stream.recv().await {
                if matches!(
                    event,
                    HarnessChatEvent::Turn {
                        event: nenjo::TurnEvent::Done { .. },
                        ..
                    }
                ) {
                    saw_done = true;
                }
            }
        })
        .await
        .expect("chat event stream should close after done");
        assert!(saw_done);

        let output = tokio::time::timeout(std::time::Duration::from_secs(2), stream.output())
            .await
            .expect("stream output should not hang")
            .expect("chat turn succeeds");
        assert_eq!(output.text, "ok");
    }
}

#[tokio::test]
async fn chat_stream_replaces_stale_active_execution_before_preparing_next_turn() {
    let dir = tempdir().unwrap();
    let stores = FileSessionStores::new(dir.path());
    let harness = Harness::builder(chat_test_provider().await)
        .with_session_runtime(FileSessionRuntime::new(stores))
        .build();
    let session_id = Uuid::new_v4();

    let _stale = harness
        .chat(
            nenjo_harness::ChatRequest::new("system", "first").with_session(session_id),
            Streaming,
        )
        .await
        .expect("first chat stream starts");

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        harness
            .chat(
                nenjo_harness::ChatRequest::new("system", "second").with_session(session_id),
                Buffered,
            )
            .await?
            .output()
            .await
    })
    .await
    .expect("second chat turn should replace stale active execution before preparation")
    .expect("second chat turn succeeds");

    assert_eq!(second.text, "ok");
}

#[tokio::test]
async fn file_transcript_store_appends_without_rewriting_existing_events() {
    let dir = tempdir().unwrap();
    let store = FileSessionStores::new(dir.path()).transcripts;
    let session_id = Uuid::new_v4();

    for content in ["one", "two"] {
        store
            .append(SessionTranscriptEvent {
                session_id,
                seq: 0,
                recorded_at: Utc::now(),
                turn_id: None,
                payload: SessionTranscriptEventPayload::ConversationMessage {
                    message: nenjo_models::ConversationMessage::user(content),
                },
            })
            .await
            .unwrap();
    }

    let events = store
        .read(session_id, TranscriptQuery::default())
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[1].seq, 2);
}

async fn seeded_session(dir: &tempfile::TempDir, session_id: Uuid) -> FileSessionRuntime {
    let stores = FileSessionStores::new(dir.path());
    let runtime = FileSessionRuntime::new(stores);
    let grant = runtime
        .acquire_session_lease(nenjo_sessions::SessionLeaseRequest {
            session_id,
            worker_id: "test".to_string(),
            owner_kind: SessionOwnerKind::Chat,
            ttl: std::time::Duration::from_secs(30),
        })
        .await
        .unwrap();
    runtime
        .record_batch(
            &grant,
            vec![SessionRuntimeEvent::SessionUpsert(SessionUpsert {
                session_id,
                kind: SessionKind::Chat,
                status: SessionStatus::Active,
                agent: Some("test_agent".to_string()),
                project: None,
                task_id: None,
                routine: None,
                execution_run_id: None,
                parent_session_id: None,
                lease: None,
                memory_namespace: None,
                refs: SessionRefs::default(),
                metadata: serde_json::Value::Null,
            })],
        )
        .await
        .unwrap();
    runtime.release_session_lease(grant).await.unwrap();
    runtime
}

#[tokio::test]
async fn first_user_message_is_recorded_in_session_summary() {
    let dir = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let runtime = seeded_session(&dir, session_id).await;

    let grant = runtime
        .acquire_session_lease(nenjo_sessions::SessionLeaseRequest {
            session_id,
            worker_id: "test".to_string(),
            owner_kind: SessionOwnerKind::Chat,
            ttl: std::time::Duration::from_secs(30),
        })
        .await
        .unwrap();
    let long_content = "explain ".repeat(80);
    runtime
        .record_batch(
            &grant,
            vec![SessionRuntimeEvent::TranscriptAppend(
                SessionTranscriptAppend {
                    session_id,
                    turn_id: None,
                    payload: SessionTranscriptEventPayload::ConversationMessage {
                        message: nenjo_models::ConversationMessage::user(long_content.clone()),
                    },
                    transcript_state: nenjo_sessions::TranscriptState::MidTurn,
                },
            )],
        )
        .await
        .unwrap();
    runtime.release_session_lease(grant).await.unwrap();

    let record = runtime.get_session(session_id).await.unwrap().unwrap();
    let first = record
        .summary
        .first_user_prompt
        .expect("first user prompt recorded");
    assert_eq!(
        first.chars().count(),
        nenjo_sessions::SessionSummary::FIRST_USER_PROMPT_MAX_CHARS
    );
    assert!(long_content.starts_with(&first));
}

#[tokio::test]
async fn first_user_prompt_ignores_non_user_and_subsequent_messages() {
    let dir = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let runtime = seeded_session(&dir, session_id).await;

    let grant = runtime
        .acquire_session_lease(nenjo_sessions::SessionLeaseRequest {
            session_id,
            worker_id: "test".to_string(),
            owner_kind: SessionOwnerKind::Chat,
            ttl: std::time::Duration::from_secs(30),
        })
        .await
        .unwrap();
    runtime
        .record_batch(
            &grant,
            vec![
                SessionRuntimeEvent::TranscriptAppend(SessionTranscriptAppend {
                    session_id,
                    turn_id: None,
                    payload: SessionTranscriptEventPayload::ConversationMessage {
                        message: nenjo_models::ConversationMessage::system("system prompt"),
                    },
                    transcript_state: nenjo_sessions::TranscriptState::MidTurn,
                }),
                SessionRuntimeEvent::TranscriptAppend(SessionTranscriptAppend {
                    session_id,
                    turn_id: None,
                    payload: SessionTranscriptEventPayload::ConversationMessage {
                        message: nenjo_models::ConversationMessage::user("the real question"),
                    },
                    transcript_state: nenjo_sessions::TranscriptState::MidTurn,
                }),
                SessionRuntimeEvent::TranscriptAppend(SessionTranscriptAppend {
                    session_id,
                    turn_id: None,
                    payload: SessionTranscriptEventPayload::ConversationMessage {
                        message: nenjo_models::ConversationMessage::user("second question"),
                    },
                    transcript_state: nenjo_sessions::TranscriptState::MidTurn,
                }),
            ],
        )
        .await
        .unwrap();
    runtime.release_session_lease(grant).await.unwrap();

    let record = runtime.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(
        record.summary.first_user_prompt.as_deref(),
        Some("the real question")
    );
}

#[tokio::test]
async fn set_session_title_bounds_and_persists() {
    let dir = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let runtime = seeded_session(&dir, session_id).await;

    let long_title = "a very long session title that keeps going and going".repeat(6);
    let applied = runtime
        .set_session_title(session_id, &long_title)
        .await
        .unwrap();
    assert!(applied);
    let record = runtime.get_session(session_id).await.unwrap().unwrap();
    let stored = record.summary.title.clone().expect("title recorded");
    assert_eq!(
        stored.chars().count(),
        nenjo_sessions::SessionSummary::TITLE_MAX_CHARS
    );
    assert!(long_title.starts_with(&stored));

    // Missing sessions report not-applied rather than erroring.
    let missing = runtime
        .set_session_title(Uuid::new_v4(), "nope")
        .await
        .unwrap();
    assert!(!missing);
}

async fn append_message(
    runtime: &FileSessionRuntime,
    session_id: Uuid,
    payload: SessionTranscriptEventPayload,
) {
    let grant = runtime
        .acquire_session_lease(nenjo_sessions::SessionLeaseRequest {
            session_id,
            worker_id: "test".to_string(),
            owner_kind: SessionOwnerKind::Chat,
            ttl: std::time::Duration::from_secs(30),
        })
        .await
        .unwrap();
    runtime
        .record_batch(
            &grant,
            vec![SessionRuntimeEvent::TranscriptAppend(
                SessionTranscriptAppend {
                    session_id,
                    turn_id: None,
                    payload,
                    transcript_state: nenjo_sessions::TranscriptState::MidTurn,
                },
            )],
        )
        .await
        .unwrap();
    runtime.release_session_lease(grant).await.unwrap();
}

fn search_service(runtime: FileSessionRuntime) -> nenjo_harness::SessionSearchService {
    nenjo_harness::SessionSearchService::new(Arc::new(runtime))
}

#[tokio::test]
async fn search_corpus_truncates_to_the_most_recent_events_per_session() {
    let dir = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let runtime = seeded_session(&dir, session_id).await;

    let total = 510;
    let kept = nenjo_harness::MAX_EVENTS_PER_SESSION;
    for seq in 1..=total {
        append_message(
            &runtime,
            session_id,
            SessionTranscriptEventPayload::ConversationMessage {
                message: nenjo_models::ConversationMessage::user(format!("uq{seq}x filler token")),
            },
        )
        .await;
    }

    let (docs, version) = {
        use nenjo_harness::SessionSearchBackend;
        runtime.search_corpus().await.unwrap()
    };
    // Summary doc + exactly the most recent `kept` events.
    assert_eq!(docs.len(), 1 + kept);
    assert_eq!(docs.iter().filter(|doc| doc.seq > 0).count(), kept);
    assert_eq!(version.session_count, 1);
    let min_event_seq = docs
        .iter()
        .filter(|doc| doc.seq > 0)
        .map(|doc| doc.seq)
        .min();
    assert_eq!(min_event_seq, Some((total - kept + 1) as u64));

    let service = search_service(runtime);
    // A truncated event's unique token is gone from the corpus.
    let hits = service
        .search(
            "uq2x",
            &nenjo_harness::SessionSearchFilter::default(),
            10,
            true,
        )
        .await
        .unwrap()
        .expect("index search");
    assert!(hits.is_empty(), "truncated events must not be searchable");

    // A retained event is searchable and reports its real seq.
    let hits = service
        .search(
            "uq505x",
            &nenjo_harness::SessionSearchFilter::default(),
            10,
            true,
        )
        .await
        .unwrap()
        .expect("index search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].seq, 505);
    assert_eq!(hits[0].role, "user");

    // The first event was truncated, but its text survives in the session
    // summary through first_user_prompt (designed behavior).
    let hits = service
        .search(
            "uq1x",
            &nenjo_harness::SessionSearchFilter::default(),
            10,
            true,
        )
        .await
        .unwrap()
        .expect("index search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].seq, 0);
    assert_eq!(hits[0].role, "summary");
}

#[tokio::test]
async fn search_indexes_tool_results_and_session_titles() {
    let dir = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let runtime = seeded_session(&dir, session_id).await;

    append_message(
        &runtime,
        session_id,
        SessionTranscriptEventPayload::ToolResult {
            parent_tool_name: None,
            tool_name: "read".to_string(),
            success: true,
            output_preview: Some("the magic word is lantern".to_string()),
            error_preview: None,
        },
    )
    .await;
    assert!(
        runtime
            .set_session_title(session_id, "Quarterly review notes")
            .await
            .unwrap()
    );

    let service = search_service(runtime);
    let hits = service
        .search(
            "lantern",
            &nenjo_harness::SessionSearchFilter::default(),
            10,
            true,
        )
        .await
        .unwrap()
        .expect("index search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].role, "tool");
    assert!(hits[0].excerpt.contains("lantern"));

    // Title-set sessions are findable by title terms in the summary doc.
    let hits = service
        .search(
            "quarterly review",
            &nenjo_harness::SessionSearchFilter::default(),
            10,
            true,
        )
        .await
        .unwrap()
        .expect("index search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].seq, 0);
    assert_eq!(hits[0].role, "summary");
}

#[tokio::test]
async fn deleted_sessions_leave_the_search_corpus() {
    let dir = tempdir().unwrap();
    let kept_session = Uuid::new_v4();
    let removed_session = Uuid::new_v4();
    let runtime = seeded_session(&dir, kept_session).await;
    // Seed a second session in the same store through a fresh lease.
    {
        let grant = runtime
            .acquire_session_lease(nenjo_sessions::SessionLeaseRequest {
                session_id: removed_session,
                worker_id: "test".to_string(),
                owner_kind: SessionOwnerKind::Chat,
                ttl: std::time::Duration::from_secs(30),
            })
            .await
            .unwrap();
        runtime
            .record_batch(
                &grant,
                vec![SessionRuntimeEvent::SessionUpsert(SessionUpsert {
                    session_id: removed_session,
                    kind: SessionKind::Chat,
                    status: SessionStatus::Active,
                    agent: Some("test_agent".to_string()),
                    project: None,
                    task_id: None,
                    routine: None,
                    execution_run_id: None,
                    parent_session_id: None,
                    lease: None,
                    memory_namespace: None,
                    refs: SessionRefs::default(),
                    metadata: serde_json::Value::Null,
                })],
            )
            .await
            .unwrap();
        runtime.release_session_lease(grant).await.unwrap();
    }
    append_message(
        &runtime,
        kept_session,
        SessionTranscriptEventPayload::ConversationMessage {
            message: nenjo_models::ConversationMessage::user(
                "the deletion marker phrase".to_string(),
            ),
        },
    )
    .await;
    append_message(
        &runtime,
        removed_session,
        SessionTranscriptEventPayload::ConversationMessage {
            message: nenjo_models::ConversationMessage::user(
                "the deletion marker phrase".to_string(),
            ),
        },
    )
    .await;

    let service = search_service(runtime.clone());
    let hits = service
        .search(
            "deletion marker phrase",
            &nenjo_harness::SessionSearchFilter::default(),
            10,
            true,
        )
        .await
        .unwrap()
        .expect("index search");
    assert_eq!(
        hits.iter()
            .map(|hit| hit.session_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2
    );

    runtime.delete_session(removed_session).await.unwrap();

    let hits = service
        .search(
            "deletion marker phrase",
            &nenjo_harness::SessionSearchFilter::default(),
            10,
            true,
        )
        .await
        .unwrap()
        .expect("index search after delete");
    assert!(
        hits.iter().all(|hit| hit.session_id == kept_session),
        "deleted session must disappear from search results"
    );
}
