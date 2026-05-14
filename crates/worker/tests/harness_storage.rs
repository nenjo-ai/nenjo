use std::sync::Arc;

use chrono::Utc;
use nenjo::manifest::{Manifest, ProjectManifest};
use nenjo_models::{ChatRequest, ChatResponse, TokenUsage};
use nenjo_sessions::{
    CheckpointRecord, SessionCheckpoint, SessionRuntimeEvent, SessionStatus, SessionStore,
    SessionTranscriptChatMessage, SessionTranscriptEventPayload, SessionTranscriptRecord,
    TraceEvent, TracePhase,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use nenjo_harness::handlers::manifest::ManifestStore;
use nenjo_worker::api_client::{DocumentSyncMeta, NenjoClient};
use nenjo_worker::bootstrap::WorkerManifestCache;
use nenjo_worker::sessions::{LocalSessionCoordinator, WorkerSessionRuntime, WorkerSessionStores};

struct TestModelProvider;

#[async_trait::async_trait]
impl nenjo::ModelProvider for TestModelProvider {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some("ok".to_string()),
            tool_calls: vec![],
            usage: TokenUsage::default(),
        })
    }
}

struct TestModelFactory;

impl nenjo::ModelProviderFactory for TestModelFactory {
    fn create(&self, _provider_name: &str) -> anyhow::Result<Arc<dyn nenjo::ModelProvider>> {
        Ok(Arc::new(TestModelProvider))
    }
}

type TestProvider = nenjo::Provider<
    TestModelFactory,
    nenjo::provider::NoopToolFactory,
    nenjo::provider::builder::NoMemory,
>;

async fn provider_with_manifest(manifest: Manifest) -> TestProvider {
    nenjo::Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(TestModelFactory)
        .with_tool_factory(nenjo::provider::NoopToolFactory)
        .build()
        .await
        .expect("provider builds")
}

fn manifest_with_project(project_id: Uuid, slug: &str) -> Manifest {
    Manifest {
        projects: vec![ProjectManifest {
            id: project_id,
            name: "Alpha Project".to_string(),
            slug: slug.to_string(),
            description: None,
            settings: json!({}),
        }],
        ..Default::default()
    }
}

fn document_meta(document_id: Uuid) -> DocumentSyncMeta {
    DocumentSyncMeta {
        id: document_id,
        pack_id: Uuid::from_u128(7),
        slug: "alpha".to_string(),
        filename: "spec.md".to_string(),
        path: Some("domain".to_string()),
        title: Some("Spec".to_string()),
        kind: Some("note".to_string()),
        authority: Some("project".to_string()),
        summary: Some("Project spec".to_string()),
        status: Some("active".to_string()),
        tags: vec!["planning".to_string()],
        aliases: vec![],
        keywords: vec![],
        content_type: "text/markdown".to_string(),
        size_bytes: 11,
        updated_at: Utc::now().to_rfc3339(),
    }
}

#[tokio::test]
async fn worker_manifest_stores_keep_file_locations_worker_owned() {
    let temp = tempdir().unwrap();
    let workspace_dir = temp.path().join("workspace");
    let state_dir = temp.path().join("state");
    let manifests_dir = temp.path().join("manifests");
    let project_id = Uuid::new_v4();
    let document_id = Uuid::new_v4();
    let manifest = manifest_with_project(project_id, "alpha");
    let provider = provider_with_manifest(manifest.clone()).await;
    let harness = nenjo_harness::Harness::builder(provider).build();

    let api = NenjoClient::new("http://127.0.0.1:9", "test-api-key");
    let cache = WorkerManifestCache {
        manifests_dir: manifests_dir.clone(),
        workspace_dir: workspace_dir.clone(),
        state_dir: state_dir.clone(),
    };

    cache
        .persist_resource(
            harness.provider().manifest(),
            nenjo_events::ResourceType::Project,
        )
        .expect("persist project manifest cache");
    assert!(manifests_dir.join("projects.json").exists());
    assert!(!workspace_dir.join("projects.json").exists());
    assert!(!state_dir.join("projects.json").exists());

    cache
        .sync_document_metadata(
            &api,
            harness.provider().manifest(),
            project_id,
            document_id,
            Some(&document_meta(document_id)),
        )
        .await
        .expect("sync document metadata");
    let pack_dir = workspace_dir.join("library").join("alpha");
    assert!(pack_dir.join("manifest.json").exists());
    assert!(
        !manifests_dir
            .join("library")
            .join("alpha")
            .join("manifest.json")
            .exists()
    );
    assert!(
        !state_dir
            .join("library")
            .join("alpha")
            .join("manifest.json")
            .exists()
    );

    cache
        .write_document_content(
            harness.provider().manifest(),
            project_id,
            "domain/spec.md",
            "hello world",
        )
        .expect("write document content");
    assert_eq!(
        std::fs::read_to_string(pack_dir.join("docs").join("domain").join("spec.md")).unwrap(),
        "hello world"
    );

    cache
        .remove_document(harness.provider().manifest(), project_id, document_id)
        .expect("remove document");
    assert!(
        !pack_dir
            .join("docs")
            .join("domain")
            .join("spec.md")
            .exists()
    );
}

