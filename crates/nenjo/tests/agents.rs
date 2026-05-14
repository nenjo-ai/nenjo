//! Tests for AgentBuilder and AgentRunner.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, DomainManifest, Manifest, ModelManifest,
    ProjectManifest, PromptConfig, PromptTemplates,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider, ToolFactory};
use nenjo::types::{AbilityPromptConfig, DomainPromptConfig};
use nenjo::{Tool, ToolCategory, ToolResult};
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
        description: Some("A test coding agent".into()),
        prompt_config: PromptConfig {
            system_prompt: "You are a helpful coding assistant.".into(),
            developer_prompt: "Focus on writing clean, idiomatic Rust.".into(),
            templates: PromptTemplates {
                task_execution:
                    "Execute the following task:\n\nTitle: {{ task.title }}\nDescription: {{ task.description }}"
                        .into(),
                chat_task: "{{ chat.message }}".into(),
                gate_eval: "Evaluate: {{ gate.criteria }}".into(),
                cron_task: String::new(),
                heartbeat_task: String::new(),
            },
            ..Default::default()
        },
        color: None,
        model_id: Some(model.id),
        domain_ids: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        ability_ids: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![ProjectManifest {
            id: Uuid::new_v4(),
            name: "test-project".into(),
            slug: "test-project".into(),
            description: Some("A test project".into()),
            settings: serde_json::Value::Null,
        }],
        context_blocks: vec![ContextBlockManifest {
            id: Uuid::new_v4(),
            name: "available_agents".into(),
            path: "nenjo".into(),
            display_name: None,
            description: None,
            template: "<available_agents>\n{{items}}\n</available_agents>".into(),
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
        .agent_by_name("test-coder")
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
        .agent_by_name("test-coder")
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
        .agent_by_name("test-coder")
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
        .agent_by_name("test-coder")
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
        .agent_by_name("test-coder")
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
        project_id: None,
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
        tool_name: name.replace('-', "_"),
        path: String::new(),
        display_name: None,
        description: Some(format!("{name} ability")),
        activation_condition: format!("when {name} is needed"),
        prompt_config: AbilityPromptConfig {
            developer_prompt: format!("You are the {name} ability."),
        },
        platform_scopes: scopes.into_iter().map(String::from).collect(),
        mcp_server_ids: vec![],
    }
}

fn domain_manifest_with_config(
    name: &str,
    developer_prompt_addon: Option<&str>,
    platform_scopes: Vec<String>,
    ability_ids: Vec<Uuid>,
    mcp_server_ids: Vec<Uuid>,
) -> DomainManifest {
    DomainManifest {
        id: Uuid::new_v4(),
        name: name.into(),
        path: String::new(),
        display_name: name.into(),
        description: Some(format!("{name} domain")),
        command: name.into(),
        platform_scopes,
        ability_ids,
        mcp_server_ids,
        prompt_config: DomainPromptConfig {
            developer_prompt_addon: developer_prompt_addon.map(str::to_string),
        },
    }
}

fn manifest_with_abilities_and_domains(
    agent_abilities: Vec<Uuid>,
    agent_domains: Vec<Uuid>,
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
        description: Some("Test agent".into()),
        prompt_config: PromptConfig {
            system_prompt: "You are a test agent.".into(),
            developer_prompt: "Be helpful.".into(),
            templates: PromptTemplates {
                task_execution: String::new(),
                chat_task: "{{ chat.message }}".into(),
                gate_eval: String::new(),
                cron_task: String::new(),
                heartbeat_task: String::new(),
            },
            ..Default::default()
        },
        color: None,
        model_id: Some(model.id),
        domain_ids: agent_domains,
        platform_scopes: agent_scopes.into_iter().map(String::from).collect(),
        mcp_server_ids: vec![],
        ability_ids: agent_abilities,
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
            slug: "test-project".into(),
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
        vec![ability.id],
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
        .agent_by_name("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(
        tool_names.contains(&"writer"),
        "base agent should have assigned ability tool, got: {tool_names:?}"
    );

    let writer_spec = specs
        .iter()
        .find(|spec| spec.name == "writer")
        .expect("missing writer ability tool");
    assert!(
        writer_spec.description.contains("writer ability"),
        "ability tool description should include ability description, got: {}",
        writer_spec.description
    );
    assert!(
        writer_spec.description.contains("when writer is needed"),
        "ability tool description should include activation condition, got: {}",
        writer_spec.description
    );
}

