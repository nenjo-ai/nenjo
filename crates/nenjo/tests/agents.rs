//! Tests for AgentBuilder and AgentRunner.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, DomainManifest, Manifest, ModelManifest,
    ProjectManifest, PromptConfig, PromptTemplates, model_manifest_slug,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider, ToolFactory};
use nenjo::types::{AbilityPromptConfig, DomainPromptConfig};
use nenjo::{Slug, Tool, ToolCategory, ToolResult};
use nenjo_models::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage};

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

struct MockProvider {
    response_text: String,
}

impl MockProvider {
    fn new(text: &str) -> Self {
        Self {
            response_text: text.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some(self.response_text.clone()),
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            },
        })
    }

    fn context_window(&self, _model: &str) -> Option<usize> {
        Some(128_000)
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_developer_role(&self, _model: &str) -> bool {
        true
    }
}

struct MockModelProviderFactory {
    response_text: String,
}

impl MockModelProviderFactory {
    fn new(text: &str) -> Self {
        Self {
            response_text: text.to_string(),
        }
    }
}

impl ModelProviderFactory for MockModelProviderFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(MockProvider::new(&self.response_text)))
    }
}

struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes back the input"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let msg = args["message"].as_str().unwrap_or("no message");
        Ok(ToolResult {
            success: true,
            output: format!("echo: {msg}"),
            error: None,
        })
    }
}

struct EchoToolFactory;

#[async_trait::async_trait]
impl ToolFactory for EchoToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(EchoTool)]
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_manifest() -> Manifest {
    let model = ModelManifest {
        id: Uuid::new_v4(),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
    };

    let agent = AgentManifest {
        id: Uuid::new_v4(),
        name: "test-coder".into(),
        slug: None,
        description: Some("A test coding agent".into()),
        prompt_config: PromptConfig {
            system_prompt: "You are a helpful coding assistant.".into(),
            developer_prompt: "Focus on writing clean, idiomatic Rust.".into(),
            templates: PromptTemplates {
                task_execution:
                    "Execute the following task:\n\nTitle: {{ task.title }}\nDescription: {{ task.description }}"
                        .into(),
                chat_task: "{{ chat.message }}".into(),
                gate_eval:
                    "Evaluate:\n{{ routine.step.instructions }}\n\nPrevious output:\n{{ gate.previous_output }}"
                        .into(),
                heartbeat_task: String::new(),
            },
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug(&model.model_provider, &model.model)),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![ProjectManifest {
            id: Uuid::new_v4(),
            name: "test-project".into(),
            slug: Slug::derive("test-project"),
            description: Some("A test project".into()),
            settings: serde_json::Value::Null,
        }],
        context_blocks: vec![ContextBlockManifest {
            id: Uuid::new_v4(),
            name: "agent_notes".into(),
            path: "nenjo".into(),
            display_name: None,
            description: None,
            template: "<agent_notes>\n{{items}}\n</agent_notes>".into(),
        }],
        ..Default::default()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[tokio::test]
async fn runner_chat() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory::new("Hello from the mock LLM!"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();
    let output = runner.chat("Hi there").await.expect("chat should succeed");

    assert_eq!(output.text, "Hello from the mock LLM!");
    assert_eq!(output.input_tokens, 100);
    assert_eq!(output.output_tokens, 50);
    assert_eq!(output.tool_calls, 0);
    assert!(
        !output.messages.is_empty(),
        "should have conversation messages"
    );
}

#[tokio::test]
async fn runner_chat_with_history() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory::new(
            "I remember our conversation.",
        ))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let history = vec![
        ChatMessage::user("What's 2+2?"),
        ChatMessage::assistant("4"),
    ];

    let output = runner
        .chat_with_history("And what's 3+3?", history)
        .await
        .expect("chat_with_history should succeed");

    assert_eq!(output.text, "I remember our conversation.");
}

#[tokio::test]
async fn runner_with_custom_tool() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory::new("Done!"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-coder")
        .await
        .unwrap()
        .with_tool(EchoTool)
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "echo");

    let output = runner.chat("Use the echo tool").await.unwrap();
    assert_eq!(output.text, "Done!");
}

#[tokio::test]
async fn runner_with_tool_factory() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory::new("Tool factory works!"))
        .with_tool_factory(EchoToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "echo");

    let output = runner.chat("Hello").await.unwrap();
    assert_eq!(output.text, "Tool factory works!");
}

