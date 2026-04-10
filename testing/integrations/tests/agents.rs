//! Integration tests using a real LLM provider (OpenRouter).
//!
//! Requires `OPENROUTER_API_KEY` environment variable.
//! Tests are skipped automatically if the key is not set.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AbilityManifest, AgentManifest, DomainManifest, Manifest, ModelManifest, ProjectManifest,
};
use nenjo::memory::{MarkdownMemory, MemoryScope};
use nenjo::provider::{ModelProviderFactory, Provider, ToolFactory};
use nenjo_models::ModelProvider;
use nenjo_models::openrouter::OpenRouterProvider;
use nenjo_tools::{Tool, ToolCategory, ToolResult};

// ---------------------------------------------------------------------------
// Shared mocks / helpers
// ---------------------------------------------------------------------------

struct OpenRouterFactory {
    api_key: String,
}

impl ModelProviderFactory for OpenRouterFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(OpenRouterProvider::new(Some(&self.api_key))))
    }
}

struct GetWeatherTool;

#[async_trait::async_trait]
impl Tool for GetWeatherTool {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn description(&self) -> &str {
        "Get the current weather for a given city. Returns temperature and conditions."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "The city name, e.g. 'San Francisco'"
                }
            },
            "required": ["city"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(ToolResult {
            success: true,
            output: format!("{city}: 72°F, sunny with light clouds"),
            error: None,
        })
    }
}

struct WeatherToolFactory;

#[async_trait::async_trait]
impl ToolFactory for WeatherToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(GetWeatherTool)]
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
        base_url: None,
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

#[tokio::test]
async fn tool_call_round_trip() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();
    let agent = make_agent(
        "weather-agent",
        model.id,
        "You are a helpful weather assistant. When asked about the weather, \
         use the get_weather tool to look it up. After getting the result, \
         summarize it for the user.",
    );

    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![make_project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(WeatherToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("weather-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "get_weather");

    let output = runner
        .chat("What's the weather like in San Francisco?")
        .await
        .expect("chat should succeed");

    println!("--- Tool Call Test ---");
    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);

    assert!(!output.text.is_empty());
    assert!(
        output.tool_calls >= 1,
        "should have called get_weather, got: {}",
        output.tool_calls
    );
    assert!(
        output.text.contains("72")
            || output.text.contains("sunny")
            || output.text.contains("San Francisco"),
        "response should reference weather data, got: {}",
        output.text
    );
}

#[tokio::test]
async fn memory_store_recall_with_real_llm() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path(), ws_dir.path());

    let model = make_model();
    let project = make_project();
    let agent = make_agent(
        "memory-agent",
        model.id,
        "You are a helpful assistant with persistent memory.\n\
         When the user tells you something to remember, use memory_store.\n\
         When asked what you know, use memory_recall first.\n\
         Always respond concisely.",
    );

    let _agent_id = agent.id;

    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![project],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory {
            api_key: api_key.clone(),
        })
        .with_tool_factory(nenjo::provider::NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    // Verify memory tools are present
    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"memory_store"),
        "should have memory_store, got: {tool_names:?}"
    );
    assert!(
        tool_names.contains(&"memory_recall"),
        "should have memory_recall, got: {tool_names:?}"
    );

    // Ask the agent to store something
    let output = runner
        .chat("Remember that my favorite programming language is Rust.")
        .await
        .expect("store chat should succeed");

    println!("--- Memory Store ---");
    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);

    assert!(
        output.tool_calls >= 1,
        "agent should have called memory_store, got: {}",
        output.tool_calls
    );

    // Verify the fact was actually persisted to the markdown backend
    let scope = MemoryScope::new("memory-agent", Some("test-project"));
    let backend = MarkdownMemory::new(dir.path(), ws_dir.path());

    use nenjo::memory::Memory;
    let cats = backend.list_categories(&scope.project).await.unwrap();

    println!("--- Persisted Categories ---");
    for cat in &cats {
        println!("  {}: {} facts", cat.category, cat.facts.len());
        for fact in &cat.facts {
            println!("    - {}", fact.text);
        }
    }

    assert!(
        !cats.is_empty(),
        "memory_store should have persisted facts to disk"
    );
    let has_rust = cats
        .iter()
        .flat_map(|c| &c.facts)
        .any(|f| f.text.to_lowercase().contains("rust"));
    assert!(has_rust, "stored fact should mention Rust");

    // Now ask the agent to forget it
    let output = runner
        .chat("Forget what you stored about my favorite programming language.")
        .await
        .expect("forget chat should succeed");

    println!("--- Memory Forget ---");
    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);

    assert!(
        output.tool_calls >= 1,
        "agent should have called memory_forget, got: {}",
        output.tool_calls
    );

    // Verify the fact was deleted from disk
    let cats_after = backend.list_categories(&scope.project).await.unwrap();
    let has_rust_after = cats_after
        .iter()
        .flat_map(|c| &c.facts)
        .any(|f| f.text.to_lowercase().contains("rust"));

    println!("--- Categories After Forget ---");
    for cat in &cats_after {
        println!("  {}: {} facts", cat.category, cat.facts.len());
    }

    assert!(
        !has_rust_after,
        "memory_forget should have deleted the Rust fact"
    );
}

