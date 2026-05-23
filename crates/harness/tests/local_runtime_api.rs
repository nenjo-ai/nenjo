#![cfg(feature = "local-runtime")]

use std::sync::Arc;

use chrono::Utc;
use nenjo_harness::{
    FileSessionRuntime, FileSessionStores, Harness, Manifest, ModelProviderFactory,
    NoopToolFactory, Provider,
};
use nenjo_sessions::{
    SessionKind, SessionRefs, SessionRuntimeEvent, SessionStatus, SessionStore,
    SessionTranscriptEventPayload, SessionTranscriptRecord, SessionUpsert, TokenUsage, TraceEvent,
    TracePhase, TranscriptQuery,
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

#[tokio::test]
async fn file_session_runtime_persists_sessions_transcripts_and_traces() {
    let dir = tempdir().unwrap();
    let stores = FileSessionStores::new(dir.path());
    let records = stores.records.clone();
    let harness = Harness::builder(test_provider().await)
        .with_session_runtime(FileSessionRuntime::new(stores))
        .build();
    let session_id = Uuid::new_v4();

    harness
        .sessions()
        .record(SessionRuntimeEvent::SessionUpsert(SessionUpsert {
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
        }))
        .await
        .unwrap();

    harness
        .sessions()
        .record(SessionRuntimeEvent::Transcript(SessionTranscriptRecord {
            session_id,
            turn_id: None,
            payload: SessionTranscriptEventPayload::TurnCompleted {
                final_output: "done".to_string(),
            },
        }))
        .await
        .unwrap();

    harness
        .sessions()
        .record(SessionRuntimeEvent::Trace(TraceEvent {
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
        }))
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
