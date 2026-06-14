use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use nenjo::agents::prompts::PromptConfig;
use nenjo::manifest::{
    AbilityManifest, AbilityPromptConfig, AgentManifest, ContextBlockManifest,
    CouncilDelegationStrategy, CouncilManifest, DomainManifest, DomainPromptConfig, Manifest,
    McpServerManifest, ModelManifest, ProjectManifest, RoutineManifest, RoutineMetadata,
    RoutineTrigger,
};
use nenjo::provider::NoopToolFactory;
use nenjo::{ModelProviderFactory, Provider, Slug};
use nenjo_events::{ResourceAction, ResourceType};
use nenjo_harness::Harness;
use nenjo_worker::handlers::manifest::{
    ManifestChangedCommand, ManifestCommandContext, ManifestStore, McpRuntime,
    WorkerManifestHarnessExt,
};
use uuid::Uuid;

#[derive(Default)]
struct RecordingManifestStore {
    persisted: Mutex<Vec<ResourceType>>,
    removed: Mutex<Vec<ResourceType>>,
    metadata_syncs: Mutex<Vec<String>>,
    content_syncs: Mutex<Vec<String>>,
    removals: Mutex<Vec<String>>,
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
        _resource: &Slug,
    ) -> Result<()> {
        self.removed.lock().unwrap().push(resource_type);
        Ok(())
    }

    async fn full_refresh(
        &self,
        _client: &nenjo_platform::api_client::ApiClient,
    ) -> Result<Manifest> {
        Ok(Manifest::default())
    }

    async fn sync_document_metadata(
        &self,
        _client: &nenjo_platform::api_client::ApiClient,
        doc: &Slug,
        _metadata: Option<&nenjo_platform::api_client::KnowledgeDocumentRecord>,
        _edges: Option<nenjo_worker::handlers::manifest::DocumentEdgesSource<'_>>,
    ) -> Result<()> {
        self.metadata_syncs.lock().unwrap().push(doc.to_string());
        Ok(())
    }

    async fn sync_document(
        &self,
        _client: &nenjo_platform::api_client::ApiClient,
        doc: &Slug,
        _metadata: Option<&nenjo_platform::api_client::KnowledgeDocumentRecord>,
    ) -> Result<()> {
        self.content_syncs.lock().unwrap().push(doc.to_string());
        Ok(())
    }

    async fn remove_document(
        &self,
        doc: &Slug,
        _metadata: Option<&nenjo_platform::api_client::KnowledgeDocumentRecord>,
    ) -> Result<()> {
        self.removals.lock().unwrap().push(doc.to_string());
        Ok(())
    }

    fn write_document_content(
        &self,
        _pack: &Slug,
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
    harness: Harness<TestProvider, nenjo_sessions::NoopSessionRuntime>,
    store: Arc<RecordingManifestStore>,
    mcp: Arc<RecordingMcpRuntime>,
}

async fn test_harness(manifest: Manifest) -> TestHarness {
    let store = Arc::new(RecordingManifestStore::default());
    let mcp = Arc::new(RecordingMcpRuntime::default());
    let harness = Harness::builder(provider_with_manifest(manifest).await).build();

    TestHarness {
        harness,
        store,
        mcp,
    }
}

impl TestHarness {
    fn manifest_context(
        &self,
    ) -> ManifestCommandContext<Arc<RecordingManifestStore>, Arc<RecordingMcpRuntime>> {
        ManifestCommandContext {
            client: Arc::new(nenjo_platform::api_client::ApiClient::new(
                "http://127.0.0.1:9",
                "test",
            )),
            store: self.store.clone(),
            mcp: Some(self.mcp.clone()),
        }
    }
}

