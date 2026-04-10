//! E2E tests for memory and resource tools with a real LLM.
//!
//! Requires `OPENROUTER_API_KEY` environment variable.
//! Tests are skipped automatically if the key is not set.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use nenjo::memory::MarkdownMemory;
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
        base_url: None,
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
                "chat_task": "{{ chat.message }}",
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

/// Agent stores a fact via memory_store, verify it lands in the correct
/// file on disk under the project-scoped namespace.
#[tokio::test]
async fn memory_store_writes_to_correct_scope() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let mem_dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(mem_dir.path(), ws_dir.path());

    let model = make_model();
    let project = ProjectManifest {
        id: Uuid::new_v4(),
        name: "webapp".into(),
        slug: "webapp".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    };
    let agent = make_agent(
        "coder",
        model.id,
        "You are a helpful assistant.\n\
         When the user tells you to remember something, use memory_store with scope 'project'.\n\
         Always respond concisely.",
    );

    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![project.clone()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("coder")
        .await
        .unwrap()
        .with_project_context(&project)
        .build()
        .await
        .unwrap();

    let output = runner
        .chat("Remember that we use Axum for HTTP. Category: architecture")
        .await
        .expect("chat should succeed");

    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);

    assert!(
        output.tool_calls >= 1,
        "agent should have called memory_store, got: {}",
        output.tool_calls
    );

    // Verify fact landed in the project-scoped dir
    let project_dir = mem_dir.path().join("agent_coder_project_webapp");
    assert!(
        project_dir.exists(),
        "project memory dir should exist at {:?}",
        project_dir
    );

    let files: Vec<_> = std::fs::read_dir(&project_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !files.is_empty(),
        "should have at least one category file in project dir"
    );
}

/// Agent saves a resource via resource_save, verify the file and manifest
/// land in the workspace dir.
#[tokio::test]
async fn resource_save_writes_to_workspace() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let mem_dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(mem_dir.path(), ws_dir.path());

    let model = make_model();
    let project = ProjectManifest {
        id: Uuid::new_v4(),
        name: "webapp".into(),
        slug: "webapp".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    };
    let agent = make_agent(
        "architect",
        model.id,
        "You are a helpful assistant.\n\
         When the user asks you to create a document, use resource_save with scope 'project'.\n\
         Always respond concisely.",
    );

    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![project.clone()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("architect")
        .await
        .unwrap()
        .with_project_context(&project)
        .build()
        .await
        .unwrap();

    let output = runner
        .chat("Create a resource called 'auth-design.md' with description 'Auth design doc' and content '# Auth Design\nUse OAuth2 with PKCE flow.'")
        .await
        .expect("chat should succeed");

    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);

    assert!(
        output.tool_calls >= 1,
        "agent should have called resource_save, got: {}",
        output.tool_calls
    );

    // Verify resource landed in workspace dir under project
    let resource_dir = ws_dir.path().join("webapp/resources");
    assert!(
        resource_dir.exists(),
        "resource dir should exist at {:?}",
        resource_dir
    );

    let manifest_path = resource_dir.join("manifest.json");
    assert!(
        manifest_path.exists(),
        "manifest.json should exist in resource dir"
    );

    // Resource should NOT be in memory dir
    assert!(
        !mem_dir.path().join("webapp").exists(),
        "resources should NOT be in memory dir"
    );
}