#[tokio::test]
async fn assigned_abilities_register_distinct_tool_names() {
    let mut frontend = ability_manifest("review", vec!["projects:read"]);
    frontend.path = "frontend".into();
    frontend.tool_name = "frontend_review".into();
    let frontend_id = frontend.id;

    let mut backend = ability_manifest("review", vec!["projects:read"]);
    backend.path = "backend".into();
    backend.tool_name = "backend_review".into();
    let backend_id = backend.id;

    let manifest = manifest_with_abilities_and_domains(
        vec![frontend_id, backend_id],
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

    let runner = provider
        .agent_by_name("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(tool_names.contains(&"frontend_review"));
    assert!(tool_names.contains(&"backend_review"));
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
        .agent_by_name("test-agent")
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
        vec![domain.id],
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
        .agent_by_name("test-agent")
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
        vec![ability.id],
        vec![domain.id],
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
        .agent_by_name("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(tool_names.contains(&"code_review"));

    let domain_runner = runner.domain_expansion("reviewer").await.unwrap();
    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"code_review"),
        "domain expansion should preserve assigned ability tools, got: {tool_names:?}"
    );

    let ability_names: Vec<&str> = domain_runner
        .instance()
        .prompt_context()
        .available_abilities
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert!(
        ability_names.contains(&"code-review"),
        "domain expansion should preserve assigned abilities, got: {ability_names:?}"
    );
}

#[tokio::test]
async fn domain_expansion_appends_prompt_addon_without_changing_abilities() {
    let ability = ability_manifest("deployer", vec!["projects:read"]);
    let domain = domain_manifest_with_config("ops", Some("Ops mode"), vec![], vec![], vec![]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.id],
        vec![domain.id],
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
        .agent_by_name("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("ops").await.unwrap();
    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(tool_names.contains(&"deployer"));
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

    let ability_names: Vec<&str> = domain_runner
        .instance()
        .prompt_context()
        .available_abilities
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert!(ability_names.contains(&"deployer"));
}

#[tokio::test]
async fn domain_expansion_preserves_existing_ability_without_duplication() {
    let ability = ability_manifest("writer", vec!["agents:write"]);
    let domain =
        domain_manifest_with_config("creator", Some("Creator mode"), vec![], vec![], vec![]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.id], // agent already has this ability
        vec![domain.id],
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
        .agent_by_name("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("creator").await.unwrap();

    let ability_count = domain_runner
        .instance()
        .prompt_context()
        .available_abilities
        .iter()
        .filter(|a| a.name == "writer")
        .count();
    assert_eq!(ability_count, 1, "should not duplicate existing abilities");
}

#[tokio::test]
async fn domain_expansion_adds_domain_scopes_and_abilities() {
    let assigned_ability = ability_manifest("writer", vec!["agents:write"]);
    let domain_ability = ability_manifest("reviewer", vec!["projects:read"]);
    let domain = domain_manifest_with_config(
        "review",
        Some("Review mode"),
        vec!["projects:write".into()],
        vec![domain_ability.id],
        vec![],
    );
    let manifest = manifest_with_abilities_and_domains(
        vec![assigned_ability.id],
        vec![domain.id],
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
        .agent_by_name("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("review").await.unwrap();

    assert!(
        domain_runner
            .instance()
            .prompt_context()
            .platform_scopes
            .iter()
            .any(|scope| scope == "projects:write")
    );

    let ability_names: Vec<&str> = domain_runner
        .instance()
        .prompt_context()
        .available_abilities
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert!(ability_names.contains(&"writer"));
    assert!(ability_names.contains(&"reviewer"));

    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(tool_names.contains(&"writer"));
}

#[tokio::test]
async fn domain_expansion_adds_domain_mcp_metadata_without_duplication() {
    let github_id = Uuid::new_v4();
    let domain = domain_manifest_with_config(
        "github",
        Some("GitHub mode"),
        vec![],
        vec![],
        vec![github_id],
    );
    let mut manifest = manifest_with_abilities_and_domains(
        vec![],
        vec![domain.id],
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
        });

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("test-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let domain_runner = runner.domain_expansion("github").await.unwrap();
    let mcp_names: Vec<&str> = domain_runner
        .instance()
        .prompt_context()
        .mcp_server_info
        .iter()
        .map(|entry| entry.0.as_str())
        .collect();
    assert!(mcp_names.contains(&"GitHub"));
}
