//! Tests for AgentBuilder and AgentRunner.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::PlatformToolResolver;
use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, DomainManifest, Manifest, ModelManifest,
    ProjectManifest,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider, ToolFactory};
use nenjo_models::traits::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use nenjo_tools::{Tool, ToolCategory, ToolResult};

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
        tags: vec![],
    };

    let agent = AgentManifest {
        id: Uuid::new_v4(),
        name: "test-coder".into(),
        description: Some("A test coding agent".into()),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": "You are a helpful coding assistant.",
            "developer_prompt": "Focus on writing clean, idiomatic Rust.",
            "templates": {
                "task_execution": "Execute the following task:\n\nTitle: {{ task.title }}\nDescription: {{ task.description }}",
                "chat_task": "{{ chat.message }}",
                "gate_eval": "Evaluate: {{ gate.criteria }}",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model.id),
        model_name: Some("test-model".into()),
        skills: vec![],
        domains: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        abilities: vec![],
    };

    Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![ProjectManifest {
            id: Uuid::new_v4(),
            name: "test-project".into(),
            slug: "test-project".into(),
            description: Some("A test project".into()),
            is_system: false,
            settings: serde_json::Value::Null,
        }],
        context_blocks: vec![ContextBlockManifest {
            id: Uuid::new_v4(),
            name: "available_agents".into(),
            path: "nenjo".into(),
            display_name: None,
            description: None,
            template: "<available_agents>\n{{items}}\n</available_agents>".into(),
            is_system: true,
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

    let task = nenjo::types::TaskType::Chat {
        user_message: "Hello!".into(),
        history: vec![],
        project_id: Uuid::nil(),
    };

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
// Mock PlatformToolResolver for scope tests
// ---------------------------------------------------------------------------

/// A fake platform tool that carries its name. Does nothing on execute.
struct FakePlatformTool {
    tool_name: String,
}

#[async_trait::async_trait]
impl Tool for FakePlatformTool {
    fn name(&self) -> &str {
        &self.tool_name
    }
    fn description(&self) -> &str {
        "fake platform tool"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: String::new(),
            error: None,
        })
    }
}

/// Tool factory that uses MockPlatformResolver to create platform tools for the agent.
struct MockPlatformToolFactory;

#[async_trait::async_trait]
impl ToolFactory for MockPlatformToolFactory {
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        let resolver = MockPlatformResolver;
        resolver.resolve_tools(&agent.platform_scopes).await
    }
}

/// Mock resolver that returns platform tools based on scope matching.
struct MockPlatformResolver;

#[async_trait::async_trait]
impl nenjo::PlatformToolResolver for MockPlatformResolver {
    async fn resolve_tools(&self, platform_scopes: &[String]) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        for scope in platform_scopes {
            let tool_name = match scope.as_str() {
                "tasks:read" => "app.nenjo.platform/tasks_list",
                "tasks:write" => "app.nenjo.platform/tasks_create",
                "agents:read" => "app.nenjo.platform/agents_list",
                "agents:write" => "app.nenjo.platform/agents_create",
                "projects:read" => "app.nenjo.platform/projects_list",
                _ => continue,
            };
            tools.push(Arc::new(FakePlatformTool {
                tool_name: tool_name.to_string(),
            }));
        }
        tools
    }
}

// ---------------------------------------------------------------------------
// Helpers for ability/domain tests
// ---------------------------------------------------------------------------

fn ability_manifest(name: &str, scopes: Vec<&str>) -> AbilityManifest {
    AbilityManifest {
        id: Uuid::new_v4(),
        name: name.into(),
        display_name: None,
        description: Some(format!("{name} ability")),
        activation_condition: format!("when {name} is needed"),
        prompt: format!("You are the {name} ability."),
        platform_scopes: scopes.into_iter().map(String::from).collect(),
        skill_ids: vec![],
        mcp_server_ids: vec![],
        tool_filter: serde_json::json!({}),
    }
}

fn domain_manifest_with_config(
    name: &str,
    abilities: Vec<&str>,
    scopes: Vec<&str>,
) -> DomainManifest {
    DomainManifest {
        id: Uuid::new_v4(),
        name: name.into(),
        display_name: name.into(),
        description: Some(format!("{name} domain")),
        command: name.into(),
        manifest: serde_json::json!({
            "tools": {
                "additional_scopes": scopes,
                "activate_abilities": abilities,
            },
        }),
        category: None,
        tags: vec![],
        is_system: false,
        source_domain_id: None,
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
        tags: vec![],
    };

    let agent = AgentManifest {
        id: Uuid::new_v4(),
        name: "test-agent".into(),
        description: Some("Test agent".into()),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": "You are a test agent.",
            "developer_prompt": "Be helpful.",
            "templates": {
                "task_execution": "",
                "chat_task": "{{ chat.message }}",
                "gate_eval": "",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model.id),
        model_name: Some("test-model".into()),
        skills: vec![],
        domains: agent_domains,
        platform_scopes: agent_scopes.into_iter().map(String::from).collect(),
        mcp_server_ids: vec![],
        abilities: agent_abilities,
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
            is_system: false,
            settings: serde_json::Value::Null,
        }],
        ..Default::default()
    }
}

// ===========================================================================
// Ability scope tests
// ===========================================================================

