use std::borrow::Cow;

use super::*;
use crate::ManifestWriter;
use crate::manifest::{
    AbilityManifest, AbilityPromptConfig, DomainManifest, DomainPromptConfig,
    KnowledgePackManifest as ProviderKnowledgePackManifest, KnowledgePackSource, PromptConfig,
};
use crate::manifest::{ContextBlockManifest, ManifestLoader, ManifestResource, RoutineManifest};
use crate::{ArgumentValueType, ResolvedArgumentBinding};
use std::sync::Arc;

use nenjo_knowledge::tools::KnowledgePackEntry;
use nenjo_knowledge::{KnowledgeDocManifest, KnowledgePack, KnowledgePackManifestData};

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
            provider_tool_calls: vec![],
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
        name: "m".into(),
        slug: crate::manifest::model_manifest_slug("mock", "mock"),
        description: None,
        model: "mock".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
        native_tools: vec![],
    };
    let agent = AgentManifest {
        name: "agent".into(),
        slug: Slug::derive("agent"),
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
        media: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![ProjectManifest {
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
    let slug = manifest.agents[0].slug.clone();
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
    manifest.agents[0].slug = Slug::derive("worker");

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
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let domain = DomainManifest {
        name: "creator".into(),
        path: "nenjo".into(),
        description: None,
        command: "#creator".into(),
        platform_scopes: vec![],
        abilities: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
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

    assert_eq!(
        provider.find_ability("Code Review").unwrap().name,
        ability.name
    );
    assert_eq!(
        provider.find_domain(domain_slug.as_str()).unwrap().name,
        domain.name
    );
    assert_eq!(provider.find_domain("creator").unwrap().name, domain.name);
    assert_eq!(provider.find_domain("#creator").unwrap().name, domain.name);
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
async fn task_prompt_project_context_renders_into_project_xml() {
    let mut manifest = test_manifest();
    manifest.agents[0].prompt_config.templates.task_execution =
        "{{ project.context }}\n{{ project }}".into();
    manifest.projects[0].settings = serde_json::json!({
        "context": "Project {{ project.name }} context"
    });
    let project = manifest.projects[0].clone();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
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
        .build_prompts(&crate::input::AgentRun::task(
            crate::input::TaskInput::new("Task", "Description").with_project("p"),
        ));

    assert!(prompts.user_message.contains("Project p context"));
    assert!(
        prompts
            .user_message
            .contains("<context>Project p context</context>")
    );
    assert!(!prompts.user_message.contains("{{ project.name }}"));
}

#[tokio::test]
async fn task_prompt_does_not_append_routine_handoffs_twice() {
    let mut manifest = test_manifest();
    manifest.agents[0].prompt_config.templates.task_execution =
        "{{ routine }}\n{{ task.description }}".into();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let routine_context = crate::context::RoutineContext {
        name: "Pipeline".into(),
        slug: "pipeline".into(),
        execution_id: "run-1".into(),
        description: None,
        step: crate::context::RoutineStepContext {
            name: "implement".into(),
            step_type: "agent".into(),
            ..Default::default()
        },
        handoffs: crate::context::RoutineHandoffsContext {
            items: vec![crate::context::RoutineHandoffContext {
                source_step: "plan".into(),
                target_step: "implement".into(),
                summary: Some("Plan ready".into()),
                payload: r#"{"work":"build"}"#.into(),
                ..Default::default()
            }],
        },
    };

    let runner = provider
        .agent("agent")
        .await
        .unwrap()
        .with_routine_context(routine_context)
        .build()
        .await
        .unwrap();

    let prompts = runner
        .instance()
        .build_prompts(&crate::input::AgentRun::task(crate::input::TaskInput::new(
            "Task",
            "Description",
        )));

    assert_eq!(prompts.user_message.matches("<handoffs>").count(), 1);
    assert!(!prompts.user_message.contains("# Routine Handoffs"));
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

    assert!(tool_names.iter().any(|name| name == "search_knowledge"));
    assert!(tool_names.iter().any(|name| name == "read_knowledge_doc"));
}

#[tokio::test]
async fn manifest_knowledge_pack_provides_prompt_vars_and_lazy_doc_reads() {
    let temp = tempfile::tempdir().unwrap();
    let pack_dir = temp.path().join("demo");
    let docs_dir = pack_dir.join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    std::fs::write(docs_dir.join("intro.md"), "# Intro\n\nLoaded from disk.").unwrap();
    std::fs::write(
        pack_dir.join("manifest.json"),
        r#"{
          "pack_id": "demo",
          "version": "1",
          "schema_version": 1,
          "root_uri": "library://demo/",
          "content_hash": "",
          "docs": [
            {
              "id": "intro",
              "selector": "intro",
              "source_path": "docs/intro.md",
              "title": "Intro",
              "summary": "Intro summary",
              "kind": "reference",
              "tags": [],
              "related": [],
              "updated_at": ""
            }
          ]
        }"#,
    )
    .unwrap();

    let mut manifest = test_manifest();
    manifest
        .knowledge_packs
        .push(ProviderKnowledgePackManifest {
            slug: crate::Slug::derive("demo"),
            name: "Demo".to_string(),
            description: None,
            source_type: KnowledgePackSource::Library,
            selector: "lib:demo".to_string(),
            version: Some("1".to_string()),
            root_uri: "library://demo/".to_string(),
            root_path: Some(pack_dir),
            read_only: true,
            metadata: serde_json::Value::Null,
        });

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
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
    let vars = runner
        .instance()
        .prompt_context()
        .render_ctx_extra
        .knowledge_vars
        .clone();
    assert!(vars.contains_key("lib.demo.intro"));

    let read_tool = provider
        .create_knowledge_tools()
        .into_iter()
        .find(|tool| tool.name() == "read_knowledge_doc")
        .unwrap();
    let result = read_tool
        .execute(serde_json::json!({ "pack": "lib:demo", "selector": "intro" }))
        .await
        .unwrap();
    assert!(result.output.contains("Loaded from disk."));
}

#[tokio::test]
async fn provider_exposes_list_knowledge_packs_without_registered_packs() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let tools = provider.create_knowledge_tools();
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert_eq!(names, vec!["list_knowledge_packs"]);

    let result = tools[0].execute(serde_json::json!({})).await.unwrap();
    assert!(result.success);
    let packs: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
    assert!(packs.is_empty());
}

#[tokio::test]
async fn live_manifest_reader_refreshes_existing_knowledge_tools() {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::manifest::local::LocalManifestStore::new(temp.path().join("manifests"));
    let pack_dir = temp.path().join("library").join("live");

    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockFactory)
        .with_tool_factory(NoopToolFactory)
        .with_live_manifest_reader(store.clone())
        .build()
        .await
        .unwrap();

    let tools = provider.create_knowledge_tools();
    assert!(tools.iter().any(|tool| tool.name() == "read_knowledge_doc"));
    let list_tool = tools
        .iter()
        .find(|tool| tool.name() == "list_knowledge_packs")
        .unwrap()
        .clone();

    let packs: Vec<serde_json::Value> = serde_json::from_str(
        &list_tool
            .execute(serde_json::json!({}))
            .await
            .unwrap()
            .output,
    )
    .unwrap();
    assert!(packs.is_empty());

    store
        .upsert_resource(&ManifestResource::KnowledgePack(
            ProviderKnowledgePackManifest {
                slug: crate::Slug::derive("live"),
                name: "Live".to_string(),
                description: None,
                source_type: KnowledgePackSource::Library,
                selector: "lib:live".to_string(),
                version: Some("1".to_string()),
                root_uri: "library://live/".to_string(),
                root_path: Some(pack_dir),
                read_only: true,
                metadata: serde_json::Value::Null,
            },
        ))
        .await
        .unwrap();

    let packs: Vec<serde_json::Value> = serde_json::from_str(
        &list_tool
            .execute(serde_json::json!({}))
            .await
            .unwrap()
            .output,
    )
    .unwrap();
    assert_eq!(packs[0]["selector"], "lib:live");
}

#[tokio::test]
async fn builder_via_loader() {
    let manifest = test_manifest();
    let slug = manifest.agents[0].slug.clone();

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
    let slug = manifest.agents[0].slug.clone();

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
async fn provider_argument_binding_renders_before_context_blocks() {
    let mut manifest = test_manifest();
    manifest.context_blocks.push(ContextBlockManifest {
        name: "company".into(),
        path: String::new(),
        description: None,
        template: "{{ args.company }}".into(),
    });
    manifest.agents[0].prompt_config.system_prompt = "{{ context.company }}".into();
    let company = ResolvedArgumentBinding::new(
        "support-app",
        "company_context",
        "args.company",
        ArgumentValueType::Xml,
        "<company>Acme</company>",
    )
    .unwrap();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
        .with_argument_bindings([company])
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

    let prompts = runner
        .instance()
        .try_build_prompts(&crate::input::AgentRun::chat(crate::input::ChatInput::new(
            "hello",
        )))
        .unwrap();

    assert_eq!(prompts.system, "<company>Acme</company>");
}

#[tokio::test]
async fn execution_argument_binding_renders_user_value() {
    let mut manifest = test_manifest();
    manifest.agents[0].prompt_config.system_prompt = "{{ args.profile }}".into();
    let profile = ResolvedArgumentBinding::new(
        "support-app",
        "user_context",
        "args.profile",
        ArgumentValueType::Xml,
        "<user>Ada</user>",
    )
    .unwrap();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
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
    let run = crate::input::AgentRun::chat(crate::input::ChatInput::new("hello"))
        .argument_bindings([profile]);

    let prompts = runner.instance().try_build_prompts(&run).unwrap();

    assert_eq!(prompts.system, "<user>Ada</user>");
}

#[tokio::test]
async fn missing_argument_binding_fails_prompt_build() {
    let mut manifest = test_manifest();
    manifest.agents[0].prompt_config.system_prompt = "{{ args.profile }}".into();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory)
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

    let error = runner
        .instance()
        .try_build_prompts(&crate::input::AgentRun::chat(crate::input::ChatInput::new(
            "hello",
        )))
        .unwrap_err();

    assert!(error.to_string().contains("missing runtime argument"));
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
        name: "m".into(),
        slug: Slug::derive("m"),
        description: None,
        model: "mock".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
        native_tools: vec![],
    };
    let original_agent = AgentManifest {
        name: "agent-old".into(),
        slug: Slug::derive("agent-old"),
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
        media: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    let updated_agent = AgentManifest {
        name: "agent-new".into(),
        slug: Slug::derive("agent-new"),
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
        media: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    let routine = RoutineManifest {
        name: "routine".into(),
        slug: Slug::derive("routine"),
        description: None,
        trigger: crate::manifest::RoutineTrigger::Task,
        metadata: crate::manifest::RoutineMetadata::default(),
        steps: vec![crate::manifest::RoutineStepManifest {
            slug: Slug::derive("step"),
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
