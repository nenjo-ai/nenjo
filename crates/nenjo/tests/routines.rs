//! Integration tests for routine execution via Provider.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AgentManifest, CouncilManifest, CouncilMemberManifest, LambdaManifest, Manifest, ModelManifest,
    ProjectManifest, RoutineEdgeManifest, RoutineManifest, RoutineStepManifest,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo::routines::{LambdaOutput, LambdaRunner, RoutineEvent};
use nenjo::types::{Task, TaskType};
use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall};

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

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

/// Mock that returns full ChatResponse objects in sequence, allowing tool call
/// responses. Shared call index so all instances advance the same counter.
struct SequentialResponseMockLlm {
    responses: Arc<Vec<ChatResponse>>,
    call_index: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ModelProvider for SequentialResponseMockLlm {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let resp = self
            .responses
            .get(idx)
            .unwrap_or(self.responses.last().unwrap())
            .clone();
        Ok(resp)
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

struct SequentialResponseMockFactory {
    responses: Arc<Vec<ChatResponse>>,
    call_index: Arc<AtomicUsize>,
}

impl SequentialResponseMockFactory {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Arc::new(responses),
            call_index: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ModelProviderFactory for SequentialResponseMockFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(SequentialResponseMockLlm {
            responses: self.responses.clone(),
            call_index: self.call_index.clone(),
        }))
    }
}

struct MockLambdaRunner;

#[async_trait::async_trait]
impl LambdaRunner for MockLambdaRunner {
    async fn run_script(
        &self,
        _script_path: &std::path::Path,
        _interpreter: &str,
        _env: std::collections::HashMap<String, String>,
        _timeout: std::time::Duration,
    ) -> Result<LambdaOutput> {
        Ok(LambdaOutput {
            stdout: "lambda executed successfully".to_string(),
            stderr: String::new(),
            exit_code: 0,
        })
    }
}

struct FailingLambdaRunner;

#[async_trait::async_trait]
impl LambdaRunner for FailingLambdaRunner {
    async fn run_script(
        &self,
        _script_path: &std::path::Path,
        _interpreter: &str,
        _env: std::collections::HashMap<String, String>,
        _timeout: std::time::Duration,
    ) -> Result<LambdaOutput> {
        Ok(LambdaOutput {
            stdout: String::new(),
            stderr: "script failed".to_string(),
            exit_code: 1,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_task(project_id: Uuid, title: &str, desc: &str) -> TaskType {
    TaskType::Task(Task {
        task_id: Uuid::nil(),
        title: title.into(),
        description: desc.into(),
        acceptance_criteria: None,
        tags: vec![],
        source: "test".into(),
        project_id,
        status: String::new(),
        priority: String::new(),
        task_type: String::new(),
        slug: String::new(),
        complexity: String::new(),
        git: None,
    })
}

fn model(id: Uuid) -> ModelManifest {
    ModelManifest {
        id,
        name: "test-model".into(),
        description: None,
        model: "mock-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        tags: vec![],
        base_url: None,
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
                "task_execution": "Execute: {{ task.title }}\n{{ task.description }}",
                "chat_task": "{{ chat.message }}",
                "gate_eval": "Evaluate: {{ gate.criteria }}\n\nPrevious output:\n{{ gate.previous_output }}",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model_id),
        model_name: Some("test-model".into()),
        domains: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        abilities: vec![],
        prompt_locked: false,
    }
}

fn project() -> ProjectManifest {
    ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: Some("A test project".into()),
        is_system: false,
        settings: serde_json::Value::Null,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// Single agent step → terminal. The simplest possible routine.
#[tokio::test]
async fn single_agent_step() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "simple-routine".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "implement".into(),
            step_type: "agent".into(),
            model_id: Some(model_id),
            council_id: None,
            agent_id: Some(agent_id),
            lambda_id: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("Implementation complete."))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Add auth", "Implement JWT authentication");
    let result = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "Implementation complete.");
    assert_eq!(result.input_tokens, 10);
    assert_eq!(result.output_tokens, 5);
}

/// Stream events from a single-step routine.
#[tokio::test]
async fn stream_events_single_step() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "stream-test".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "work".into(),
            step_type: "agent".into(),
            model_id: Some(model_id),
            council_id: None,
            agent_id: Some(agent_id),
            lambda_id: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "worker", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("Streamed output."))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Do work");
    let mut handle = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    let mut saw_step_started = false;
    let mut saw_step_completed = false;
    let mut saw_done = false;

    while let Some(event) = handle.recv().await {
        match event {
            RoutineEvent::StepStarted {
                step_name,
                step_type,
                ..
            } => {
                assert_eq!(step_name, "work");
                assert_eq!(step_type, "agent");
                saw_step_started = true;
            }
            RoutineEvent::StepCompleted { result, .. } => {
                assert!(result.passed);
                saw_step_completed = true;
            }
            RoutineEvent::Done { result } => {
                assert_eq!(result.output, "Streamed output.");
                saw_done = true;
            }
            _ => {}
        }
    }

    assert!(saw_step_started, "should have received StepStarted");
    assert!(saw_step_completed, "should have received StepCompleted");
    assert!(saw_done, "should have received Done");
}