fn agent(_id: Uuid, name: &str, prompt: &str) -> AgentManifest {
    AgentManifest {
        name: name.into(),
        slug: Slug::derive(name),
        description: Some(format!("{name} description")),
        prompt_config: PromptConfig {
            developer_prompt: prompt.into(),
            ..Default::default()
        },
        color: None,
        model: None,
        domains: Vec::new(),
        platform_scopes: Vec::new(),
        mcp_servers: Vec::new(),
        script_tools: Vec::new(),
        abilities: Vec::new(),
        prompt_locked: false,
        heartbeat: None,
    }
}

fn model(_id: Uuid, name: &str) -> ModelManifest {
    ModelManifest {
        slug: Slug::derive(name),
        name: name.into(),
        description: None,
        model: "gpt-test".into(),
        model_provider: "test".into(),
        temperature: Some(0.1),
        base_url: None,
    }
}

fn routine(_id: Uuid, name: &str) -> RoutineManifest {
    RoutineManifest {
        name: name.into(),
        slug: Slug::derive(name),
        description: None,
        trigger: RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: Vec::new(),
        edges: Vec::new(),
    }
}

fn project(_id: Uuid, name: &str) -> ProjectManifest {
    ProjectManifest {
        name: name.into(),
        slug: Slug::derive(name),
        description: None,
        settings: serde_json::json!({}),
    }
}

fn council(_id: Uuid, name: &str) -> CouncilManifest {
    CouncilManifest {
        name: name.into(),
        delegation_strategy: CouncilDelegationStrategy::Decompose,
        leader_agent: Slug::derive("leader"),
        members: Vec::new(),
    }
}

fn ability(_id: Uuid, name: &str, prompt: &str) -> AbilityManifest {
    AbilityManifest {
        name: name.into(),
        path: None,
        description: None,
        activation_condition: "always".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: prompt.into(),
        },
        platform_scopes: Vec::new(),
        mcp_servers: Vec::new(),
        script_tools: Vec::new(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    }
}

fn context_block(_id: Uuid, name: &str, template: &str) -> ContextBlockManifest {
    ContextBlockManifest {
        name: name.into(),
        path: String::new(),
        description: None,
        template: template.into(),
    }
}

fn mcp_server(_id: Uuid, name: &str) -> McpServerManifest {
    McpServerManifest {
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

fn domain(_id: Uuid, name: &str, prompt: &str) -> DomainManifest {
    DomainManifest {
        name: name.into(),
        path: String::new(),
        description: None,
        command: name.into(),
        platform_scopes: Vec::new(),
        abilities: Vec::new(),
        mcp_servers: Vec::new(),
        script_tools: Vec::new(),
        prompt_config: DomainPromptConfig {
            developer_prompt_addon: Some(prompt.into()),
        },
    }
}

const INLINE_TS: &str = "2026-05-10T00:00:00Z";

fn inline_org_id() -> Uuid {
    Uuid::from_u128(42)
}

fn inline_created_by() -> Uuid {
    Uuid::from_u128(43)
}

fn wrap_inline_payload(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "schema": "manifest.resource.v1",
        "data": data,
    })
}

fn agent_inline_payload(id: Uuid, slug: &str, prompt: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "description": null,
        "color": null,
        "model": null,
        "domains": [],
        "platform_scopes": [],
        "mcp_servers": [],
        "script_tools": [],
        "abilities": [],
        "prompt_locked": false,
        "heartbeat": null,
        "source_type": "native",
        "read_only": false,
        "metadata": {},
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
        "prompt_config": PromptConfig {
            developer_prompt: prompt.into(),
            ..Default::default()
        },
    }))
}

fn agent_metadata_inline_payload(id: Uuid, slug: &str, name: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": name,
        "description": null,
        "color": null,
        "model": null,
        "domains": [],
        "platform_scopes": [],
        "mcp_servers": [],
        "script_tools": [],
        "abilities": [],
        "prompt_locked": false,
        "heartbeat": null,
        "source_type": "native",
        "read_only": false,
        "metadata": {},
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
    }))
}

fn model_inline_payload(id: Uuid, slug: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "description": null,
        "model": "gpt-test",
        "model_provider": "test",
        "temperature": 0.1,
        "base_url": null,
        "created_by": inline_created_by(),
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
    }))
}

