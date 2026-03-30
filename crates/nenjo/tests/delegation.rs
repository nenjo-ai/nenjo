//! Integration tests for DelegateToTool — agent-to-agent delegation.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

/// Mock LLM that returns a fixed response.
struct MockLlm {
    response: String,
}

impl MockLlm {
    fn new(text: &str) -> Self {
        Self {
            response: text.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockLlm {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some(self.response.clone()),
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
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

struct MockFactory {
    response: String,
}

impl MockFactory {
    fn new(text: &str) -> Self {
        Self {
            response: text.to_string(),
        }
    }
}

impl ModelProviderFactory for MockFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(MockLlm::new(&self.response)))
    }
}

/// Mock LLM that returns different responses on sequential calls.
#[allow(dead_code)]
struct SequentialMockLlm {
    responses: Vec<String>,
    index: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ModelProvider for SequentialMockLlm {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let i = self.index.fetch_add(1, Ordering::SeqCst);
        let text = self
            .responses
            .get(i % self.responses.len())
            .cloned()
            .unwrap_or_default();
        Ok(ChatResponse {
            text: Some(text),
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
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

#[allow(dead_code)]
struct SequentialMockFactory {
    responses: Vec<String>,
    index: Arc<AtomicUsize>,
}

impl SequentialMockFactory {
    #[allow(dead_code)]
    fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: responses.into_iter().map(|s| s.to_string()).collect(),
            index: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ModelProviderFactory for SequentialMockFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(SequentialMockLlm {
            responses: self.responses.clone(),
            index: self.index.clone(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn model(id: Uuid) -> ModelManifest {
    ModelManifest {
        id,
        name: "test-model".into(),
        description: None,
        model: "mock-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        tags: vec![],
    }
}

fn agent(id: Uuid, name: &str, model_id: Uuid) -> AgentManifest {
    AgentManifest {
        id,
        name: name.into(),
        description: Some(format!("{name} agent")),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": format!("You are the {name} agent."),
            "templates": {
                "task_execution": "Execute: {{ task.title }}",
                "chat_task": "{{ chat.message }}",
                "gate_eval": "",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model_id),
        model_name: Some("test-model".into()),
        skills: vec![],
        domains: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        abilities: vec![],
    }
}

fn project() -> ProjectManifest {
    ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// delegate_to is auto-injected when multiple agents exist.
#[tokio::test]
async fn delegate_to_injected_with_multiple_agents() {
    let model_id = Uuid::new_v4();
    let a1 = Uuid::new_v4();
    let a2 = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![
            agent(a1, "coder", model_id),
            agent(a2, "reviewer", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider.agent_by_name("coder").await.unwrap().build();
    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"delegate_to"), "tools: {names:?}");
}

/// delegate_to is NOT injected for a single agent.
#[tokio::test]
async fn delegate_to_not_injected_for_single_agent() {
    let model_id = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![agent(Uuid::new_v4(), "solo", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider.agent_by_name("solo").await.unwrap().build();
    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(!names.contains(&"delegate_to"), "tools: {names:?}");
}

/// delegate_to tool's schema lists available agent names.
#[tokio::test]
async fn delegate_to_schema_lists_agents() {
    let model_id = Uuid::new_v4();
    let a1 = Uuid::new_v4();
    let a2 = Uuid::new_v4();
    let a3 = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![
            agent(a1, "coder", model_id),
            agent(a2, "reviewer", model_id),
            agent(a3, "architect", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider.agent_by_name("coder").await.unwrap().build();

    let specs = runner.instance().tool_specs();
    let delegate_spec = specs.iter().find(|s| s.name == "delegate_to").unwrap();

    let desc = delegate_spec.parameters["properties"]["agent_name"]["description"]
        .as_str()
        .unwrap_or("");

    // Should list other agents but not the caller
    assert!(desc.contains("reviewer"), "should list reviewer: {desc}");
    assert!(desc.contains("architect"), "should list architect: {desc}");
    assert!(
        !desc.contains("coder"),
        "should NOT list self (coder): {desc}"
    );
}

/// Direct execution of delegate_to tool succeeds.
#[tokio::test]
async fn delegate_to_tool_executes() {
    let model_id = Uuid::new_v4();
    let coder_id = Uuid::new_v4();
    let reviewer_id = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![
            agent(coder_id, "coder", model_id),
            agent(reviewer_id, "reviewer", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("Review complete: all looks good."))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider.agent_by_name("coder").await.unwrap().build();

    // Find the delegate_to tool and call it directly
    let delegate_tool = runner
        .instance()
        .tools
        .iter()
        .find(|t| t.name() == "delegate_to")
        .expect("delegate_to should be present");

    let result = delegate_tool
        .execute(serde_json::json!({
            "agent_name": "reviewer",
            "task": "Review my code changes"
        }))
        .await
        .unwrap();

    assert!(result.success, "error: {:?}", result.error);
    assert_eq!(result.output, "Review complete: all looks good.");
}

/// delegate_to fails gracefully for unknown agent.
#[tokio::test]
async fn delegate_to_unknown_agent() {
    let model_id = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![
            agent(Uuid::new_v4(), "coder", model_id),
            agent(Uuid::new_v4(), "reviewer", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider.agent_by_name("coder").await.unwrap().build();

    let delegate_tool = runner
        .instance()
        .tools
        .iter()
        .find(|t| t.name() == "delegate_to")
        .unwrap();

    let result = delegate_tool
        .execute(serde_json::json!({
            "agent_name": "nonexistent",
            "task": "Do something"
        }))
        .await
        .unwrap();

    assert!(!result.success);
    assert!(
        result.error.as_deref().unwrap_or("").contains("not found"),
        "error: {:?}",
        result.error
    );
}

/// delegate_to fails with empty agent_name.
#[tokio::test]
async fn delegate_to_empty_agent_name() {
    let model_id = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![
            agent(Uuid::new_v4(), "a", model_id),
            agent(Uuid::new_v4(), "b", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider.agent_by_name("a").await.unwrap().build();

    let delegate_tool = runner
        .instance()
        .tools
        .iter()
        .find(|t| t.name() == "delegate_to")
        .unwrap();

    let result = delegate_tool
        .execute(serde_json::json!({ "agent_name": "", "task": "something" }))
        .await
        .unwrap();

    assert!(!result.success);
    assert!(result.error.is_some());
}

/// Delegation depth is limited by max_delegation_depth.
#[tokio::test]
async fn delegate_to_depth_limit() {
    let model_id = Uuid::new_v4();
    let a1 = Uuid::new_v4();
    let a2 = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![agent(a1, "alpha", model_id), agent(a2, "beta", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    // Set max_delegation_depth to 0 — should still inject the tool (depth check is at call time)
    // Actually, with depth 0 the tool won't be injected at all.
    let config = nenjo::AgentConfig {
        max_delegation_depth: 0,
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("ok"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent_by_name("alpha")
        .await
        .unwrap()
        .with_config(config)
        .build();

    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(
        !names.contains(&"delegate_to"),
        "delegate_to should NOT be injected with max_delegation_depth=0"
    );
}
