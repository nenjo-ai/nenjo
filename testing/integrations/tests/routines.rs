//! Simple routine integration test using a real LLM provider (OpenRouter).
//!
//! Requires `OPENROUTER_API_KEY` environment variable.
//! Tests are skipped automatically if the key is not set.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, PromptTemplates,
    RoutineManifest, RoutineMetadata, RoutineStepManifest, RoutineStepType, RoutineTrigger,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo::types::{Task, TaskType};
use nenjo_models::ModelProvider;
use nenjo_models::openrouter::OpenRouterProvider;

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
        base_url: None,
    }
}

fn make_project() -> ProjectManifest {
    ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        settings: serde_json::Value::Null,
    }
}

fn make_agent(model_id: Uuid) -> AgentManifest {
    AgentManifest {
        id: Uuid::new_v4(),
        name: "routine-agent".into(),
        description: Some("Executes routine steps".into()),
        prompt_config: PromptConfig {
            system_prompt:
                "You execute routine steps exactly. When given a task, complete it concisely, include the marker ROUTINE_OK in your final answer text, and then call pass_verdict exactly once as your final action with verdict 'pass' and brief reasoning."
                    .into(),
            templates: PromptTemplates {
                task_execution: "Task title: {{ task.title }}\nTask description: {{ task.description }}".into(),
                chat_task: "{{ chat.message }}".into(),
                gate_eval: String::new(),
                cron_task: String::new(),
                heartbeat_task: String::new(),
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

fn make_task(project_id: Uuid) -> TaskType {
    TaskType::Task(Task {
        task_id: Uuid::new_v4(),
        title: "Routine integration smoke test".into(),
        description: "Reply with a short sentence that includes the marker ROUTINE_OK.".into(),
        acceptance_criteria: None,
        tags: vec![],
        source: "integration-test".into(),
        project_id,
        status: String::new(),
        priority: String::new(),
        task_type: String::new(),
        slug: String::new(),
        complexity: String::new(),
        git: None,
    })
}

#[tokio::test]
async fn single_step_routine_with_real_llm() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();
    let project = make_project();
    let agent = make_agent(model.id);
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "smoke-routine".into(),
        description: Some("Single-step routine integration test".into()),
        trigger: RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "respond".into(),
            step_type: RoutineStepType::Agent,
            council_id: None,
            agent_id: Some(agent.id),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![project.clone()],
        routines: vec![routine],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let result = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run(make_task(project.id))
        .await
        .expect("routine should succeed");

    println!("--- Routine Integration Test ---");
    println!("Output: {}", result.output);
    println!("Passed: {}", result.passed);
    println!("Tool calls: {}", result.tool_calls);

    assert!(result.passed, "routine should pass");
    assert!(
        result.tool_calls >= 1,
        "routine should complete via pass_verdict tool call, got tool_calls={}",
        result.tool_calls
    );
}