fn routine_inline_payload(id: Uuid, slug: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "description": null,
        "trigger": "task",
        "metadata": {},
        "steps": [],
        "edges": [],
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
    }))
}

fn project_inline_payload(id: Uuid, slug: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "description": null,
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
    }))
}

fn council_inline_payload(id: Uuid, slug: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "delegation_strategy": "decompose",
        "leader_agent": "leader",
        "members": [],
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
    }))
}

fn ability_inline_payload(id: Uuid, slug: &str, prompt: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "path": "",
        "description": null,
        "activation_condition": "always",
        "platform_scopes": [],
        "mcp_servers": [],
        "script_tools": [],
        "source_type": "native",
        "read_only": false,
        "metadata": {},
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
        "prompt_config": AbilityPromptConfig {
            developer_prompt: prompt.into(),
        },
    }))
}

fn context_block_inline_payload(id: Uuid, slug: &str, template: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "path": "",
        "description": null,
        "source_type": "native",
        "read_only": false,
        "metadata": {},
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
        "template": template,
    }))
}

fn domain_inline_payload(id: Uuid, slug: &str, prompt: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "slug": slug,
        "name": slug,
        "path": "",
        "description": null,
        "command": slug,
        "platform_scopes": [],
        "abilities": [],
        "mcp_servers": [],
        "script_tools": [],
        "source_type": "native",
        "read_only": false,
        "metadata": {},
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
        "prompt_config": DomainPromptConfig {
            developer_prompt_addon: Some(prompt.into()),
        },
    }))
}

fn mcp_inline_payload(id: Uuid, slug: &str) -> serde_json::Value {
    wrap_inline_payload(serde_json::to_value(mcp_server(id, slug)).expect("mcp manifest"))
}

fn knowledge_document_payload(
    id: Uuid,
    pack_id: Uuid,
    pack_slug: &str,
    slug: &str,
) -> serde_json::Value {
    wrap_inline_payload(serde_json::json!({
        "id": id,
        "org_id": inline_org_id(),
        "pack_id": pack_id,
        "pack_slug": pack_slug,
        "slug": slug,
        "filename": "guide.md",
        "path": "docs",
        "title": "Guide",
        "kind": "markdown",
        "summary": null,
        "tags": [],
        "content_type": "text/markdown",
        "created_at": INLINE_TS,
        "updated_at": INLINE_TS,
        "edges": [],
    }))
}

