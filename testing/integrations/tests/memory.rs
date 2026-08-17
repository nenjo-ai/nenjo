//! E2E tests for persistent memory tools with a real LLM.
//!
//! Requires `OPENROUTER_API_KEY` environment variable.
//! Tests are skipped automatically if the key is not set.

use std::sync::Arc;

use anyhow::Result;
use nenjo::Slug;
use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, PromptTemplates,
    model_manifest_slug,
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
        slug: model_manifest_slug("openrouter", "nvidia/nemotron-3-super-120b-a12b:free"),
        name: "openrouter-nemotron".into(),
        description: None,
        model: "nvidia/nemotron-3-super-120b-a12b:free".into(),
        model_provider: "openrouter".into(),
        temperature: Some(0.7),
        context_window: None,
        base_url: None,
        native_tools: vec![],
        capabilities: Vec::new(),
        input_modalities: Vec::new(),
        output_modalities: Vec::new(),
        execution_modes: Vec::new(),
    }
}

fn make_agent(name: &str, model: &ModelManifest, system_prompt: &str) -> AgentManifest {
    AgentManifest {
        name: name.into(),
        slug: Slug::derive(name),
        description: Some(format!("Test agent: {name}")),
        prompt_config: PromptConfig {
            system_prompt: system_prompt.into(),
            templates: PromptTemplates {
                chat_task: "{{ chat.message }}".into(),
                task_execution: String::new(),
                gate_eval: String::new(),
            },
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug(&model.model_provider, &model.model)),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        abilities: vec![],
        script_tools: vec![],
        media: vec![],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
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
    let memory = MarkdownMemory::new(mem_dir.path());

    let model = make_model();
    let project = ProjectManifest {
        name: "webapp".into(),
        slug: Slug::derive("webapp"),
        description: None,
        settings: serde_json::Value::Null,
    };
    let agent = make_agent(
        "coder",
        &model,
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
        .agent("coder")
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
