use std::borrow::Cow;

use super::*;
use crate::manifest::{
    AbilityManifest, AbilityPromptConfig, DomainManifest, DomainPromptConfig, PromptConfig,
};
use crate::manifest::{ContextBlockManifest, ManifestLoader, RoutineManifest};
use nenjo_knowledge::tools::{KnowledgePackEntry, KnowledgeRegistry};
use nenjo_knowledge::{
    KnowledgeDocFilter, KnowledgeDocManifest, KnowledgePack, KnowledgePackManifestData,
};

struct MockProvider;

#[async_trait::async_trait]
impl nenjo_models::ModelProvider for MockProvider {
    async fn chat(
        &self,
        _request: nenjo_models::ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<nenjo_models::ChatResponse> {
        Ok(nenjo_models::ChatResponse {
            text: Some("mock".into()),
            tool_calls: vec![],
            usage: nenjo_models::TokenUsage::default(),
        })
    }
}

struct MockFactory;

impl ModelProviderFactory for MockFactory {
    fn create(&self, _name: &str) -> Result<Arc<dyn nenjo_models::ModelProvider>> {
        Ok(Arc::new(MockProvider))
    }
}

struct StaticLoader(Manifest);

#[async_trait::async_trait]
impl ManifestLoader for StaticLoader {
    async fn load(&self) -> Result<Manifest> {
        Ok(self.0.clone())
    }
}

fn test_manifest() -> Manifest {
    let model = ModelManifest {
        id: Uuid::new_v4(),
        name: "m".into(),
        description: None,
        model: "mock".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
    };
    let agent = AgentManifest {
        id: Uuid::new_v4(),
        name: "agent".into(),
        slug: Some(Slug::derive("agent")),
        description: Some("test".into()),
        prompt_config: PromptConfig::default(),
        color: None,
        model: Some(crate::manifest::model_manifest_slug(
            &model.model_provider,
            &model.model,
        )),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![ProjectManifest {
            id: Uuid::new_v4(),
            name: "p".into(),
            slug: Slug::derive("p"),
            description: None,
            settings: serde_json::Value::Null,
        }],
        ..Default::default()
    }
}

#[derive(Clone)]
struct TestKnowledgePack {
    manifest: KnowledgePackManifestData,
    content: String,
}

impl TestKnowledgePack {
    fn new(pack_id: &str, root_uri: &str, doc_id: &str, path: &str) -> Self {
        Self {
            manifest: KnowledgePackManifestData {
                pack_id: pack_id.to_string(),
                version: "1".to_string(),
                schema_version: 1,
                root_uri: root_uri.to_string(),
                content_hash: format!("{pack_id}-hash"),
                docs: vec![KnowledgeDocManifest {
                    id: doc_id.to_string(),
                    selector: path.to_string(),
                    source_path: path.to_string(),
                    title: doc_id.to_string(),
                    summary: format!("{doc_id} summary"),
                    kind: nenjo_knowledge::KnowledgeDocKind::new("guide"),
                    tags: vec![],
                    related: vec![],
                    updated_at: String::new(),
                }],
            },
            content: format!("{doc_id} body"),
        }
    }
}

impl KnowledgePack for TestKnowledgePack {
    fn manifest(&self) -> &dyn nenjo_knowledge::KnowledgePackManifest {
        &self.manifest
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>> {
        (manifest.id == self.manifest.docs[0].id).then_some(Cow::Borrowed(self.content.as_str()))
    }
}

#[tokio::test]
async fn from_manifest_and_agent_slug_lookup() {
    let manifest = test_manifest();
    let slug = manifest.agents[0].slug.clone().unwrap();
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    assert!(provider.agent(&slug).await.is_ok());
    assert!(provider.agent("missing").await.is_err());
}

#[tokio::test]
async fn manifest_index_uses_agent_slug_not_name_when_present() {
    let mut manifest = test_manifest();
    manifest.agents[0].name = "Display Agent".into();
    manifest.agents[0].slug = Some(Slug::derive("worker"));

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    assert!(provider.agent("worker").await.is_ok());
    assert!(provider.agent("display-agent").await.is_err());
}

#[tokio::test]
async fn manifest_index_finds_abilities_and_domains_without_scanning() {
    let mut manifest = test_manifest();
    let ability = AbilityManifest {
        id: Uuid::new_v4(),
        name: "Code Review".into(),
        path: Some("review".into()),
        description: None,
        activation_condition: "when code needs review".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "review code".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let domain = DomainManifest {
        id: Uuid::new_v4(),
        name: "creator".into(),
        path: "nenjo".into(),
        description: None,
        command: "#creator".into(),
        platform_scopes: vec![],
        abilities: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        prompt_config: DomainPromptConfig::default(),
    };
    let domain_slug = domain.slug();
    manifest.abilities.push(ability.clone());
    manifest.domains.push(domain.clone());

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    assert_eq!(provider.find_ability("Code Review").unwrap().id, ability.id);
    assert_eq!(
        provider.find_domain(domain_slug.as_str()).unwrap().id,
        domain.id
    );
    assert_eq!(provider.find_domain("creator").unwrap().id, domain.id);
    assert_eq!(provider.find_domain("#creator").unwrap().id, domain.id);
}

#[tokio::test]
async fn project_context_renders_template_and_knowledge_vars() {
    let mut manifest = test_manifest();
    manifest.agents[0].prompt_config.system_prompt = "{{ project.context }}".into();
    manifest.projects[0].settings = serde_json::json!({
        "context": "Project {{ project.name }}: {{ lib.product }}"
    });
    let project = manifest.projects[0].clone();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .with_knowledge_packs([KnowledgePackEntry::library(
            "product",
            TestKnowledgePack::new(
                "product",
                "library://product/",
                "first_doc",
                "library://product/first.md",
            ),
        )
        .unwrap()])
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("agent")
        .await
        .unwrap()
        .with_project_context(&project)
        .build()
        .await
        .unwrap();

    let prompts = runner
        .instance()
        .build_prompts(&crate::input::AgentRun::chat(crate::input::ChatInput::new(
            "hello",
        )));

    assert!(prompts.system.contains("Project p:"));
    assert!(prompts.system.contains("<knowledge_pack"));
    assert!(prompts.system.contains("first_doc summary"));
}

#[tokio::test]
async fn provider_registers_multiple_knowledge_packs() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .with_knowledge_packs([
            KnowledgePackEntry::local(
                "first",
                TestKnowledgePack::new(
                    "first",
                    "local://first/",
                    "first_doc",
                    "local://first/first.md",
                ),
            )
            .unwrap(),
            KnowledgePackEntry::local(
                "second",
                TestKnowledgePack::new(
                    "second",
                    "local://second/",
                    "second_doc",
                    "local://second/second.md",
                ),
            )
            .unwrap(),
        ])
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();
    let tool_names = runner
        .instance()
        .tool_specs()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(tool_names.iter().any(|name| name == "list_knowledge_packs"));

    let vars = runner
        .instance()
        .prompt_context()
        .render_ctx_extra
        .to_vars();
    assert!(vars.contains_key("local.first"));
    assert!(vars.contains_key("local.second"));

    let registry = provider.inner.services.knowledge_registry.clone();
    let packs = registry.list_packs().await.unwrap();
    assert_eq!(packs.len(), 2);
    assert!(packs.iter().any(|pack| pack.pack == "local:first"));
    assert!(packs.iter().any(|pack| pack.pack == "local:second"));

    let first = registry.resolve_pack("local:first").await.unwrap();
    assert!(
        first
            .list_docs(KnowledgeDocFilter::default())
            .iter()
            .any(|doc| doc.id == "first_doc")
    );
}

#[tokio::test]
async fn builder_via_loader() {
    let manifest = test_manifest();
    let slug = manifest.agents[0].slug.clone().unwrap();

    let provider = Provider::builder()
        .with_loader(StaticLoader(manifest))
        .with_model_factory(MockFactory)
        .build()
        .await
        .unwrap();

    assert!(provider.agent(&slug).await.is_ok());
}

#[tokio::test]
async fn blank_provider_builds_without_manifest_or_factories() {
    let provider = Provider::builder().build().await.unwrap();

    assert!(provider.manifest().agents.is_empty());
    assert!(provider.agent("missing").await.is_err());
}

#[tokio::test]
async fn new_agent_uses_provider_model_factory() {
    let manifest = test_manifest();
    let agent = manifest.agents[0].clone();
    let model = manifest.models[0].clone();

    let runner = Provider::builder()
        .with_model_factory(MockFactory)
        .build()
        .await
        .unwrap()
        .new_agent()
        .with_agent_manifest(agent)
        .with_model_manifest(model)
        .build()
        .await
        .unwrap();

    assert_eq!(runner.agent_name(), "agent");
}

#[tokio::test]
async fn builder_can_preserve_typed_model_factory() {
    let manifest = test_manifest();
    let slug = manifest.agents[0].slug.clone().unwrap();

    let provider: Provider<MockFactory, NoopToolFactory, builder::NoMemory> = Provider::builder()
        .with_loader(StaticLoader(manifest))
        .with_model_factory(MockFactory)
        .build()
        .await
        .unwrap();

    assert!(provider.agent(&slug).await.is_ok());
}

#[tokio::test]
async fn multiple_loaders_merge() {
    let manifest = test_manifest();

    let local = Manifest {
        context_blocks: vec![ContextBlockManifest {
            id: Uuid::new_v4(),
            name: "local_block".into(),
            path: "local".into(),
            description: None,
            template: "local content".into(),
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_loader(StaticLoader(manifest))
        .with_loader(StaticLoader(local))
        .with_model_factory(MockFactory)
        .build()
        .await
        .unwrap();

    assert_eq!(provider.manifest().agents.len(), 1);
    assert!(
        provider
            .manifest()
            .context_blocks
            .iter()
            .any(|b| b.name == "local_block")
    );
}

#[tokio::test]
async fn agent_without_model_fails() {
    let mut manifest = test_manifest();
    manifest.agents[0].model = None;

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    assert!(provider.agent("agent").await.is_err());
}

#[tokio::test]
async fn routine_runner_keeps_manifest_snapshot_after_provider_update() {
    let model = ModelManifest {
        id: Uuid::new_v4(),
        name: "m".into(),
        description: None,
        model: "mock".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
    };
    let original_agent_id = Uuid::new_v4();
    let updated_agent_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();

    let original_agent = AgentManifest {
        id: original_agent_id,
        name: "agent-old".into(),
        slug: None,
        description: Some("old".into()),
        prompt_config: PromptConfig::default(),
        color: None,
        model: Some(crate::manifest::model_manifest_slug(
            &model.model_provider,
            &model.model,
        )),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    let updated_agent = AgentManifest {
        id: updated_agent_id,
        name: "agent-new".into(),
        slug: None,
        description: Some("new".into()),
        prompt_config: PromptConfig::default(),
        color: None,
        model: Some(crate::manifest::model_manifest_slug(
            &model.model_provider,
            &model.model,
        )),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    let routine = RoutineManifest {
        id: routine_id,
        name: "routine".into(),
        slug: None,
        description: None,
        trigger: crate::manifest::RoutineTrigger::Task,
        metadata: crate::manifest::RoutineMetadata::default(),
        steps: vec![crate::manifest::RoutineStepManifest {
            id: step_id,
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("routine"),
            name: "step".into(),
            step_type: crate::manifest::RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("agent-old")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let original_manifest = Manifest {
        agents: vec![original_agent.clone()],
        models: vec![model.clone()],
        routines: vec![routine.clone()],
        projects: vec![ProjectManifest {
            id: Uuid::new_v4(),
            name: "p".into(),
            slug: Slug::derive("p"),
            description: None,
            settings: serde_json::Value::Null,
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(original_manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let original_runner = provider.routine("routine").unwrap();

    let mut updated_manifest = provider.manifest().clone();
    updated_manifest.agents = vec![updated_agent.clone()];
    updated_manifest.routines[0].steps[0].agent = Some(Slug::derive("agent-new"));

    let updated_provider = provider.with_manifest(updated_manifest);
    let updated_runner = updated_provider.routine("routine").unwrap();

    assert_eq!(
        original_runner.routine().steps[0].agent,
        Some(Slug::derive("agent-old"))
    );
    assert_eq!(
        updated_runner.routine().steps[0].agent,
        Some(Slug::derive("agent-new"))
    );
    assert_eq!(
        original_runner.provider().manifest().agents[0].name,
        "agent-old"
    );
    assert_eq!(
        updated_runner.provider().manifest().agents[0].name,
        "agent-new"
    );
}