#[tokio::test]
async fn manifest_inline_upserts_each_provider_resource() {
    let id = Uuid::new_v4();

    let cases = vec![
        (
            ResourceType::Agent,
            agent_inline_payload(id, "agent", "agent prompt"),
        ),
        (ResourceType::Model, model_inline_payload(id, "model")),
        (ResourceType::Routine, routine_inline_payload(id, "routine")),
        (ResourceType::Project, project_inline_payload(id, "project")),
        (ResourceType::Council, council_inline_payload(id, "council")),
        (
            ResourceType::Ability,
            ability_inline_payload(id, "ability", "ability prompt"),
        ),
        (
            ResourceType::ContextBlock,
            context_block_inline_payload(id, "context", "template"),
        ),
        (ResourceType::McpServer, mcp_inline_payload(id, "mcp")),
        (
            ResourceType::Domain,
            domain_inline_payload(id, "domain", "domain prompt"),
        ),
    ];

    for (resource_type, payload) in cases {
        let resource = Slug::derive(match resource_type {
            ResourceType::Agent => "agent",
            ResourceType::Model => "model",
            ResourceType::Routine => "routine",
            ResourceType::Project => "project",
            ResourceType::Council => "council",
            ResourceType::Ability => "ability",
            ResourceType::ContextBlock => "context",
            ResourceType::McpServer => "mcp",
            ResourceType::Domain => "domain",
            ResourceType::Document | ResourceType::KnowledgePack => unreachable!(),
        });
        let env = test_harness(Manifest::default()).await;
        env.harness
            .handle_manifest_changed(
                &env.manifest_context(),
                ManifestChangedCommand {
                    resource_id: Uuid::nil(),
                    resource_type,
                    resource: resource.clone(),
                    action: ResourceAction::Created,
                    project: None,
                    payload: Some(payload),
                    encrypted_payload: None,
                },
            )
            .await
            .unwrap();

        let manifest = env.harness.provider();
        let manifest = manifest.manifest_snapshot();
        match resource_type {
            ResourceType::Agent => {
                let item = manifest
                    .agents
                    .iter()
                    .find(|item| item.slug == resource)
                    .unwrap();
                assert_eq!(item.name, "agent");
                assert_eq!(item.prompt_config.developer_prompt, "agent prompt");
            }
            ResourceType::Model => {
                assert!(manifest.models.iter().any(|item| item.slug == resource))
            }
            ResourceType::Routine => {
                assert!(manifest.routines.iter().any(|item| item.slug == resource))
            }
            ResourceType::Project => {
                assert!(manifest.projects.iter().any(|item| item.slug == resource))
            }
            ResourceType::Council => {
                assert!(manifest.councils.iter().any(|item| item.name == "council"))
            }
            ResourceType::Ability => {
                let item = manifest
                    .abilities
                    .iter()
                    .find(|item| Slug::derive(&item.name) == resource)
                    .unwrap();
                assert_eq!(item.prompt_config.developer_prompt, "ability prompt");
            }
            ResourceType::ContextBlock => {
                let item = manifest
                    .context_blocks
                    .iter()
                    .find(|item| Slug::derive(&item.name) == resource)
                    .unwrap();
                assert_eq!(item.template, "template");
            }
            ResourceType::McpServer => {
                assert!(
                    manifest
                        .mcp_servers
                        .iter()
                        .any(|item| Slug::derive(&item.name) == resource)
                )
            }
            ResourceType::Domain => {
                let item = manifest
                    .domains
                    .iter()
                    .find(|item| Slug::derive(&item.name) == resource)
                    .unwrap();
                assert_eq!(
                    item.prompt_config.developer_prompt_addon.as_deref(),
                    Some("domain prompt")
                );
            }
            ResourceType::Document | ResourceType::KnowledgePack => unreachable!(),
        }

        assert_eq!(
            env.store.persisted.lock().unwrap().as_slice(),
            &[resource_type]
        );
        assert!(env.store.removed.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn manifest_inline_agent_metadata_update_preserves_cached_prompt() {
    let id = Uuid::new_v4();
    let env = test_harness(Manifest {
        agents: vec![agent(id, "old", "cached prompt")],
        ..Default::default()
    })
    .await;
    let metadata_payload = agent_metadata_inline_payload(id, "old", "renamed");

    env.harness
        .handle_manifest_changed(
            &env.manifest_context(),
            ManifestChangedCommand {
                resource_id: Uuid::nil(),
                resource_type: ResourceType::Agent,
                resource: Slug::derive("old"),
                action: ResourceAction::Updated,
                project: None,
                payload: Some(metadata_payload),
                encrypted_payload: None,
            },
        )
        .await
        .unwrap();

    let provider = env.harness.provider();
    let manifest = provider.manifest_snapshot();
    let item = manifest
        .agents
        .iter()
        .find(|item| item.slug == Slug::derive("old"))
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
        ..Default::default()
    };

    for (resource_type, _resource_id) in ids {
        let resource = match resource_type {
            ResourceType::Agent => Slug::derive("agent"),
            ResourceType::Model => Slug::derive("model"),
            ResourceType::Routine => Slug::derive("routine"),
            ResourceType::Project => Slug::derive("project"),
            ResourceType::Council => Slug::derive("council"),
            ResourceType::Ability => Slug::derive("ability"),
            ResourceType::ContextBlock => Slug::derive("context"),
            ResourceType::McpServer => Slug::derive("mcp"),
            ResourceType::Domain => Slug::derive("domain"),
            ResourceType::Document | ResourceType::KnowledgePack => unreachable!(),
        };
        let env = test_harness(manifest.clone()).await;
        env.harness
            .handle_manifest_changed(
                &env.manifest_context(),
                ManifestChangedCommand {
                    resource_id: Uuid::nil(),
                    resource_type,
                    resource: resource.clone(),
                    action: ResourceAction::Deleted,
                    project: None,
                    payload: None,
                    encrypted_payload: None,
                },
            )
            .await
            .unwrap();

        let provider = env.harness.provider();
        let manifest = provider.manifest_snapshot();
        match resource_type {
            ResourceType::Agent => {
                assert!(!manifest.agents.iter().any(|item| item.slug == resource))
            }
            ResourceType::Model => {
                assert!(!manifest.models.iter().any(|item| item.slug == resource))
            }
            ResourceType::Routine => {
                assert!(!manifest.routines.iter().any(|item| item.slug == resource))
            }
            ResourceType::Project => {
                assert!(!manifest.projects.iter().any(|item| item.slug == resource))
            }
            ResourceType::Council => {
                assert!(
                    !manifest
                        .councils
                        .iter()
                        .any(|item| Slug::derive(&item.name) == resource)
                )
            }
            ResourceType::Ability => {
                assert!(
                    !manifest
                        .abilities
                        .iter()
                        .any(|item| Slug::derive(&item.name) == resource)
                )
            }
            ResourceType::ContextBlock => {
                assert!(
                    !manifest
                        .context_blocks
                        .iter()
                        .any(|item| Slug::derive(&item.name) == resource)
                )
            }
            ResourceType::McpServer => {
                assert!(
                    !manifest
                        .mcp_servers
                        .iter()
                        .any(|item| Slug::derive(&item.name) == resource)
                )
            }
            ResourceType::Domain => {
                assert!(
                    !manifest
                        .domains
                        .iter()
                        .any(|item| Slug::derive(&item.name) == resource)
                )
            }
            ResourceType::Document | ResourceType::KnowledgePack => unreachable!(),
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
    let pack_id = Uuid::new_v4();
    let document_id = Uuid::new_v4();
    let env = test_harness(Manifest {
        projects: vec![project(project_id, "project")],
        ..Default::default()
    })
    .await;

    env.harness
        .handle_manifest_changed(
            &env.manifest_context(),
            ManifestChangedCommand {
                resource_id: document_id,
                resource_type: ResourceType::Document,
                resource: Slug::derive("guide"),
                action: ResourceAction::Updated,
                project: Some(Slug::derive("project")),
                payload: Some(knowledge_document_payload(
                    document_id,
                    pack_id,
                    "project",
                    "guide",
                )),
                encrypted_payload: None,
            },
        )
        .await
        .unwrap();

    env.harness
        .handle_manifest_changed(
            &env.manifest_context(),
            ManifestChangedCommand {
                resource_id: document_id,
                resource_type: ResourceType::Document,
                resource: Slug::derive("guide"),
                action: ResourceAction::Deleted,
                project: Some(Slug::derive("project")),
                payload: Some(knowledge_document_payload(
                    document_id,
                    pack_id,
                    "project",
                    "guide",
                )),
                encrypted_payload: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        env.store.metadata_syncs.lock().unwrap().as_slice(),
        &["guide".to_string()]
    );
    assert!(env.store.content_syncs.lock().unwrap().is_empty());
    assert_eq!(
        env.store.removals.lock().unwrap().as_slice(),
        &["guide".to_string()]
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
            &env.manifest_context(),
            ManifestChangedCommand {
                resource_id: id,
                resource_type: ResourceType::McpServer,
                resource: Slug::derive("mcp"),
                action: ResourceAction::Created,
                project: None,
                payload: Some(mcp_inline_payload(id, "mcp")),
                encrypted_payload: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(env.mcp.reconciles.lock().unwrap().as_slice(), &[1]);
}
