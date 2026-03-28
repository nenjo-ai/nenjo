//! Tests for AgentBuilder and AgentRunner.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AgentManifest, ContextBlockManifest, Manifest, ModelManifest, ProjectManifest,
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
                "chat_task": "{{ message }}",
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

    let runner = provider.agent_by_name("test-coder").await.unwrap().build();
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

    let runner = provider.agent_by_name("test-coder").await.unwrap().build();

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
        .build();

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

    let runner = provider.agent_by_name("test-coder").await.unwrap().build();

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
        .with_memory_xml("<memory>test memory</memory>")
        .build();

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