#[tokio::test]
async fn use_ability_with_real_llm() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();
    let code_review_ability_id = Uuid::new_v4();

    // Agent with an ability assigned
    let mut agent = make_agent(
        "developer",
        model.id,
        "You are a senior software developer. \
         When asked to review code, use the code_review ability.",
    );
    agent.abilities = vec![code_review_ability_id];

    // The ability: code review with specific instructions
    let ability = AbilityManifest {
        id: code_review_ability_id,
        name: "code_review".into(),
        path: String::new(),
        display_name: Some("Code Review".into()),
        description: Some("Reviews code for bugs, style issues, and improvements".into()),
        activation_condition: "When the user asks for a code review".into(),
        prompt: "You are performing a code review. Analyze the code for:\n\
                 1. Bugs and logic errors\n\
                 2. Style and naming issues\n\
                 3. Potential improvements\n\
                 Respond with a concise review in bullet points."
            .into(),
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        tool_filter: serde_json::json!({}),
        is_system: false,
    };

    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![make_project()],
        abilities: vec![ability],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(nenjo::provider::NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("developer")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    // Verify per-ability tool is present
    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        tool_names.contains(&"ability/code_review"),
        "should have ability/code_review tool, got: {tool_names:?}"
    );

    // Ask the agent to review some code — it should activate the code_review ability
    let output = runner
        .chat("Please review this code:\n\n```rust\nfn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n```")
        .await
        .expect("chat should succeed");

    println!("--- Use Ability Test ---");
    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);

    // The agent should have called the ability tool and returned a review
    assert!(!output.text.is_empty(), "should have a response");
    assert!(
        output.tool_calls >= 1,
        "agent should have called ability tool, got: {}",
        output.tool_calls
    );
    // The review should mention the bug (subtraction instead of addition)
    let text_lower = output.text.to_lowercase();
    assert!(
        text_lower.contains("bug")
            || text_lower.contains("subtract")
            || text_lower.contains("minus")
            || text_lower.contains("a - b")
            || text_lower.contains("error"),
        "review should catch the bug (a - b instead of a + b), got: {}",
        output.text
    );
}

#[tokio::test]
async fn domain_expansion_with_real_llm() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();
    let prd_domain_id = Uuid::new_v4();

    // Agent with a domain assigned
    let mut agent = make_agent(
        "product-manager",
        model.id,
        "You are a product manager. You can enter domain modes for specialized work.",
    );
    agent.domains = vec![prd_domain_id];

    // The PRD domain with specific prompt overlay and guidelines
    let domain = DomainManifest {
        id: prd_domain_id,
        name: "prd".into(),
        path: String::new(),
        display_name: "PRD Writer".into(),
        description: Some("Write product requirements documents".into()),
        command: "/prd".into(),
        manifest: serde_json::json!({
            "schema_version": 1,
            "domain_type": "document",
            "prompt": {
                "system_addon": "You are now in PRD writing mode. Structure your response as a PRD with sections: Problem Statement, Goals, Non-Goals, User Stories, and Success Metrics. Be concise — one sentence per bullet point.",
                "guidelines": [
                    "Always include measurable success metrics",
                    "Separate goals from non-goals explicitly"
                ]
            },
            "tools": {},
            "session": {
                "max_turns": 20,
                "exit_commands": ["/exit", "/done"]
            }
        }),
        category: Some("documents".into()),
        tags: vec!["prd".into()],
        is_system: false,
        source_domain_id: None,
    };

    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![make_project()],
        domains: vec![domain],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(nenjo::provider::NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("product-manager")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    // Activate the PRD domain
    let prd_runner = runner
        .domain_expansion("prd")
        .await
        .expect("should activate prd domain");

    // Verify the domain is active on the instance
    let active = &prd_runner.instance().prompt_context.active_domain;
    assert!(active.is_some(), "should have active domain");
    assert_eq!(active.as_ref().unwrap().domain_name, "prd");

    // Ask the agent to write a PRD — the domain's prompt overlay should
    // guide it to produce structured output
    let output = prd_runner
        .chat("Write a PRD for a user authentication system with SSO support")
        .await
        .expect("chat should succeed");

    println!("--- Domain Expansion Test ---");
    println!("Response: {}", output.text);
    println!("Input tokens: {}", output.input_tokens);
    println!("Output tokens: {}", output.output_tokens);

    assert!(!output.text.is_empty(), "should have a response");

    // The domain's system_addon instructs the agent to structure as a PRD
    let text_lower = output.text.to_lowercase();
    assert!(
        text_lower.contains("problem")
            || text_lower.contains("goal")
            || text_lower.contains("user stor")
            || text_lower.contains("metric")
            || text_lower.contains("requirement"),
        "response should be structured as a PRD with sections, got: {}",
        output.text
    );
}

#[tokio::test]
async fn domain_expansion_unknown_domain_fails() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();
    let manifest = Manifest {
        agents: vec![make_agent("agent", model.id, "You are a test agent.")],
        models: vec![model],
        projects: vec![make_project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(nenjo::provider::NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let result = runner.domain_expansion("nonexistent").await;
    assert!(result.is_err());
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("not found"),
        "should report not found, got: {msg}"
    );
}
