//! Tests for memory integration with Provider and AgentRunner.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use nenjo::memory::{MarkdownMemory, Memory, MemoryScope};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use nenjo_tools::Tool; // needed to call .execute() on tool structs

// ---------------------------------------------------------------------------
// Mock Provider
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
}

struct MockModelProviderFactory {
    response_text: String,
}

impl ModelProviderFactory for MockModelProviderFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(MockProvider::new(&self.response_text)))
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
        name: "memory-agent".into(),
        description: Some("An agent with memory".into()),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": "You are a helpful assistant with persistent memory.",
            "templates": {
                "chat_task": "{{ message }}",
                "task_execution": "",
                "gate_eval": "",
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

    let project = ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    };

    Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![project],
        ..Default::default()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[tokio::test]
async fn provider_with_memory_adds_tools() {
    let dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path());

    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory {
            response_text: "Got it!".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .build();

    // Memory tools should be auto-added
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
    assert!(
        tool_names.contains(&"memory_forget"),
        "should have memory_forget, got: {tool_names:?}"
    );
}

#[tokio::test]
async fn provider_without_memory_has_no_memory_tools() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory {
            response_text: "No memory.".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .build();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(
        !tool_names.contains(&"memory_store"),
        "should NOT have memory tools without .with_memory()"
    );
}

#[tokio::test]
async fn builder_with_memory_adds_tools() {
    let dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path());
    let manifest = test_manifest();
    let agent_id = manifest.agents[0].id;
    let project_id = manifest.projects[0].id;

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory {
            response_text: "Builder memory.".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let scope = MemoryScope::new(&project_id.to_string(), &agent_id.to_string());

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .with_memory(Arc::new(memory), scope)
        .build();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(tool_names.contains(&"memory_store"));
    assert!(tool_names.contains(&"memory_recall"));
    assert!(tool_names.contains(&"memory_forget"));
}

#[tokio::test]
async fn memory_store_recall_and_forget() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
    let scope = MemoryScope::new("test-project", "test-agent");

    // Store facts
    let rust_id = memory
        .store(&scope.project, "User prefers Rust", "preferences", 0.9)
        .await
        .unwrap();

    memory
        .store(&scope.project, "Always write tests", "practices", 0.95)
        .await
        .unwrap();

    memory
        .store(
            &scope.core,
            "Expert in distributed systems",
            "expertise",
            0.85,
        )
        .await
        .unwrap();

    // Search project scope
    let results = memory.search(&scope.project, "Rust", 5).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact, "User prefers Rust");

    // Search core scope
    let results = memory.search(&scope.core, "distributed", 5).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].fact.contains("distributed"));

    // Delete by ID
    assert!(memory.delete(&rust_id).await.unwrap());
    let results = memory.search(&scope.project, "Rust", 5).await.unwrap();
    assert!(
        results.is_empty(),
        "deleted item should not appear in search"
    );

    // Other facts remain
    let results = memory.search(&scope.project, "tests", 5).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact, "Always write tests");

    // Delete non-existent returns false
    assert!(!memory.delete("no-such-id").await.unwrap());
}

#[tokio::test]
async fn memory_forget_tool_by_query() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
    let scope = MemoryScope::new("test-project", "test-agent");

    memory
        .store(&scope.project, "User likes Python", "preferences", 0.8)
        .await
        .unwrap();
    memory
        .store(&scope.project, "User likes Rust", "preferences", 0.9)
        .await
        .unwrap();
    memory
        .store(&scope.project, "Deploy on Fridays", "practices", 0.7)
        .await
        .unwrap();

    // Use the forget tool to delete by query
    let tool = nenjo::memory::tools::MemoryForgetTool::new(memory.clone(), scope.clone());
    let result = tool
        .execute(serde_json::json!({
            "query": "Python",
            "scope": "project"
        }))
        .await
        .unwrap();

    assert!(result.success);
    assert!(
        result.output.contains("1"),
        "should have deleted 1 item: {}",
        result.output
    );

    // Python gone, Rust and Fridays remain
    let all = memory.search(&scope.project, "", 10).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(!all.iter().any(|i| i.fact.contains("Python")));
}

