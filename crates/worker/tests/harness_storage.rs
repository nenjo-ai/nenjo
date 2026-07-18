use std::sync::Arc;

use chrono::Utc;
use nenjo::Slug;
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

use nenjo_worker::api_client::{ApiClient, KnowledgeDocumentRecord};
use nenjo_worker::bootstrap::WorkerManifestCache;
use nenjo_worker::handlers::manifest::ManifestStore;
use nenjo_worker::sessions::{WorkerSessionRuntime, WorkerSessionStores};

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
            provider_tool_calls: vec![],
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

fn manifest_with_project(_project_id: Uuid, slug: &str) -> Manifest {
    Manifest {
        projects: vec![ProjectManifest {
            name: "Alpha Project".to_string(),
            slug: Slug::derive(slug),
            description: None,
            settings: json!({}),
        }],
        ..Default::default()
    }
}

fn document_meta(document_id: Uuid) -> KnowledgeDocumentRecord {
    let now = Utc::now();
    KnowledgeDocumentRecord {
        id: document_id,
        org_id: Uuid::from_u128(9),
        pack_id: Uuid::from_u128(7),
        pack_slug: "alpha".to_string(),
        slug: "alpha".to_string(),
        filename: "spec.md".to_string(),
        path: Some("domain".to_string()),
        title: Some("Spec".to_string()),
        kind: Some("note".to_string()),
        summary: Some("Project spec".to_string()),
        tags: vec!["planning".to_string()],
        content_type: "text/markdown".to_string(),
        created_at: now,
        updated_at: now,
        edges: Vec::new(),
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

    let api = ApiClient::new("http://127.0.0.1:9", "test-api-key");
    let cache = WorkerManifestCache {
        manifests_dir: manifests_dir.clone(),
        workspace_dir: workspace_dir.clone(),
        state_dir: state_dir.clone(),
        config_dir: temp.path().join("config"),
    };
    let config_dir = cache.config_dir.clone();

    let manifest = harness.provider().manifest_snapshot();
    cache
        .persist_resource(&manifest, nenjo_events::ResourceType::Project)
        .await
        .expect("persist project manifest cache");
    assert!(manifests_dir.join("projects.json").exists());
    assert!(!workspace_dir.join("projects.json").exists());
    assert!(!state_dir.join("projects.json").exists());

    cache
        .sync_document_metadata(
            &api,
            &Slug::derive("alpha"),
            Some(&document_meta(document_id)),
            None,
        )
        .await
        .expect("sync document metadata");
    let pack_dir = config_dir.join("library").join("alpha");
    assert!(pack_dir.join("manifest.json").exists());
    assert!(
        !workspace_dir
            .join("library")
            .join("alpha")
            .join("manifest.json")
            .exists()
    );
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
        .write_document_content(&Slug::derive("alpha"), "domain/spec.md", "hello world")
        .expect("write document content");
    assert_eq!(
        std::fs::read_to_string(pack_dir.join("docs").join("domain").join("spec.md")).unwrap(),
        "hello world"
    );

    cache
        .remove_document(&Slug::derive("alpha"), Some(&document_meta(document_id)))
        .await
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

    let session_stores = WorkerSessionStores::new(&state_dir);
    let records = session_stores.records.clone();
    let runtime = WorkerSessionRuntime::with_host(session_stores, "worker-test");
    let provider = provider_with_manifest(Manifest::default()).await;
    let harness = nenjo_harness::Harness::builder(provider)
        .with_session_runtime(runtime)
        .build();

    harness
        .sessions()
        .upsert_chat(nenjo_sessions::ChatSessionUpsert {
            session_id,
            status: SessionStatus::Active,
            project: Some("alpha".to_string()),
            agent: "test_agent".to_string(),
            memory_namespace: Some("project_alpha_agent_test".to_string()),
            metadata: json!({"progress": "started"}),
        })
        .await
        .expect("upsert chat session");

    harness
        .sessions()
        .record(SessionRuntimeEvent::Transcript(SessionTranscriptRecord {
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
        .sessions()
        .record(SessionRuntimeEvent::Trace(TraceEvent {
            session_id,
            turn_id: Some(turn_id),
            recorded_at: Utc::now(),
            phase: TracePhase::Completed,
            agent_id: Some(agent_id),
            agent_name: Some("test-agent".to_string()),
            tool_name: None,
            parent_tool_name: None,
            ability_name: None,
            target_agent_id: None,
            target_agent_name: None,
            success: Some(true),
            usage: Default::default(),
            preview: Some("done".to_string()),
            task_input: None,
            final_output: None,
            tool_args: None,
            error_preview: None,
            metadata: json!({}),
        }))
        .await
        .expect("record trace event");

    harness
        .sessions()
        .record(SessionRuntimeEvent::Checkpoint(CheckpointRecord {
            session_id,
            turn_id: Some(turn_id),
            checkpoint: SessionCheckpoint {
                session_id,
                seq: 0,
                saved_at: Utc::now(),
                current_phase: Some(nenjo_sessions::ExecutionPhase::CallingModel),
                active_tool_name: None,
                worktree: None,
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
