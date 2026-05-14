//! E2E tests for memory and artifact tools with a real LLM.
//!
//! Requires `OPENROUTER_API_KEY` environment variable.
//! Tests are skipped automatically if the key is not set.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, PromptTemplates,
};
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
        name: "openrouter-nemotron".into(),
        description: None,
        model: "nvidia/nemotron-3-super-120b-a12b:free".into(),
        model_provider: "openrouter".into(),
        temperature: Some(0.7),
        base_url: None,
    }
}

fn make_agent(name: &str, model_id: Uuid, system_prompt: &str) -> AgentManifest {
    AgentManifest {
        id: Uuid::new_v4(),
        name: name.into(),
        description: Some(format!("Test agent: {name}")),
        prompt_config: PromptConfig {
            system_prompt: system_prompt.into(),
            templates: PromptTemplates {
                chat_task: "{{ chat.message }}".into(),
                task_execution: String::new(),
                gate_eval: String::new(),
                cron_task: String::new(),
                ..Default::default()
            },
            ..Default::default()
        },
        color: None,
        model_id: Some(model_id),
        domain_ids: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        ability_ids: vec![],
        prompt_locked: false,
        heartbeat: None,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// Agent stores a fact via save_memory, verify it lands in the correct
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
        settings: serde_json::Value::Null,
    };
    let agent = make_agent(
        "coder",
        model.id,
        "You are a helpful assistant.\n\
         When the user tells you to remember something, use save_memory with scope 'project'.\n\
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
        "agent should have called save_memory, got: {}",
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

/// Agent saves an artifact via save_artifact, verify the file and manifest
/// land in the workspace dir.
#[tokio::test]
async fn save_artifact_writes_to_workspace() {
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
        settings: serde_json::Value::Null,
    };
    let agent = make_agent(
        "architect",
        model.id,
        "You are a helpful assistant.\n\
         When the user asks you to create a document, use save_artifact with scope 'project'.\n\
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
        .chat("Create an artifact called 'auth-design.md' with description 'Auth design doc' and content '# Auth Design\nUse OAuth2 with PKCE flow.'")
        .await
        .expect("chat should succeed");

    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);

    assert!(
        output.tool_calls >= 1,
        "agent should have called save_artifact, got: {}",
        output.tool_calls
    );

    // Verify artifact landed in workspace dir under project
    let resource_dir = ws_dir.path().join("webapp/artifacts");
    assert!(
        resource_dir.exists(),
        "artifact dir should exist at {:?}",
        resource_dir
    );

    let manifest_path = resource_dir.join("manifest.json");
    assert!(
        manifest_path.exists(),
        "manifest.json should exist in artifact dir"
    );

    // Artifact should NOT be in memory dir
    assert!(
        !mem_dir.path().join("webapp").exists(),
        "artifacts should NOT be in memory dir"
    );
}
