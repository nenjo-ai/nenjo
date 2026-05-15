use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use nenjo::manifest::{
    AbilityManifest, AbilityPromptConfig, AgentManifest, ContextBlockManifest,
    CouncilDelegationStrategy, CouncilManifest, DomainManifest, DomainPromptConfig, Manifest,
    McpServerManifest, ModelManifest, ProjectManifest, PromptConfig, RoutineManifest,
    RoutineMetadata, RoutineTrigger,
};
use nenjo::provider::NoopToolFactory;
use nenjo::{ModelProviderFactory, Provider};
use nenjo_events::{ResourceAction, ResourceType};
use nenjo_harness::handlers::manifest::{ManifestStore, McpRuntime};
use nenjo_harness::{Harness, HarnessError};
use uuid::Uuid;

#[derive(Default)]
struct RecordingManifestStore {
    persisted: Mutex<Vec<ResourceType>>,
    removed: Mutex<Vec<ResourceType>>,
    metadata_syncs: Mutex<Vec<Uuid>>,
    content_syncs: Mutex<Vec<Uuid>>,
    removals: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl ManifestStore for RecordingManifestStore {
    async fn persist_resource(
        &self,
        _manifest: &Manifest,
        resource_type: ResourceType,
    ) -> Result<()> {
        self.persisted.lock().unwrap().push(resource_type);
        Ok(())
    }

    async fn remove_resource(
        &self,
        _manifest: &Manifest,
        resource_type: ResourceType,
        _resource_id: Uuid,
    ) -> Result<()> {
        self.removed.lock().unwrap().push(resource_type);
        Ok(())
    }

    async fn full_refresh(&self, _client: &nenjo::client::NenjoClient) -> Result<Manifest> {
        Ok(Manifest::default())
    }

    async fn sync_document_metadata(
        &self,
        _client: &nenjo::client::NenjoClient,
        _manifest: &Manifest,
        _project_id: Uuid,
        document_id: Uuid,
        _metadata: Option<&nenjo::client::DocumentSyncMeta>,
    ) -> Result<()> {
        self.metadata_syncs.lock().unwrap().push(document_id);
        Ok(())
    }

    async fn sync_document(
        &self,
        _client: &nenjo::client::NenjoClient,
        _manifest: &Manifest,
        _project_id: Uuid,
        document_id: Uuid,
        _metadata: Option<&nenjo::client::DocumentSyncMeta>,
    ) -> Result<()> {
        self.content_syncs.lock().unwrap().push(document_id);
        Ok(())
    }

    fn remove_document(
        &self,
        _manifest: &Manifest,
        _project_id: Uuid,
        document_id: Uuid,
    ) -> Result<()> {
        self.removals.lock().unwrap().push(document_id);
        Ok(())
    }

    fn write_document_content(
        &self,
        _manifest: &Manifest,
        _project_id: Uuid,
        _relative_path: &str,
        _content: &str,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingMcpRuntime {
    reconciles: Mutex<Vec<usize>>,
}

#[async_trait]
impl McpRuntime for RecordingMcpRuntime {
    async fn reconcile_mcp(&self, servers: &[McpServerManifest]) {
        self.reconciles.lock().unwrap().push(servers.len());
    }
}

struct TestModelProvider;

#[async_trait]
impl nenjo::ModelProvider for TestModelProvider {
    async fn chat(
        &self,
        _request: nenjo_models::ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<nenjo_models::ChatResponse> {
        Ok(nenjo_models::ChatResponse {
            text: Some("ok".to_string()),
            tool_calls: vec![],
            usage: nenjo_models::TokenUsage::default(),
        })
    }
}

struct TestModelFactory;

impl ModelProviderFactory for TestModelFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn nenjo::ModelProvider>> {
        Ok(Arc::new(TestModelProvider))
    }
}

type TestProvider = Provider<TestModelFactory, NoopToolFactory, nenjo::provider::builder::NoMemory>;

async fn provider_with_manifest(manifest: Manifest) -> TestProvider {
    Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(TestModelFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap()
}

struct TestHarness {
    harness: Harness<
        TestProvider,
        nenjo_sessions::NoopSessionRuntime,
        nenjo_harness::execution_trace::NoopExecutionTraceRuntime,
        Arc<RecordingManifestStore>,
        Arc<RecordingMcpRuntime>,
    >,
    store: Arc<RecordingManifestStore>,
    mcp: Arc<RecordingMcpRuntime>,
}

async fn test_harness(manifest: Manifest) -> TestHarness {
    let store = Arc::new(RecordingManifestStore::default());
    let mcp = Arc::new(RecordingMcpRuntime::default());
    let client = Arc::new(nenjo::client::NenjoClient::new(
        "http://127.0.0.1:9",
        "test",
    ));
    let harness = Harness::builder(provider_with_manifest(manifest).await)
        .with_manifest_client(client)
        .with_manifest_store(store.clone())
        .with_mcp_runtime(mcp.clone())
        .build();

    TestHarness {
        harness,
        store,
        mcp,
    }
}

fn agent(id: Uuid, name: &str, prompt: &str) -> AgentManifest {
    AgentManifest {
        id,
        name: name.into(),
        description: Some(format!("{name} description")),
        prompt_config: PromptConfig {
            developer_prompt: prompt.into(),
            ..Default::default()
        },
        color: None,
        model_id: None,
        domain_ids: Vec::new(),
        platform_scopes: Vec::new(),
        mcp_server_ids: Vec::new(),
        ability_ids: Vec::new(),
        prompt_locked: false,
        heartbeat: None,
    }
}

fn model(id: Uuid, name: &str) -> ModelManifest {
    ModelManifest {
        id,
        name: name.into(),
        description: None,
        model: "gpt-test".into(),
        model_provider: "test".into(),
        temperature: Some(0.1),
        base_url: None,
    }
}

fn routine(id: Uuid, name: &str) -> RoutineManifest {
    RoutineManifest {
        id,
        name: name.into(),
        description: None,
        trigger: RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: Vec::new(),
        edges: Vec::new(),
    }
}

fn project(id: Uuid, name: &str) -> ProjectManifest {
    ProjectManifest {
        id,
        name: name.into(),
        slug: name.to_lowercase(),
        description: None,
        settings: serde_json::json!({}),
    }
}

fn council(id: Uuid, name: &str) -> CouncilManifest {
    CouncilManifest {
        id,
        name: name.into(),
        delegation_strategy: CouncilDelegationStrategy::Decompose,
        leader_agent_id: Uuid::new_v4(),
        members: Vec::new(),
    }
}

fn ability(id: Uuid, name: &str, prompt: &str) -> AbilityManifest {
    AbilityManifest {
        id,
        name: name.into(),
        tool_name: format!("{name}_tool"),
        path: String::new(),
        display_name: Some(name.into()),
        description: None,
        activation_condition: "always".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: prompt.into(),
        },
        platform_scopes: Vec::new(),
        mcp_server_ids: Vec::new(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    }
}

fn context_block(id: Uuid, name: &str, template: &str) -> ContextBlockManifest {
    ContextBlockManifest {
        id,
        name: name.into(),
        path: String::new(),
        display_name: Some(name.into()),
        description: None,
        template: template.into(),
    }
}

fn mcp_server(id: Uuid, name: &str) -> McpServerManifest {
    McpServerManifest {
        id,
        name: name.into(),
        display_name: name.into(),
        description: None,
        transport: "stdio".into(),
        command: Some("test-mcp".into()),
        args: Some(Vec::new()),
        url: None,
        env_schema: serde_json::json!({}),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    }
}

fn domain(id: Uuid, name: &str, prompt: &str) -> DomainManifest {
    DomainManifest {
        id,
        name: name.into(),
        path: String::new(),
        display_name: name.into(),
        description: None,
        command: name.into(),
        platform_scopes: Vec::new(),
        ability_ids: Vec::new(),
        mcp_server_ids: Vec::new(),
        prompt_config: DomainPromptConfig {
            developer_prompt_addon: Some(prompt.into()),
        },
    }
}

#[tokio::test]
async fn manifest_inline_upserts_each_provider_resource() {
    let id = Uuid::new_v4();

    let cases = vec![
        (
            ResourceType::Agent,
            serde_json::to_value(agent(id, "agent", "agent prompt")).unwrap(),
        ),
        (
            ResourceType::Model,
            serde_json::to_value(model(id, "model")).unwrap(),
        ),
        (
            ResourceType::Routine,
            serde_json::to_value(routine(id, "routine")).unwrap(),
        ),
        (
            ResourceType::Project,
            serde_json::to_value(project(id, "project")).unwrap(),
        ),
        (
            ResourceType::Council,
            serde_json::to_value(council(id, "council")).unwrap(),
        ),
        (
            ResourceType::Ability,
            serde_json::to_value(ability(id, "ability", "ability prompt")).unwrap(),
        ),
        (
            ResourceType::ContextBlock,
            serde_json::to_value(context_block(id, "context", "template")).unwrap(),
        ),
        (
            ResourceType::McpServer,
            serde_json::to_value(mcp_server(id, "mcp")).unwrap(),
        ),
        (
            ResourceType::Domain,
            serde_json::to_value(domain(id, "domain", "domain prompt")).unwrap(),
        ),
    ];

    for (resource_type, payload) in cases {
        let env = test_harness(Manifest::default()).await;
        env.harness
            .handle_manifest_changed(
                resource_type,
                id,
                ResourceAction::Created,
                None,
                Some(payload),
                None,
            )
            .await
            .unwrap();

        let manifest = env.harness.provider();
        let manifest = manifest.manifest();
        match resource_type {
            ResourceType::Agent => {
                let item = manifest.agents.iter().find(|item| item.id == id).unwrap();
                assert_eq!(item.name, "agent");
                assert_eq!(item.prompt_config.developer_prompt, "agent prompt");
            }
            ResourceType::Model => assert!(manifest.models.iter().any(|item| item.id == id)),
            ResourceType::Routine => assert!(manifest.routines.iter().any(|item| item.id == id)),
            ResourceType::Project => assert!(manifest.projects.iter().any(|item| item.id == id)),
            ResourceType::Council => assert!(manifest.councils.iter().any(|item| item.id == id)),
            ResourceType::Ability => {
                let item = manifest
                    .abilities
                    .iter()
                    .find(|item| item.id == id)
                    .unwrap();
                assert_eq!(item.prompt_config.developer_prompt, "ability prompt");
            }
            ResourceType::ContextBlock => {
                let item = manifest
                    .context_blocks
                    .iter()
                    .find(|item| item.id == id)
                    .unwrap();
                assert_eq!(item.template, "template");
            }
            ResourceType::McpServer => {
                assert!(manifest.mcp_servers.iter().any(|item| item.id == id))
            }
            ResourceType::Domain => {
                let item = manifest.domains.iter().find(|item| item.id == id).unwrap();
                assert_eq!(
                    item.prompt_config.developer_prompt_addon.as_deref(),
                    Some("domain prompt")
                );
            }
            ResourceType::Document => unreachable!(),
        }

        assert_eq!(
            env.store.persisted.lock().unwrap().as_slice(),
            &[resource_type]
        );
        assert!(env.store.removed.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn manifest_handler_reports_missing_services_as_typed_harness_error() {
    let harness = Harness::builder(provider_with_manifest(Manifest::default()).await).build();
    let error = harness
        .handle_manifest_changed(
            ResourceType::Agent,
            Uuid::new_v4(),
            ResourceAction::Created,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();

    assert!(matches!(error, HarnessError::ManifestServicesNotConfigured));
}

#[tokio::test]
async fn manifest_inline_agent_metadata_update_preserves_cached_prompt() {
    let id = Uuid::new_v4();
    let env = test_harness(Manifest {
        agents: vec![agent(id, "old", "cached prompt")],
        ..Default::default()
    })
    .await;
    let metadata_payload = serde_json::json!({
        "id": id,
        "name": "renamed",
        "description": null,
        "color": null,
        "model_id": null,
        "domains": [],
        "platform_scopes": [],
        "mcp_server_ids": [],
        "abilities": [],
        "prompt_locked": false,
        "heartbeat": null
    });

    env.harness
        .handle_manifest_changed(
            ResourceType::Agent,
            id,
            ResourceAction::Updated,
            None,
            Some(metadata_payload),
            None,
        )
        .await
        .unwrap();

    let provider = env.harness.provider();
    let item = provider
        .manifest()
        .agents
        .iter()
        .find(|item| item.id == id)
        .unwrap();
    assert_eq!(item.name, "renamed");
    assert_eq!(item.prompt_config.developer_prompt, "cached prompt");
}

#[tokio::test]
async fn manifest_deletes_each_provider_resource_and_uses_remove_store_path() {
    let ids = [
        (ResourceType::Agent, Uuid::new_v4()),
        (ResourceType::Model, Uuid::new_v4()),
        (ResourceType::Routine, Uuid::new_v4()),
        (ResourceType::Project, Uuid::new_v4()),
        (ResourceType::Council, Uuid::new_v4()),
        (ResourceType::Ability, Uuid::new_v4()),
        (ResourceType::ContextBlock, Uuid::new_v4()),
        (ResourceType::McpServer, Uuid::new_v4()),
        (ResourceType::Domain, Uuid::new_v4()),
    ];
    let manifest = Manifest {
        agents: vec![agent(ids[0].1, "agent", "prompt")],
        models: vec![model(ids[1].1, "model")],
        routines: vec![routine(ids[2].1, "routine")],
        projects: vec![project(ids[3].1, "project")],
        councils: vec![council(ids[4].1, "council")],
        abilities: vec![ability(ids[5].1, "ability", "prompt")],
        context_blocks: vec![context_block(ids[6].1, "context", "template")],
        mcp_servers: vec![mcp_server(ids[7].1, "mcp")],
        domains: vec![domain(ids[8].1, "domain", "prompt")],
    };

    for (resource_type, resource_id) in ids {
        let env = test_harness(manifest.clone()).await;
        env.harness
            .handle_manifest_changed(
                resource_type,
                resource_id,
                ResourceAction::Deleted,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let provider = env.harness.provider();
        let manifest = provider.manifest();
        match resource_type {
            ResourceType::Agent => {
                assert!(!manifest.agents.iter().any(|item| item.id == resource_id))
            }
            ResourceType::Model => {
                assert!(!manifest.models.iter().any(|item| item.id == resource_id))
            }
            ResourceType::Routine => {
                assert!(!manifest.routines.iter().any(|item| item.id == resource_id))
            }
            ResourceType::Project => {
                assert!(!manifest.projects.iter().any(|item| item.id == resource_id))
            }
            ResourceType::Council => {
                assert!(!manifest.councils.iter().any(|item| item.id == resource_id))
            }
            ResourceType::Ability => {
                assert!(!manifest.abilities.iter().any(|item| item.id == resource_id))
            }
            ResourceType::ContextBlock => {
                assert!(
                    !manifest
                        .context_blocks
                        .iter()
                        .any(|item| item.id == resource_id)
                )
            }
            ResourceType::McpServer => {
                assert!(
                    !manifest
                        .mcp_servers
                        .iter()
                        .any(|item| item.id == resource_id)
                )
            }
            ResourceType::Domain => {
                assert!(!manifest.domains.iter().any(|item| item.id == resource_id))
            }
            ResourceType::Document => unreachable!(),
        }

        assert!(env.store.persisted.lock().unwrap().is_empty());
        assert_eq!(
            env.store.removed.lock().unwrap().as_slice(),
            &[resource_type]
        );
    }
}

#[tokio::test]
async fn manifest_document_upsert_and_delete_use_document_store_side_effects() {
    let project_id = Uuid::new_v4();
    let document_id = Uuid::new_v4();
    let env = test_harness(Manifest {
        projects: vec![project(project_id, "project")],
        ..Default::default()
    })
    .await;

    env.harness
        .handle_manifest_changed(
            ResourceType::Document,
            document_id,
            ResourceAction::Updated,
            Some(project_id),
            Some(serde_json::json!({
                "id": document_id,
                "project_id": project_id,
                "filename": "guide.md",
                "path": "docs",
                "title": "Guide",
                "kind": "markdown",
                "authority": null,
                "summary": null,
                "status": null,
                "tags": [],
                "aliases": [],
                "keywords": [],
                "size_bytes": 42,
                "updated_at": "2026-05-10T00:00:00Z"
            })),
            None,
        )
        .await
        .unwrap();

    env.harness
        .handle_manifest_changed(
            ResourceType::Document,
            document_id,
            ResourceAction::Deleted,
            Some(project_id),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        env.store.content_syncs.lock().unwrap().as_slice(),
        &[document_id]
    );
    assert!(env.store.metadata_syncs.lock().unwrap().is_empty());
    assert_eq!(
        env.store.removals.lock().unwrap().as_slice(),
        &[document_id]
    );
    assert_eq!(
        env.store.persisted.lock().unwrap().as_slice(),
        &[ResourceType::Document]
    );
    assert_eq!(
        env.store.removed.lock().unwrap().as_slice(),
        &[ResourceType::Document]
    );
}

#[tokio::test]
async fn manifest_mcp_changes_reconcile_mcp_runtime() {
    let id = Uuid::new_v4();
    let env = test_harness(Manifest::default()).await;

    env.harness
        .handle_manifest_changed(
            ResourceType::McpServer,
            id,
            ResourceAction::Created,
            None,
            Some(serde_json::to_value(mcp_server(id, "mcp")).unwrap()),
            None,
        )
        .await
        .unwrap();

    assert_eq!(env.mcp.reconciles.lock().unwrap().as_slice(), &[1]);
}