#[tokio::test]
async fn instance_builds_prompts() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory::new("irrelevant"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-coder")
        .await
        .unwrap()
        .with_memory_vars(std::collections::HashMap::from([(
            "memories".to_string(),
            "<memories>test memory</memories>".to_string(),
        )]))
        .build()
        .await
        .unwrap();

    let task = nenjo::AgentRun::chat(nenjo::ChatInput {
        message: "Hello!".into(),
        history: vec![],
        project: None,
    });

    let prompts = runner.instance().build_prompts(&task);

    assert!(
        prompts.system.contains("helpful coding assistant"),
        "system prompt should contain configured text, got: {}",
        prompts.system
    );

    assert!(
        prompts.developer.contains("clean, idiomatic Rust"),
        "developer prompt should contain configured text, got: {}",
        prompts.developer
    );
}

#[tokio::test]
async fn instance_uses_heartbeat_template_for_heartbeat_runs() {
    let mut manifest = test_manifest();
    manifest.agents[0].prompt_config.templates.heartbeat_task =
        "HEARTBEAT {{ heartbeat.previous_output }} {{ agent.name }}".into();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("irrelevant"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let run = nenjo::AgentRun {
        kind: nenjo::AgentRunKind::Heartbeat(nenjo::HeartbeatInput {
            agent: Slug::derive("test-coder"),
            interval: std::time::Duration::from_secs(60),
            start_at: None,
            previous_output: Some("previous heartbeat output".into()),
            last_run_at: None,
            next_run_at: None,
        }),
        execution: nenjo::ExecutionOptions::default(),
    };

    let prompts = runner.instance().build_prompts(&run);

    assert!(
        prompts
            .user_message
            .contains("HEARTBEAT previous heartbeat output test-coder"),
        "heartbeat runs should render the heartbeat template, got: {}",
        prompts.user_message
    );
}

#[tokio::test]
async fn instance_renders_self_prompt_var() {
    let mut manifest = test_manifest();
    manifest.agents[0].prompt_config.system_prompt = "{{ self }}".into();
    manifest.agents[0].prompt_config.developer_prompt =
        "{{ agent.slug }}|{{ agent.role }}|{{ agent.name }}|{{ agent.model }}|{{ agent.description }}"
            .into();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("irrelevant"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let task = nenjo::AgentRun::chat(nenjo::ChatInput {
        message: "Hello!".into(),
        history: vec![],
        project: None,
    });
    let prompts = runner.instance().build_prompts(&task);

    assert!(prompts.system.contains("<agent "));
    assert!(prompts.system.contains("slug=\"test-coder\""));
    assert!(prompts.system.contains("name=\"test-coder\""));
    assert!(prompts.system.contains("llm_model_name=\"mock-llm-v1\""));
    assert!(
        prompts
            .system
            .contains("description=\"A test coding agent\"")
    );
    assert_eq!(
        prompts.developer,
        "test-coder||test-coder|mock-llm-v1|A test coding agent"
    );
}

// ---------------------------------------------------------------------------
// Helpers for ability/domain tool exposure tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Helpers for ability/domain tests
// ---------------------------------------------------------------------------

fn ability_manifest(name: &str, scopes: Vec<&str>) -> AbilityManifest {
    AbilityManifest {
        id: Uuid::new_v4(),
        name: name.into(),
        path: None,
        description: Some(format!("{name} ability")),
        activation_condition: format!("when {name} is needed"),
        prompt_config: AbilityPromptConfig {
            developer_prompt: format!("You are the {name} ability."),
        },
        platform_scopes: scopes.into_iter().map(String::from).collect(),
        mcp_servers: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    }
}

fn domain_manifest_with_config(
    name: &str,
    developer_prompt_addon: Option<&str>,
    platform_scopes: Vec<String>,
    abilities: Vec<String>,
    mcp_servers: Vec<Slug>,
) -> DomainManifest {
    DomainManifest {
        id: Uuid::new_v4(),
        name: name.into(),
        path: String::new(),
        display_name: name.into(),
        description: Some(format!("{name} domain")),
        command: name.into(),
        platform_scopes,
        abilities,
        mcp_servers,
        prompt_config: DomainPromptConfig {
            developer_prompt_addon: developer_prompt_addon.map(str::to_string),
        },
    }
}

fn manifest_with_abilities_and_domains(
    agent_abilities: Vec<String>,
    agent_domains: Vec<Slug>,
    agent_scopes: Vec<&str>,
    abilities: Vec<AbilityManifest>,
    domains: Vec<DomainManifest>,
) -> Manifest {
    let model = ModelManifest {
        id: Uuid::new_v4(),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
    };

    let agent = AgentManifest {
        id: Uuid::new_v4(),
        name: "test-agent".into(),
        slug: None,
        description: Some("Test agent".into()),
        prompt_config: PromptConfig {
            system_prompt: "You are a test agent.".into(),
            developer_prompt: "Be helpful.".into(),
            templates: PromptTemplates {
                task_execution: String::new(),
                chat_task: "{{ chat.message }}".into(),
                gate_eval: String::new(),
                heartbeat_task: String::new(),
            },
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug(&model.model_provider, &model.model)),
        domains: agent_domains,
        platform_scopes: agent_scopes.into_iter().map(String::from).collect(),
        mcp_servers: vec![],
        abilities: agent_abilities,
        prompt_locked: false,
        heartbeat: None,
    };

    Manifest {
        agents: vec![agent],
        models: vec![model],
        abilities,
        domains,
        projects: vec![ProjectManifest {
            id: Uuid::new_v4(),
            name: "test-project".into(),
            slug: Slug::derive("test-project"),
            description: None,
            settings: serde_json::Value::Null,
        }],
        ..Default::default()
    }
}

// ===========================================================================
// Ability scope tests
// ===========================================================================

#[tokio::test]
async fn ability_agent_has_ability_invoke_tool_only() {
    let ability = ability_manifest("writer", vec!["agents:write"]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.name.clone()],
        vec![],
        vec!["projects:read"],
        vec![ability],
        vec![],
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(
        tool_names.contains(&"list_assigned_abilities"),
        "base agent should have ability discovery tool, got: {tool_names:?}"
    );
    assert!(
        tool_names.contains(&"use_ability"),
        "base agent should have ability invocation tool, got: {tool_names:?}"
    );
    assert!(
        !tool_names.contains(&"writer"),
        "assigned ability name should not be a top-level tool, got: {tool_names:?}"
    );

    let use_ability_spec = specs
        .iter()
        .find(|spec| spec.name == "use_ability")
        .expect("missing use_ability tool");
    assert!(
        !use_ability_spec.description.contains("writer ability"),
        "use_ability description should not include ability description, got: {}",
        use_ability_spec.description
    );
    assert!(
        !use_ability_spec
            .description
            .contains("when writer is needed"),
        "use_ability description should not include activation condition, got: {}",
        use_ability_spec.description
    );
}

#[tokio::test]
async fn duplicate_ability_ids_are_rejected() {
    let mut frontend = ability_manifest("review", vec!["projects:read"]);
    frontend.path = Some("frontend".into());
    let frontend_name = frontend.name.clone();

    let mut backend = ability_manifest("review", vec!["projects:read"]);
    backend.path = Some("backend".into());
    let backend_name = backend.name.clone();

    let manifest = manifest_with_abilities_and_domains(
        vec![frontend_name, backend_name],
        vec![],
        vec!["projects:read"],
        vec![frontend, backend],
        vec![],
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider.agent("test-agent").await.unwrap().build().await;

    let error = match runner {
        Ok(_) => panic!("duplicate ability ids should fail agent build"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("duplicate ability_id 'review'"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn agent_without_abilities_has_no_ability_tools() {
    let manifest =
        manifest_with_abilities_and_domains(vec![], vec![], vec!["projects:read"], vec![], vec![]);

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        !tool_names.contains(&"writer"),
        "agent without abilities should not have ability tools, got: {tool_names:?}"
    );
    assert!(
        !tool_names.contains(&"list_assigned_abilities") && !tool_names.contains(&"use_ability"),
        "agent without abilities should not have ability broker tools, got: {tool_names:?}"
    );
}

// ===========================================================================
// Domain expansion tests
// ===========================================================================

#[tokio::test]
async fn domain_expansion_preserves_tool_set() {
    let domain = domain_manifest_with_config(
        "creator",
        Some("Creator mode enabled."),
        vec![],
        vec![],
        vec![],
    );
    let manifest = manifest_with_abilities_and_domains(
        vec![],
        vec![Slug::derive(&domain.name)],
        vec!["projects:read"],
        vec![],
        vec![domain],
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("creator").await.unwrap();
    let before_specs = runner.instance().tool_specs();
    let after_specs = domain_runner.instance().tool_specs();
    let before_names: Vec<&str> = before_specs.iter().map(|s| s.name.as_str()).collect();
    let after_names: Vec<&str> = after_specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        before_names, after_names,
        "domain expansion should not change tools"
    );

    assert!(
        domain_runner
            .instance()
            .prompt_context()
            .active_domain
            .as_ref()
            .and_then(|domain| domain
                .manifest
                .prompt_config
                .developer_prompt_addon
                .as_deref())
            == Some("Creator mode enabled."),
        "domain should expose the configured prompt addon"
    );
}

#[tokio::test]
async fn domain_expansion_preserves_existing_abilities() {
    let ability = ability_manifest("code-review", vec!["projects:write"]);
    let domain =
        domain_manifest_with_config("reviewer", Some("Review mode"), vec![], vec![], vec![]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.name.clone()],
        vec![Slug::derive(&domain.name)],
        vec!["projects:read"],
        vec![ability.clone()],
        vec![domain],
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(tool_names.contains(&"list_assigned_abilities"));
    assert!(tool_names.contains(&"use_ability"));
    assert!(!tool_names.contains(&"code_review"));

    let domain_runner = runner.domain_expansion("reviewer").await.unwrap();
    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"list_assigned_abilities") && tool_names.contains(&"use_ability"),
        "domain expansion should preserve ability broker tools, got: {tool_names:?}"
    );
    assert!(!tool_names.contains(&"code_review"));

    // Ability availability is exposed through the broker tools, not prompt context.
}

#[tokio::test]
async fn domain_expansion_appends_prompt_addon_without_changing_abilities() {
    let ability = ability_manifest("deployer", vec!["projects:read"]);
    let domain = domain_manifest_with_config("ops", Some("Ops mode"), vec![], vec![], vec![]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.name.clone()],
        vec![Slug::derive(&domain.name)],
        vec!["projects:read"],
        vec![ability.clone()],
        vec![domain],
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("ops").await.unwrap();
    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(tool_names.contains(&"list_assigned_abilities"));
    assert!(tool_names.contains(&"use_ability"));
    assert!(!tool_names.contains(&"deployer"));
    assert_eq!(
        domain_runner
            .instance()
            .prompt_context()
            .active_domain
            .as_ref()
            .and_then(|domain| domain
                .manifest
                .prompt_config
                .developer_prompt_addon
                .as_deref()),
        Some("Ops mode")
    );
    let prompts = domain_runner
        .instance()
        .build_prompts(&nenjo::AgentRun::chat(nenjo::ChatInput {
            message: "Deploy it".into(),
            history: vec![],
            project: None,
        }));
    assert!(
        prompts.developer.contains("Ops mode"),
        "domain developer prompt addon must be rendered before the turn loop starts, got: {}",
        prompts.developer
    );

    // Ability availability is exposed through the broker tools, not prompt context.
}

#[tokio::test]
async fn domain_expansion_preserves_existing_ability_without_duplication() {
    let ability = ability_manifest("writer", vec!["agents:write"]);
    let domain =
        domain_manifest_with_config("creator", Some("Creator mode"), vec![], vec![], vec![]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.name.clone()], // agent already has this ability
        vec![Slug::derive(&domain.name)],
        vec!["projects:read"],
        vec![ability],
        vec![domain],
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("creator").await.unwrap();

    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        tool_names
            .iter()
            .filter(|tool_name| **tool_name == "use_ability")
            .count(),
        1,
        "should not duplicate ability broker tools"
    );
}

#[tokio::test]
async fn domain_expansion_adds_domain_scopes_and_abilities() {
    let assigned_ability = ability_manifest("writer", vec!["agents:write"]);
    let domain_ability = ability_manifest("reviewer", vec!["projects:read"]);
    let domain = domain_manifest_with_config(
        "review",
        Some("Review mode"),
        vec!["projects:write".into()],
        vec![domain_ability.name.clone()],
        vec![],
    );
    let manifest = manifest_with_abilities_and_domains(
        vec![assigned_ability.name.clone()],
        vec![Slug::derive(&domain.name)],
        vec!["projects:read"],
        vec![assigned_ability, domain_ability],
        vec![domain],
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("review").await.unwrap();

    assert_eq!(
        domain_runner
            .instance()
            .prompt_context()
            .active_domain
            .as_ref()
            .map(|domain| domain.domain_name.as_str()),
        Some("review")
    );

    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(tool_names.contains(&"list_assigned_abilities"));
    assert!(tool_names.contains(&"use_ability"));
    assert!(!tool_names.contains(&"writer"));
}

#[tokio::test]
async fn domain_expansion_tracks_active_domain_without_available_context() {
    let github_id = Uuid::new_v4();
    let domain = domain_manifest_with_config(
        "github",
        Some("GitHub mode"),
        vec![],
        vec![],
        vec![Slug::derive("github")],
    );
    let mut manifest = manifest_with_abilities_and_domains(
        vec![],
        vec![Slug::derive(&domain.name)],
        vec!["projects:read"],
        vec![],
        vec![domain],
    );
    manifest
        .mcp_servers
        .push(nenjo::manifest::McpServerManifest {
            id: github_id,
            name: "github".into(),
            display_name: "GitHub".into(),
            description: Some("GitHub API".into()),
            transport: "stdio".into(),
            command: None,
            args: None,
            url: None,
            env_schema: serde_json::Value::Null,
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        });

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("github").await.unwrap();
    let active_domain = domain_runner
        .instance()
        .prompt_context()
        .active_domain
        .as_ref()
        .map(|domain| domain.domain_name.as_str());
    assert_eq!(active_domain, Some("github"));
}
