#![cfg(feature = "local-runtime")]

use std::sync::Arc;

use chrono::Utc;
use nenjo_harness::{
    FileSessionRuntime, FileSessionStores, Harness, Manifest, ModelProviderFactory,
    NoopToolFactory, Provider,
};
use nenjo_sessions::{
    SessionKind, SessionOwnerKind, SessionRefs, SessionRuntimeEvent, SessionStatus, SessionStore,
    SessionTranscriptChatMessage, SessionTranscriptEvent, SessionTranscriptEventPayload,
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
            text: Some("ok".to_string()),
            tool_calls: Vec::new(),
            provider_tool_calls: vec![],
            usage: nenjo_models::TokenUsage::default(),
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
        base_url: None,
        native_tools: Vec::new(),
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
        heartbeat: None,
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

    let first = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        harness.chat(nenjo_harness::ChatRequest::new("system", "first").with_session(session_id)),
    )
    .await
    .expect("first chat turn should not hang")
    .expect("first chat turn succeeds");
    assert_eq!(first.text, "ok");

    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        harness.chat(nenjo_harness::ChatRequest::new("system", "second").with_session(session_id)),
    )
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
            .chat_stream(
                nenjo_harness::ChatRequest::new("system", message).with_session(session_id),
            )
            .await
            .expect("chat stream starts");
        let mut saw_done = false;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(event) = stream.recv().await {
                if matches!(
                    event,
                    nenjo_harness::HarnessEvent::Turn {
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
        .chat_stream(nenjo_harness::ChatRequest::new("system", "first").with_session(session_id))
        .await
        .expect("first chat stream starts");

    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        harness.chat(nenjo_harness::ChatRequest::new("system", "second").with_session(session_id)),
    )
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
                payload: SessionTranscriptEventPayload::ChatMessage {
                    message: SessionTranscriptChatMessage {
                        role: "user".to_string(),
                        content: content.to_string(),
                    },
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