#[tokio::test]
async fn ability_agent_has_use_ability_and_platform_tools() {
    let ability = ability_manifest("writer", vec!["agents:write"]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.id],
        vec![],
        vec!["tasks:read"],
        vec![ability],
        vec![],
    );

    let resolver: Arc<dyn nenjo::PlatformToolResolver> = Arc::new(MockPlatformResolver);

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(MockPlatformToolFactory)
        .with_platform_resolver(resolver)
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
        tool_names.contains(&"app.nenjo.platform/tasks_list"),
        "base agent should have tasks_list, got: {tool_names:?}"
    );
    assert!(
        tool_names.contains(&"use_ability"),
        "base agent should have use_ability, got: {tool_names:?}"
    );
}

#[tokio::test]
async fn agent_without_abilities_has_no_use_ability() {
    let manifest =
        manifest_with_abilities_and_domains(vec![], vec![], vec!["tasks:read"], vec![], vec![]);

    let resolver: Arc<dyn nenjo::PlatformToolResolver> = Arc::new(MockPlatformResolver);

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(MockPlatformToolFactory)
        .with_platform_resolver(resolver)
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
        !tool_names.contains(&"use_ability"),
        "agent without abilities should not have use_ability, got: {tool_names:?}"
    );
    assert!(tool_names.contains(&"app.nenjo.platform/tasks_list"));
}

// ===========================================================================
// Domain expansion tests
// ===========================================================================

#[tokio::test]
async fn domain_expansion_adds_scopes_and_tools() {
    let domain = domain_manifest_with_config("creator", vec![], vec!["agents:write"]);
    let manifest = manifest_with_abilities_and_domains(
        vec![],
        vec![domain.id],
        vec!["tasks:read"],
        vec![],
        vec![domain],
    );

    let resolver: Arc<dyn nenjo::PlatformToolResolver> = Arc::new(MockPlatformResolver);

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(MockPlatformToolFactory)
        .with_platform_resolver(resolver)
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

    // Before: only tasks_list
    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(tool_names.contains(&"app.nenjo.platform/tasks_list"));
    assert!(!tool_names.contains(&"app.nenjo.platform/agents_create"));

    // After: also agents_create
    let domain_runner = runner.domain_expansion("creator").await.unwrap();
    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"app.nenjo.platform/tasks_list"),
        "domain should preserve parent tools, got: {tool_names:?}"
    );
    assert!(
        tool_names.contains(&"app.nenjo.platform/agents_create"),
        "domain should add scope-resolved tools, got: {tool_names:?}"
    );
    assert!(
        domain_runner
            .instance()
            .prompt_context
            .platform_scopes
            .contains(&"agents:write".to_string()),
        "domain should merge scopes into prompt_context"
    );
}

#[tokio::test]
async fn domain_expansion_activates_abilities_and_injects_use_ability() {
    let ability = ability_manifest("code-review", vec!["tasks:write"]);
    let domain = domain_manifest_with_config("reviewer", vec!["code-review"], vec![]);
    let manifest = manifest_with_abilities_and_domains(
        vec![], // agent has no abilities
        vec![domain.id],
        vec!["tasks:read"],
        vec![ability],
        vec![domain],
    );

    let resolver: Arc<dyn nenjo::PlatformToolResolver> = Arc::new(MockPlatformResolver);

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(MockPlatformToolFactory)
        .with_platform_resolver(resolver)
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

    // Before: no abilities, no use_ability
    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(!tool_names.contains(&"use_ability"));
    assert!(
        runner
            .instance()
            .prompt_context
            .available_abilities
            .is_empty()
    );

    // After: ability activated, use_ability injected
    let domain_runner = runner.domain_expansion("reviewer").await.unwrap();
    let specs = domain_runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"use_ability"),
        "domain should inject use_ability, got: {tool_names:?}"
    );

    let ability_names: Vec<&str> = domain_runner
        .instance()
        .prompt_context
        .available_abilities
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert!(
        ability_names.contains(&"code-review"),
        "domain should activate ability, got: {ability_names:?}"
    );
}

#[tokio::test]
async fn domain_expansion_with_scopes_and_abilities() {
    let ability = ability_manifest("deployer", vec!["projects:read"]);
    let domain = domain_manifest_with_config("ops", vec!["deployer"], vec!["agents:write"]);
    let manifest = manifest_with_abilities_and_domains(
        vec![],
        vec![domain.id],
        vec!["tasks:read"],
        vec![ability],
        vec![domain],
    );

    let resolver: Arc<dyn nenjo::PlatformToolResolver> = Arc::new(MockPlatformResolver);

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(MockPlatformToolFactory)
        .with_platform_resolver(resolver)
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

    assert!(tool_names.contains(&"app.nenjo.platform/tasks_list"));
    assert!(tool_names.contains(&"app.nenjo.platform/agents_create"));
    assert!(tool_names.contains(&"use_ability"));

    let ability_names: Vec<&str> = domain_runner
        .instance()
        .prompt_context
        .available_abilities
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert!(ability_names.contains(&"deployer"));
}

#[tokio::test]
async fn domain_expansion_does_not_duplicate_existing_abilities() {
    let ability = ability_manifest("writer", vec!["agents:write"]);
    let domain = domain_manifest_with_config("creator", vec!["writer"], vec![]);
    let manifest = manifest_with_abilities_and_domains(
        vec![ability.id], // agent already has this ability
        vec![domain.id],
        vec!["tasks:read"],
        vec![ability],
        vec![domain],
    );

    let resolver: Arc<dyn nenjo::PlatformToolResolver> = Arc::new(MockPlatformResolver);

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory::new("ok"))
        .with_tool_factory(MockPlatformToolFactory)
        .with_platform_resolver(resolver)
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
        .prompt_context
        .available_abilities
        .iter()
        .filter(|a| a.name == "writer")
        .count();
    assert_eq!(ability_count, 1, "should not duplicate existing abilities");
}