#[tokio::test]
async fn memory_forget_tool_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
    let scope = MemoryScope::new("test-project", "test-agent");

    let id = memory
        .store(&scope.project, "Temporary fact", "temp", 0.5)
        .await
        .unwrap();

    let tool = nenjo::memory::tools::MemoryForgetTool::new(memory.clone(), scope.clone());
    let result = tool.execute(serde_json::json!({ "id": id })).await.unwrap();

    assert!(result.success);
    assert!(
        result.output.contains("Deleted"),
        "should confirm deletion: {}",
        result.output
    );

    // Verify it's gone
    let results = memory
        .search(&scope.project, "Temporary", 10)
        .await
        .unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn memory_store_and_recall_tools_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
    let scope = MemoryScope::new("test-project", "test-agent");

    // Store via tool
    let store_tool = nenjo::memory::tools::MemoryStoreTool::new(memory.clone(), scope.clone());
    let result = store_tool
        .execute(serde_json::json!({
            "fact": "The API uses REST with JSON",
            "category": "architecture",
            "confidence": 0.95,
            "scope": "project"
        }))
        .await
        .unwrap();
    assert!(result.success);

    // Recall via tool
    let recall_tool = nenjo::memory::tools::MemoryRecallTool::new(memory.clone(), scope.clone());
    let result = recall_tool
        .execute(serde_json::json!({
            "query": "API REST",
            "scope": "project"
        }))
        .await
        .unwrap();
    assert!(result.success);
    assert!(
        result.output.contains("REST"),
        "recall should find the fact: {}",
        result.output
    );
    assert!(
        result.output.contains("architecture"),
        "should show category: {}",
        result.output
    );

    // Forget via tool
    let forget_tool = nenjo::memory::tools::MemoryForgetTool::new(memory.clone(), scope.clone());
    let result = forget_tool
        .execute(serde_json::json!({
            "query": "REST",
            "scope": "project"
        }))
        .await
        .unwrap();
    assert!(result.success);
    assert!(
        result.output.contains("1"),
        "should delete 1: {}",
        result.output
    );

    // Recall again — should be empty
    let result = recall_tool
        .execute(serde_json::json!({
            "query": "API REST",
            "scope": "project"
        }))
        .await
        .unwrap();
    assert!(result.success);
    assert!(
        result.output.contains("No memories"),
        "should find nothing after forget: {}",
        result.output
    );
}

#[tokio::test]
async fn memory_summaries_injected_into_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
    let manifest = test_manifest();
    let agent_id = manifest.agents[0].id;
    let project_id = manifest.projects[0].id;
    let scope = MemoryScope::new(&project_id.to_string(), &agent_id.to_string());

    // Store summaries in each tier
    memory
        .upsert_summary(
            &scope.project,
            "preferences",
            "User prefers Rust and snake_case",
            3,
        )
        .await
        .unwrap();

    memory
        .upsert_summary(
            &scope.core,
            "expertise",
            "Expert in distributed systems and Rust",
            5,
        )
        .await
        .unwrap();

    memory
        .upsert_summary(
            &scope.shared,
            "decisions",
            "Using PostgreSQL for the database",
            2,
        )
        .await
        .unwrap();

    // Build memory XML
    let xml = nenjo::memory::build_memory_xml(memory.as_ref(), &scope)
        .await
        .unwrap();

    assert!(xml.contains("<memory>"), "should have memory root tag");
    assert!(xml.contains("<memory-core>"), "should have core tier");
    assert!(
        xml.contains("<memory-summaries>"),
        "should have project tier"
    );
    assert!(xml.contains("<memory-shared>"), "should have shared tier");
    assert!(
        xml.contains("User prefers Rust"),
        "should contain project summary"
    );
    assert!(
        xml.contains("distributed systems"),
        "should contain core summary"
    );
    assert!(xml.contains("PostgreSQL"), "should contain shared summary");
}

#[tokio::test]
async fn memory_xml_empty_when_no_summaries() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
    let scope = MemoryScope::new("empty-project", "empty-agent");

    let xml = nenjo::memory::build_memory_xml(memory.as_ref(), &scope)
        .await
        .unwrap();

    assert!(xml.is_empty(), "should be empty when no summaries exist");
}

#[tokio::test]
async fn runner_with_memory_injects_xml() {
    let dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path());
    let manifest = test_manifest();
    let agent_id = manifest.agents[0].id;
    let project_id = manifest.projects[0].id;

    // Pre-populate a summary
    let scope = MemoryScope::new(&project_id.to_string(), &agent_id.to_string());
    memory
        .upsert_summary(
            &scope.project,
            "context",
            "This is a Rust project using Axum",
            1,
        )
        .await
        .unwrap();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory {
            response_text: "I see from memory this is a Rust/Axum project.".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .build();

    // The runner should inject memory XML and run successfully
    let output = runner
        .chat("What do you know about this project?")
        .await
        .unwrap();
    assert_eq!(
        output.text,
        "I see from memory this is a Rust/Axum project."
    );
}