/// Two agent steps connected by an edge: implement → review (terminal).
#[tokio::test]
async fn two_step_chain() {
    let model_id = Uuid::new_v4();
    let coder_id = Uuid::new_v4();
    let reviewer_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let step2_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "code-review".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![
            RoutineStepManifest {
                id: step1_id,
                routine_id,
                name: "implement".into(),
                step_type: "agent".into(),
                model_id: Some(model_id),
                council_id: None,
                agent_id: Some(coder_id),
                lambda_id: None,
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                id: step2_id,
                routine_id,
                name: "review".into(),
                step_type: "agent".into(),
                model_id: Some(model_id),
                council_id: None,
                agent_id: Some(reviewer_id),
                lambda_id: None,
                config: serde_json::json!({}),
                order_index: 1,
            },
        ],
        edges: vec![RoutineEdgeManifest {
            id: Uuid::new_v4(),
            routine_id,
            source_step_id: step1_id,
            target_step_id: step2_id,
            condition: "always".into(),
            metadata: serde_json::json!({}),
        }],
    };

    let manifest = Manifest {
        agents: vec![
            agent(coder_id, "coder", model_id),
            agent(reviewer_id, "reviewer", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("Step done."))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Feature", "Add login");

    let mut handle = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    let mut step_names = Vec::new();
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::StepStarted { step_name, .. } = event {
            step_names.push(step_name);
        }
    }

    assert_eq!(step_names, vec!["implement", "review"]);

    let result = handle.output().await.unwrap();
    assert!(result.passed);
}

/// Gate step: agent → gate (pass) → terminal.
#[tokio::test]
async fn gate_step_pass() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let gate_id = Uuid::new_v4();
    let terminal_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "gated-routine".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![
            RoutineStepManifest {
                id: step1_id,
                routine_id,
                name: "implement".into(),
                step_type: "agent".into(),
                model_id: Some(model_id),
                council_id: None,
                agent_id: Some(agent_id),
                lambda_id: None,
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                id: gate_id,
                routine_id,
                name: "quality-check".into(),
                step_type: "gate".into(),
                model_id: Some(model_id),
                council_id: None,
                agent_id: Some(agent_id),
                lambda_id: None,
                config: serde_json::json!({ "criteria": "Code must compile and have tests." }),
                order_index: 1,
            },
            RoutineStepManifest {
                id: terminal_id,
                routine_id,
                name: "done".into(),
                step_type: "terminal".into(),
                model_id: None,
                council_id: None,
                agent_id: None,
                lambda_id: None,
                config: serde_json::json!({}),
                order_index: 2,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                id: Uuid::new_v4(),
                routine_id,
                source_step_id: step1_id,
                target_step_id: gate_id,
                condition: "always".into(),
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                id: Uuid::new_v4(),
                routine_id,
                source_step_id: gate_id,
                target_step_id: terminal_id,
                condition: "on_pass".into(),
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        ..Default::default()
    };

    // The mock LLM says "PASS" which the gate parser detects as passed
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("Code looks good. PASS."))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Build feature");
    let result = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run(task)
        .await
        .unwrap();

    // Terminal step returns the last result
    assert!(result.passed);
}

/// Lambda step execution with mock runner.
#[tokio::test]
async fn lambda_step() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let lambda_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let step2_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "lambda-routine".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![
            RoutineStepManifest {
                id: step1_id,
                routine_id,
                name: "implement".into(),
                step_type: "agent".into(),
                model_id: Some(model_id),
                council_id: None,
                agent_id: Some(agent_id),
                lambda_id: None,
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                id: step2_id,
                routine_id,
                name: "run-tests".into(),
                step_type: "lambda".into(),
                model_id: None,
                council_id: None,
                agent_id: None,
                lambda_id: Some(lambda_id),
                config: serde_json::json!({}),
                order_index: 1,
            },
        ],
        edges: vec![RoutineEdgeManifest {
            id: Uuid::new_v4(),
            routine_id,
            source_step_id: step1_id,
            target_step_id: step2_id,
            condition: "always".into(),
            metadata: serde_json::json!({}),
        }],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        lambdas: vec![LambdaManifest {
            id: lambda_id,
            name: "test-runner".into(),
            description: None,
            path: "scripts/run_tests.sh".into(),
            body: "#!/bin/bash\necho 'tests pass'".into(),
            interpreter: "bash".into(),
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_loader(StaticLoader(manifest))
        .with_model_factory(MockFactory::new("Code written."))
        .with_lambda_runner(MockLambdaRunner)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Write and test");
    let result = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "lambda executed successfully");
}