#[tokio::test]
async fn worker_session_runtime_persists_harness_events_under_state_events() {
    let temp = tempdir().unwrap();
    let state_dir = temp.path().join("state");
    let session_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();

    let session_stores = WorkerSessionStores::new(&state_dir);
    let records = session_stores.records.clone();
    let runtime = WorkerSessionRuntime::new(
        session_stores,
        LocalSessionCoordinator::new(),
        "worker-test",
    );
    let provider = provider_with_manifest(Manifest::default()).await;
    let harness = nenjo_harness::Harness::builder(provider)
        .with_session_runtime(runtime)
        .build();

    harness
        .upsert_chat_session(nenjo_sessions::ChatSessionUpsert {
            session_id,
            status: SessionStatus::Active,
            project_id: Some(project_id),
            agent_id,
            memory_namespace: Some("project_alpha_agent_test".to_string()),
            trace_ref: None,
            metadata: json!({"progress": "started"}),
        })
        .await
        .expect("upsert chat session");

    harness
        .record_session_event(SessionRuntimeEvent::Transcript(SessionTranscriptRecord {
            session_id,
            turn_id: Some(turn_id),
            payload: SessionTranscriptEventPayload::ChatMessage {
                message: SessionTranscriptChatMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                },
            },
        }))
        .await
        .expect("record transcript event");

    harness
        .record_session_event(SessionRuntimeEvent::Trace(TraceEvent {
            session_id,
            turn_id: Some(turn_id),
            recorded_at: Utc::now(),
            phase: TracePhase::Completed,
            agent_id: Some(agent_id),
            agent_name: Some("test-agent".to_string()),
            tool_name: None,
            success: Some(true),
            usage: Default::default(),
            preview: Some("done".to_string()),
            metadata: json!({}),
        }))
        .await
        .expect("record trace event");

    harness
        .record_session_event(SessionRuntimeEvent::Checkpoint(CheckpointRecord {
            session_id,
            turn_id: Some(turn_id),
            checkpoint: SessionCheckpoint {
                session_id,
                seq: 0,
                saved_at: Utc::now(),
                current_phase: Some(nenjo_sessions::ExecutionPhase::CallingModel),
                active_tool_name: None,
                worktree: None,
                scheduler_runtime: None,
            },
        }))
        .await
        .expect("record checkpoint");

    assert!(
        state_dir
            .join("sessions")
            .join(format!("{session_id}.json"))
            .exists()
    );
    assert!(
        state_dir
            .join("events")
            .join("transcripts")
            .join(format!("{session_id}.jsonl"))
            .exists()
    );
    assert!(
        state_dir
            .join("events")
            .join("traces")
            .join(format!("{session_id}.jsonl"))
            .exists()
    );
    assert!(
        state_dir
            .join("events")
            .join("checkpoints")
            .join(format!("{session_id}.jsonl"))
            .exists()
    );
    assert!(!temp.path().join("workspace").join("chat_history").exists());

    let record = records
        .get(session_id)
        .expect("read session record")
        .expect("session record exists");
    let transcript_ref = format!("transcripts/{session_id}.jsonl");
    let trace_ref = format!("traces/{session_id}.jsonl");
    let checkpoint_ref = format!("checkpoints/{session_id}.jsonl");
    assert_eq!(
        record.refs.transcript_ref.as_deref(),
        Some(transcript_ref.as_str())
    );
    assert_eq!(record.refs.trace_ref.as_deref(), Some(trace_ref.as_str()));
    assert_eq!(
        record.refs.checkpoint_ref.as_deref(),
        Some(checkpoint_ref.as_str())
    );
    assert_eq!(record.summary.last_transcript_seq, 1);
    assert_eq!(record.summary.last_checkpoint_seq, 1);
}
