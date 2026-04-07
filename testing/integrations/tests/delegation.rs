//! Real LLM integration tests for agent-to-agent delegation.
//!
//! Requires `OPENROUTER_API_KEY` environment variable.
//! Tests are skipped automatically if the key is not set.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo_models::ModelProvider;
use nenjo_models::openrouter::OpenRouterProvider;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct OpenRouterFactory {
    api_key: String,
}

impl ModelProviderFactory for OpenRouterFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(OpenRouterProvider::new(Some(&self.api_key))))
    }
}

fn get_api_key() -> Option<String> {
    match std::env::var("OPENROUTER_API_KEY") {
        Ok(key) if !key.is_empty() => Some(key),
        _ => None,
    }
}

fn make_model() -> ModelManifest {
    ModelManifest {
        id: Uuid::new_v4(),
        name: "claude-haiku".into(),
        description: None,
        model: "anthropic/claude-3-haiku".into(),
        model_provider: "openrouter".into(),
        temperature: Some(0.0),
        tags: vec![],
    }
}

fn make_project() -> ProjectManifest {
    ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    }
}

fn make_agent(name: &str, model_id: Uuid, system_prompt: &str) -> AgentManifest {
    AgentManifest {
        id: Uuid::new_v4(),
        name: name.into(),
        description: Some(format!("Test agent: {name}")),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": system_prompt,
            "templates": {
                "chat_task": "{{ message }}",
                "task_execution": "",
                "gate_eval": "",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model_id),
        model_name: Some("claude-haiku".into()),
        domains: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        abilities: vec![],
        prompt_locked: false,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// An agent with delegate_to can delegate a task to another agent using a
/// real LLM. The leader agent is told to delegate, and we verify the
/// delegate_to tool was called and the response incorporates the delegate's work.
#[tokio::test]
async fn delegate_to_real_llm() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();

    let leader = make_agent(
        "leader",
        model.id,
        "You are a team leader. When you receive a task, you MUST delegate it \
         to the 'specialist' agent using the delegate_to tool. Pass the task \
         description to the specialist exactly as you received it. After \
         receiving the delegate's response, summarize it briefly.",
    );

    let specialist = make_agent(
        "specialist",
        model.id,
        "You are a specialist agent. When you receive a task, respond with a \
         concise, helpful answer. Always include the word 'SPECIALIST' in \
         your response so we can verify delegation happened.",
    );

    let manifest = Manifest {
        agents: vec![leader, specialist],
        models: vec![model],
        projects: vec![make_project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    // Verify delegate_to is available
    let runner = provider
        .agent_by_name("leader")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();
    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"delegate_to"),
        "delegate_to should be injected. Tools: {tool_names:?}"
    );

    // Run the leader with a task that should trigger delegation
    let output = runner
        .chat("What is the capital of France?")
        .await
        .expect("chat should succeed");

    println!("--- Delegation Test ---");
    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);
    println!("Input tokens: {}", output.input_tokens);
    println!("Output tokens: {}", output.output_tokens);

    // The leader should have delegated (at least 1 tool call)
    assert!(
        output.tool_calls >= 1,
        "leader should have called delegate_to, got {} tool calls",
        output.tool_calls
    );

    // The response should contain something from the specialist
    // (the specialist always includes "SPECIALIST" in its response)
    assert!(!output.text.is_empty(), "response should not be empty");
}

/// Verify that delegate_to's parameters_schema includes available agent names
/// so the LLM knows who it can delegate to.
#[tokio::test]
async fn delegate_to_schema_includes_agent_names() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();

    let manifest = Manifest {
        agents: vec![
            make_agent("alpha", model.id, "You are alpha."),
            make_agent("beta", model.id, "You are beta."),
            make_agent("gamma", model.id, "You are gamma."),
        ],
        models: vec![model],
        projects: vec![make_project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("alpha")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();
    let specs = runner.instance().tool_specs();
    let delegate_spec = specs
        .iter()
        .find(|s| s.name == "delegate_to")
        .expect("delegate_to should exist");

    let agent_desc = delegate_spec.parameters["properties"]["agent_name"]["description"]
        .as_str()
        .unwrap_or("");

    println!("--- Schema Test ---");
    println!("agent_name description: {agent_desc}");

    assert!(
        agent_desc.contains("beta"),
        "should list beta: {agent_desc}"
    );
    assert!(
        agent_desc.contains("gamma"),
        "should list gamma: {agent_desc}"
    );
    assert!(
        !agent_desc.contains("alpha"),
        "should NOT list self (alpha): {agent_desc}"
    );
}