/// Lambda step fails → result.passed is false.
#[tokio::test]
async fn lambda_step_failure() {
    let lambda_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "failing-lambda".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "run-script".into(),
            step_type: "lambda".into(),
            model_id: None,
            council_id: None,
            agent_id: None,
            lambda_id: Some(lambda_id),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        routines: vec![routine],
        lambdas: vec![LambdaManifest {
            id: lambda_id,
            name: "failing-script".into(),
            description: None,
            path: "scripts/fail.sh".into(),
            body: "#!/bin/bash\nexit 1".into(),
            interpreter: "bash".into(),
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_loader(StaticLoader(manifest))
        .with_model_factory(MockFactory::new("irrelevant"))
        .with_lambda_runner(FailingLambdaRunner)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Run failing script");
    let result = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(!result.passed);
    assert!(result.output.contains("exit"));
}

/// Lambda step without a runner configured → clear error.
#[tokio::test]
async fn lambda_step_no_runner() {
    let lambda_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "no-runner".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "run-script".into(),
            step_type: "lambda".into(),
            model_id: None,
            council_id: None,
            agent_id: None,
            lambda_id: Some(lambda_id),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        routines: vec![routine],
        lambdas: vec![LambdaManifest {
            id: lambda_id,
            name: "test".into(),
            description: None,
            path: "test.sh".into(),
            body: String::new(),
            interpreter: "bash".into(),
        }],
        ..Default::default()
    };

    // No lambda runner configured
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("irrelevant"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Run script");

    let mut handle = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    let mut saw_failure = false;
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::StepFailed { error, .. } = event {
            assert!(error.contains("LambdaRunner"));
            saw_failure = true;
        }
    }

    assert!(
        saw_failure,
        "should have received StepFailed for missing runner"
    );
}

/// Routine not found → error.
#[tokio::test]
async fn routine_not_found() {
    let provider = Provider::builder()
        .with_manifest(Manifest::default())
        .with_model_factory(MockFactory::new("irrelevant"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let err = provider.routine_by_id(Uuid::new_v4());

    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("not found"));
}

/// Terminal fail step produces a failed result.
#[tokio::test]
async fn terminal_fail_step() {
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "fail-routine".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "abort".into(),
            step_type: "terminal_fail".into(),
            model_id: None,
            council_id: None,
            agent_id: None,
            lambda_id: None,
            config: serde_json::json!({ "reason": "Blocked by policy." }),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        routines: vec![routine],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("irrelevant"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Desc");
    let result = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(!result.passed);
    assert_eq!(result.output, "Blocked by policy.");
}

/// Council step with decompose strategy.
#[tokio::test]
async fn council_decompose() {
    let model_id = Uuid::new_v4();
    let leader_id = Uuid::new_v4();
    let member_id = Uuid::new_v4();
    let council_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "council-routine".into(),
        description: None,
        trigger: "manual".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "council-step".into(),
            step_type: "council".into(),
            model_id: None,
            council_id: Some(council_id),
            agent_id: None,
            lambda_id: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![
            agent(leader_id, "leader", model_id),
            agent(member_id, "member", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        councils: vec![CouncilManifest {
            id: council_id,
            name: "test-council".into(),
            delegation_strategy: "decompose".into(),
            leader_agent_id: leader_id,
            members: vec![CouncilMemberManifest {
                agent_id: member_id,
                agent_name: "member".into(),
                priority: 1,
            }],
        }],
        ..Default::default()
    };

    // Mock LLM responds with a numbered subtask list for decomposition,
    // then does member work, then aggregates.
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("1. Do the thing"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Council task", "Build the feature");
    let result = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run(task)
        .await
        .unwrap();

    // Council always returns a result (even with mock LLM)
    assert!(!result.output.is_empty());
}

/// Cron execution: runs until the agent signals pass via JSON.
#[tokio::test]
async fn cron_execution() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "cron-routine".into(),
        description: None,
        trigger: "cron".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "check".into(),
            step_type: "gate".into(),
            model_id: Some(model_id),
            council_id: None,
            agent_id: Some(agent_id),
            lambda_id: None,
            config: serde_json::json!({ "criteria": "Check system health" }),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "monitor", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        ..Default::default()
    };

    // Mock returns a gate_verdict tool call → cron routine sees "pass" verdict
    // and completes after one cycle.
    let verdict_response = ChatResponse {
        text: Some("Evaluation complete.".into()),
        tool_calls: vec![ToolCall {
            id: "call_1".into(),
            name: "gate_verdict".into(),
            arguments: r#"{"verdict": "pass", "reasoning": "All checks passed"}"#.into(),
        }],
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        },
    };
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![verdict_response]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = TaskType::Cron {
        task: None,
        project_id,
        interval: Duration::from_millis(50),
        timeout: Duration::from_secs(5),
    };

    let mut handle = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    let mut cycles_started = 0u32;
    let mut cycles_completed = 0u32;

    while let Some(event) = handle.recv().await {
        match event {
            RoutineEvent::CronCycleStarted { .. } => cycles_started += 1,
            RoutineEvent::CronCycleCompleted { .. } => cycles_completed += 1,
            _ => {}
        }
    }

    assert_eq!(cycles_started, 1, "should have started 1 cron cycle");
    assert_eq!(cycles_completed, 1, "should have completed 1 cron cycle");

    let result = handle.output().await.unwrap();
    assert!(result.passed, "cron routine should pass with gate verdict");
    assert_eq!(
        result.data.get("verdict").and_then(|v| v.as_str()),
        Some("pass"),
        "should have structured verdict data"
    );
}

/// Cron cancellation: cancel the handle mid-execution and verify it stops.
#[tokio::test]
async fn cron_cancellation() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let routine_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();

    let routine = RoutineManifest {
        id: routine_id,
        name: "cancel-cron".into(),
        description: None,
        trigger: "cron".into(),
        is_active: true,
        is_default: false,
        max_retries: 0,
        metadata: serde_json::json!({}),
        steps: vec![RoutineStepManifest {
            id: step_id,
            routine_id,
            name: "poll".into(),
            step_type: "agent".into(),
            model_id: Some(model_id),
            council_id: None,
            agent_id: Some(agent_id),
            lambda_id: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "poller", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![routine],
        ..Default::default()
    };

    // Always returns "wait" so the cron never finishes on its own.
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("wait"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = TaskType::Cron {
        task: None,
        project_id,
        interval: Duration::from_millis(50),
        timeout: Duration::from_secs(30),
    };

    let mut handle = provider
        .routine_by_id(routine_id)
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    // Wait for at least one cycle to complete, then cancel.
    let mut saw_cycle = false;
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::CronCycleCompleted { .. } = event {
            saw_cycle = true;
            handle.cancel();
            break;
        }
    }

    assert!(
        saw_cycle,
        "should have seen at least one cron cycle before cancel"
    );

    // The handle should finish after cancellation.
    let result = handle.output().await.unwrap();
    // Cancelled cron returns the last cycle result (passed=true for a "wait" output
    // since agent step itself succeeds, but the cron was cancelled not completed).
    // The key assertion is that the routine terminates rather than running forever.
    assert!(
        !result.output.is_empty(),
        "cancelled cron should still return a result"
    );
}

// ===========================================================================
// Delegation tests
// ===========================================================================

/// Agent with delegate_to available can delegate to another agent.
#[tokio::test]
async fn delegation_basic() {
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
        .with_model_factory(MockFactory::new("Delegation result from reviewer."))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    // Build the coder agent — it should have delegate_to since reviewer exists.
    let runner = provider
        .agent_by_name("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(
        tool_names.contains(&"delegate_to"),
        "delegate_to should be auto-injected when other agents exist. Tools: {:?}",
        tool_names
    );
}

/// Single agent should NOT get delegate_to (no one to delegate to).
#[tokio::test]
async fn delegation_not_injected_for_single_agent() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![agent(agent_id, "solo", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("irrelevant"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("solo")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(
        !tool_names.contains(&"delegate_to"),
        "delegate_to should NOT be injected for a single agent"
    );
}

// ---------------------------------------------------------------------------
// Helper: static manifest loader for builder tests
// ---------------------------------------------------------------------------

struct StaticLoader(Manifest);

#[async_trait::async_trait]
impl nenjo::ManifestLoader for StaticLoader {
    async fn load(&self) -> Result<Manifest> {
        Ok(self.0.clone())
    }
}
