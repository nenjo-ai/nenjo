//! Integration tests for routine execution via Provider.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{
    AgentManifest, CouncilDelegationStrategy, CouncilManifest, CouncilMemberManifest, Manifest,
    ModelManifest, ProjectManifest, PromptConfig, PromptTemplates, RoutineEdgeCondition,
    RoutineEdgeManifest, RoutineManifest, RoutineMetadata, RoutineStepManifest, RoutineStepType,
    model_manifest_slug,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider, ToolFactory};
use nenjo::routines::RoutineEvent;
use nenjo::{CronInput, ProjectLocation, RoutineRun, Slug, TaskInput};
use nenjo::{Tool, ToolCategory, ToolResult};
use nenjo_models::traits::{
    ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall,
};

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
            provider_tool_calls: vec![],
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
    seen_messages: Option<Arc<Mutex<Vec<Vec<ChatMessage>>>>>,
}

#[async_trait::async_trait]
impl ModelProvider for SequentialResponseMockLlm {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        if let Some(seen_messages) = &self.seen_messages {
            seen_messages
                .lock()
                .unwrap()
                .push(request.messages.to_vec());
        }
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
    seen_messages: Option<Arc<Mutex<Vec<Vec<ChatMessage>>>>>,
}

impl SequentialResponseMockFactory {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Arc::new(responses),
            call_index: Arc::new(AtomicUsize::new(0)),
            seen_messages: None,
        }
    }

    fn with_message_recording(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Arc::new(responses),
            call_index: Arc::new(AtomicUsize::new(0)),
            seen_messages: Some(Arc::new(Mutex::new(Vec::new()))),
        }
    }

    fn seen_messages(&self) -> Option<Arc<Mutex<Vec<Vec<ChatMessage>>>>> {
        self.seen_messages.clone()
    }
}

impl ModelProviderFactory for SequentialResponseMockFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(SequentialResponseMockLlm {
            responses: self.responses.clone(),
            call_index: self.call_index.clone(),
            seen_messages: self.seen_messages.clone(),
        }))
    }
}

struct FallibleSequentialResponseMockLlm {
    responses: Arc<Vec<Result<ChatResponse, String>>>,
    call_index: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ModelProvider for FallibleSequentialResponseMockLlm {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        match self.responses.get(idx).unwrap_or_else(|| {
            self.responses
                .last()
                .expect("fallible sequential mock must have at least one response")
        }) {
            Ok(response) => Ok(response.clone()),
            Err(error) => anyhow::bail!(error.clone()),
        }
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

struct FallibleSequentialResponseMockFactory {
    responses: Arc<Vec<Result<ChatResponse, String>>>,
    call_index: Arc<AtomicUsize>,
}

impl FallibleSequentialResponseMockFactory {
    fn new(responses: Vec<Result<ChatResponse, String>>) -> Self {
        Self {
            responses: Arc::new(responses),
            call_index: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ModelProviderFactory for FallibleSequentialResponseMockFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(FallibleSequentialResponseMockLlm {
            responses: self.responses.clone(),
            call_index: self.call_index.clone(),
        }))
    }
}

struct RecordingToolsMockLlm {
    response: ChatResponse,
    seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ModelProvider for RecordingToolsMockLlm {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        self.seen_tools
            .lock()
            .unwrap()
            .push(request.tools.map(tool_names).unwrap_or_default());
        Ok(self.response.clone())
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

struct RecordingToolsMockFactory {
    response: ChatResponse,
    seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

impl RecordingToolsMockFactory {
    fn new(response: ChatResponse) -> Self {
        Self {
            response,
            seen_tools: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn seen_tools(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
        self.seen_tools.clone()
    }
}

impl ModelProviderFactory for RecordingToolsMockFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(RecordingToolsMockLlm {
            response: self.response.clone(),
            seen_tools: self.seen_tools.clone(),
        }))
    }
}

struct RecordingMessagesMockLlm {
    response: ChatResponse,
    seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[async_trait::async_trait]
impl ModelProvider for RecordingMessagesMockLlm {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        self.seen_messages
            .lock()
            .unwrap()
            .push(request.messages.to_vec());
        Ok(self.response.clone())
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

struct RecordingMessagesMockFactory {
    response: ChatResponse,
    seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

impl RecordingMessagesMockFactory {
    fn new(response: ChatResponse) -> Self {
        Self {
            response,
            seen_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn seen_messages(&self) -> Arc<Mutex<Vec<Vec<ChatMessage>>>> {
        self.seen_messages.clone()
    }
}

impl ModelProviderFactory for RecordingMessagesMockFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(RecordingMessagesMockLlm {
            response: self.response.clone(),
            seen_messages: self.seen_messages.clone(),
        }))
    }
}

struct ProgressTool;

#[async_trait::async_trait]
impl Tool for ProgressTool {
    fn name(&self) -> &str {
        "progress_tool"
    }

    fn description(&self) -> &str {
        "Records deterministic test progress."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: "progress recorded".into(),
            error: None,
        })
    }
}

struct ProgressToolFactory;

#[async_trait::async_trait]
impl ToolFactory for ProgressToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(ProgressTool)]
    }
}

fn tool_names(tools: &[nenjo::ToolSpec]) -> Vec<String> {
    tools.iter().map(|tool| tool.name.clone()).collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_handoff_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["work"],
        "properties": {
            "work": {"type": "string", "minLength": 1}
        },
        "additionalProperties": false
    })
}

fn implicit_done_target() -> String {
    Slug::derive("__done").to_string()
}

fn ensure_routable_edge_handoff_schemas(routine: &mut RoutineManifest) {
    let step_types = routine
        .steps
        .iter()
        .map(|step| (step.slug.clone(), step.step_type))
        .collect::<std::collections::HashMap<_, _>>();

    for edge in &mut routine.edges {
        if !matches!(
            step_types.get(&edge.source_step),
            Some(RoutineStepType::Agent | RoutineStepType::Gate)
        ) {
            continue;
        }
        if !edge.metadata.is_object() {
            edge.metadata = serde_json::json!({});
        }
        let object = edge
            .metadata
            .as_object_mut()
            .expect("metadata was normalized to an object");
        object
            .entry("handoff_schema".to_string())
            .or_insert_with(default_handoff_schema);
    }
}

fn test_task(_project_id: Uuid, title: &str, desc: &str) -> TaskInput {
    TaskInput::new(title, desc)
        .with_project("project")
        .with_task_id(Uuid::nil())
        .source("test")
}

fn model(_id: Uuid) -> ModelManifest {
    ModelManifest {
        slug: model_manifest_slug("mock", "mock-v1"),
        name: "test-model".into(),
        description: None,
        model: "mock-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
        native_tools: vec![],
    }
}

fn agent(_id: Uuid, name: &str, _model_id: Uuid) -> AgentManifest {
    AgentManifest {
        name: name.into(),
        slug: Slug::derive(name),
        description: Some(format!("{name} agent")),
        prompt_config: PromptConfig {
            system_prompt: format!("You are the {name} agent."),
            templates: PromptTemplates {
                task_execution: "Execute: {{ task.title }}\n{{ task.description }}".into(),
                chat_task: "{{ chat.message }}".into(),
                gate_eval:
                    "Evaluate:\n{{ routine.step.instructions }}\n\nIncoming handoffs:\n{{ routine.handoffs }}"
                        .into(),
                heartbeat_task: String::new(),
            },
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug("mock", "mock-v1")),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    }
}

fn messages_contain(messages: &[Vec<ChatMessage>], needle: &str) -> bool {
    messages
        .iter()
        .flatten()
        .any(|message| message.content.contains(needle))
}

fn project() -> ProjectManifest {
    ProjectManifest {
        name: "test-project".into(),
        slug: Slug::derive("test-project"),
        description: Some("A test project".into()),
        settings: serde_json::Value::Null,
    }
}

fn plain_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![],
        provider_tool_calls: vec![],
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        },
    }
}

fn canonical_routine(mut routine: RoutineManifest) -> RoutineManifest {
    if routine.metadata.entry_steps.is_empty() {
        let required_targets = routine
            .edges
            .iter()
            .filter(|edge| edge.condition != RoutineEdgeCondition::OnFail)
            .map(|edge| edge.target_step.clone())
            .collect::<std::collections::HashSet<_>>();
        if let Some(entry) = routine
            .steps
            .iter()
            .filter(|step| !required_targets.contains(&step.slug))
            .min_by_key(|step| step.order_index)
            .or_else(|| routine.steps.iter().min_by_key(|step| step.order_index))
        {
            routine.metadata.entry_steps = vec![entry.slug.clone()];
        }
    }

    let has_terminal = routine.steps.iter().any(|step| {
        matches!(
            step.step_type,
            RoutineStepType::Terminal | RoutineStepType::TerminalFail
        )
    });
    if !has_terminal {
        let terminal_slug = Slug::derive("__done");
        let outgoing = routine
            .edges
            .iter()
            .map(|edge| edge.source_step.clone())
            .collect::<std::collections::HashSet<_>>();
        let sink_steps = routine
            .steps
            .iter()
            .filter(|step| !outgoing.contains(&step.slug))
            .map(|step| (step.slug.clone(), step.step_type))
            .collect::<Vec<_>>();
        routine.steps.push(RoutineStepManifest {
            slug: terminal_slug.clone(),
            routine: routine.slug.clone(),
            name: "__done".into(),
            step_type: RoutineStepType::Terminal,
            council: None,
            agent: None,
            config: serde_json::json!({}),
            order_index: routine
                .steps
                .iter()
                .map(|step| step.order_index)
                .max()
                .unwrap_or(0)
                + 1,
        });
        for (source_step, step_type) in sink_steps {
            routine.edges.push(RoutineEdgeManifest {
                routine: routine.slug.clone(),
                source_step,
                target_step: terminal_slug.clone(),
                condition: match step_type {
                    RoutineStepType::Gate => RoutineEdgeCondition::OnPass,
                    RoutineStepType::Agent
                    | RoutineStepType::Council
                    | RoutineStepType::Terminal
                    | RoutineStepType::TerminalFail => RoutineEdgeCondition::Always,
                },
                metadata: serde_json::json!({}),
            });
        }
    }

    ensure_routable_edge_handoff_schemas(&mut routine);

    routine
}

fn route_response(
    text: &str,
    verdict: &str,
    reasoning: &str,
    next_steps: serde_json::Value,
) -> ChatResponse {
    let mut arguments = serde_json::json!({
        "verdict": verdict,
        "reasoning": reasoning,
        "output": text,
    });
    if !next_steps.is_null() {
        arguments["next_steps"] = next_steps;
    }

    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![ToolCall {
            id: format!("call_route_{verdict}"),
            name: "route_next_steps".into(),
            arguments: arguments.to_string(),
        }],
        provider_tool_calls: vec![],
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        },
    }
}

fn progress_tool_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![ToolCall {
            id: "call_progress".into(),
            name: "progress_tool".into(),
            arguments: "{}".into(),
        }],
        provider_tool_calls: vec![],
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        },
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
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "simple-routine".into(),
        slug: Slug::derive("simple-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("simple-routine"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![route_response(
            "Implementation complete.",
            "pass",
            "Implementation is complete",
            serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
        )]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Add auth", "Implement JWT authentication");
    let result = provider
        .routine("simple-routine")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "Implementation complete.");
    assert_eq!(result.input_tokens, 10);
    assert_eq!(result.output_tokens, 5);
}

#[tokio::test]
async fn routine_agent_request_includes_route_next_steps_tool() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "tool-check-routine".into(),
        slug: Slug::derive("tool-check-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("tool-check-routine"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![ProjectManifest { ..project() }],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingToolsMockFactory::new(route_response(
        "Implementation complete.",
        "pass",
        "Implementation is complete",
        serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
    ));
    let seen_tools = factory.seen_tools();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let work_dir = tempfile::tempdir().unwrap();
    let task = RoutineRun::task(test_task(
        project_id,
        "Add auth",
        "Implement JWT authentication",
    ))
    .project_location(ProjectLocation::from_git(nenjo::types::GitContext {
        branch: "agent/test".into(),
        target_branch: "main".into(),
        work_dir: work_dir.path().to_string_lossy().to_string(),
        repo_url: "https://github.com/nenjo-ai/dashboard.git".into(),
    }));

    provider
        .routine("tool-check-routine")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    let seen_tools = seen_tools.lock().unwrap();
    assert!(
        seen_tools
            .first()
            .is_some_and(|tools| tools.iter().any(|name| name == "route_next_steps")),
        "route_next_steps should be sent in the routine model request. Tool requests: {:?}",
        *seen_tools
    );
}

#[tokio::test]
async fn routine_agent_step_renders_step_instructions_context_var() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();
    let instructions = "Use the migration checklist before editing files.";

    let mut coder = agent(agent_id, "coder", model_id);
    coder.prompt_config.templates.task_execution =
        "Step instructions:\n{{ routine.step.instructions }}\n\nTask:\n{{ task.description }}"
            .into();

    let routine = RoutineManifest {
        name: "agent-instructions".into(),
        slug: Slug::derive("agent-instructions"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("agent-instructions"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({
                "instructions": instructions,
            }),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![coder],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingMessagesMockFactory::new(route_response(
        "Implementation complete.",
        "pass",
        "Implementation is complete",
        serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
    ));
    let seen_messages = factory.seen_messages();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    provider
        .routine("agent-instructions")
        .unwrap()
        .run(test_task(
            Uuid::new_v4(),
            "Add auth",
            "Implement JWT authentication",
        ))
        .await
        .unwrap();

    let seen_messages = seen_messages.lock().unwrap();
    assert!(
        messages_contain(&seen_messages, instructions),
        "agent step instructions should render through routine.step.instructions. Messages: {seen_messages:#?}"
    );
}

#[tokio::test]
async fn routine_agent_step_renders_project_context() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();

    let mut coder = agent(agent_id, "coder", model_id);
    coder.prompt_config.templates.task_execution =
        "Project context:\n{{ project.context }}\n\nProject XML:\n{{ project }}".into();

    let routine = RoutineManifest {
        name: "agent-project-context".into(),
        slug: Slug::derive("agent-project-context"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("agent-project-context"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let mut project = project();
    project.settings = serde_json::json!({
        "context": "Use {{ project.name }} conventions."
    });
    let project_slug = project.slug.clone();

    let manifest = Manifest {
        agents: vec![coder],
        models: vec![model(model_id)],
        projects: vec![project],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingMessagesMockFactory::new(route_response(
        "Implementation complete.",
        "pass",
        "Implementation is complete",
        serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
    ));
    let seen_messages = factory.seen_messages();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    provider
        .routine("agent-project-context")
        .unwrap()
        .run(
            test_task(
                Uuid::new_v4(),
                "Add dark mode",
                "Implement dark mode support",
            )
            .with_project(project_slug),
        )
        .await
        .unwrap();

    let seen_messages = seen_messages.lock().unwrap();
    assert!(
        messages_contain(&seen_messages, "Use test-project conventions."),
        "agent step should render project.context for project-backed routine tasks. Messages: {seen_messages:#?}"
    );
    assert!(
        messages_contain(
            &seen_messages,
            "<context>Use test-project conventions.</context>"
        ),
        "agent step should include rendered project context in project XML. Messages: {seen_messages:#?}"
    );
}

#[tokio::test]
async fn cron_triggered_agent_step_uses_task_execution_template() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let mut coder = agent(agent_id, "coder", model_id);
    coder.prompt_config.templates.task_execution = "TASK TEMPLATE: {{ task.description }}".into();

    let routine = RoutineManifest {
        name: "cron-agent-template".into(),
        slug: Slug::derive("cron-agent-template"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Cron,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("cron-agent-template"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![coder],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingMessagesMockFactory::new(route_response(
        "Implementation complete.",
        "pass",
        "Implementation is complete",
        serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
    ));
    let seen_messages = factory.seen_messages();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let run = RoutineRun::cron(CronInput {
        task: Some(test_task(
            Uuid::new_v4(),
            "Add auth",
            "Implement JWT authentication",
        )),
        project: Some(Slug::derive("project")),
        schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_millis(50)),
        start_at: None,
        timeout: Duration::from_secs(5),
    });

    provider
        .routine("cron-agent-template")
        .unwrap()
        .run(run)
        .await
        .unwrap();

    let seen_messages = seen_messages.lock().unwrap();
    assert!(
        messages_contain(&seen_messages, "TASK TEMPLATE"),
        "cron-triggered agent steps should use task_execution. Messages: {seen_messages:#?}"
    );
}

#[tokio::test]
async fn cron_triggered_agent_step_without_project_uses_task_execution_template() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let mut coder = agent(agent_id, "coder", model_id);
    coder.prompt_config.templates.task_execution = "TASK TEMPLATE: {{ task.description }}".into();

    let routine = RoutineManifest {
        name: "cron-agent-no-project".into(),
        slug: Slug::derive("cron-agent-no-project"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Cron,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("cron-agent-no-project"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![coder],
        models: vec![model(model_id)],
        projects: vec![],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingMessagesMockFactory::new(route_response(
        "Implementation complete.",
        "pass",
        "Implementation is complete",
        serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
    ));
    let seen_messages = factory.seen_messages();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let run = RoutineRun::cron(CronInput {
        task: None,
        project: None,
        schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_millis(50)),
        start_at: None,
        timeout: Duration::from_secs(5),
    });

    provider
        .routine("cron-agent-no-project")
        .unwrap()
        .run(run)
        .await
        .unwrap();

    let seen_messages = seen_messages.lock().unwrap();
    assert!(
        messages_contain(&seen_messages, "TASK TEMPLATE"),
        "cron-triggered agent steps without a project should use task_execution. Messages: {seen_messages:#?}"
    );
}

#[tokio::test]
async fn routine_gate_step_renders_step_instructions_context_var() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();
    let instructions = "Reject unless the output cites the acceptance criteria.";

    let mut reviewer = agent(agent_id, "reviewer", model_id);
    reviewer.prompt_config.templates.gate_eval =
        "Gate instructions:\n{{ routine.step.instructions }}\n\nIncoming handoffs:\n{{ routine.handoffs }}"
            .into();

    let routine = RoutineManifest {
        name: "gate-instructions".into(),
        slug: Slug::derive("gate-instructions"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("gate-instructions"),
            name: "review".into(),
            step_type: RoutineStepType::Gate,
            council: None,
            agent: Some(Slug::derive("reviewer")),
            config: serde_json::json!({
                "instructions": instructions,
            }),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![reviewer],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingMessagesMockFactory::new(route_response(
        "Gate passed.",
        "pass",
        "Criteria are satisfied",
        serde_json::json!([{
            "target_step": implicit_done_target(),
            "handoff": {"work": "gate accepted the prior result"}
        }]),
    ));
    let seen_messages = factory.seen_messages();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    provider
        .routine("gate-instructions")
        .unwrap()
        .run(test_task(
            Uuid::new_v4(),
            "Review auth",
            "Confirm JWT implementation is acceptable",
        ))
        .await
        .unwrap();

    let seen_messages = seen_messages.lock().unwrap();
    assert!(
        messages_contain(&seen_messages, instructions),
        "gate step instructions should render through routine.step.instructions. Messages: {seen_messages:#?}"
    );
}

/// If an agent omits route_next_steps, the runtime should route back with an
/// explicit corrective instruction and accept the follow-up tool call.
#[tokio::test]
async fn single_agent_step_retries_until_route_next_steps() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "retry-for-route-next-steps".into(),
        slug: Slug::derive("retry-for-route-next-steps"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("retry-for-route-next-steps"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let mut coder = agent(agent_id, "coder", model_id);
    coder.prompt_config.templates.chat_task = "CHAT TEMPLATE: {{ chat.message }}".into();

    let manifest = Manifest {
        agents: vec![coder],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let factory = SequentialResponseMockFactory::with_message_recording(vec![
        ChatResponse {
            text: Some("Implementation complete.".into()),
            tool_calls: vec![],
            provider_tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
            },
        },
        route_response(
            "Implementation complete.",
            "pass",
            "Implementation is complete",
            serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
        ),
    ]);
    let seen_messages = factory.seen_messages().unwrap();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Add auth", "Implement JWT authentication");
    let result = provider
        .routine("retry-for-route-next-steps")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "Implementation complete.");
    assert_eq!(result.input_tokens, 20);
    assert_eq!(result.output_tokens, 10);

    let seen_messages = seen_messages.lock().unwrap();
    assert_eq!(seen_messages.len(), 2);
    let retry_messages = &seen_messages[1];
    assert!(
        messages_contain(
            std::slice::from_ref(retry_messages),
            "call `route_next_steps`"
        ),
        "retry turn should instruct the agent to call route_next_steps. Messages: {retry_messages:#?}"
    );
    assert!(
        messages_contain(
            std::slice::from_ref(retry_messages),
            "If work remains, continue executing the step using the available tools"
        ),
        "retry turn should allow unfinished work to continue. Messages: {retry_messages:#?}"
    );
    assert!(
        !messages_contain(std::slice::from_ref(retry_messages), "CHAT TEMPLATE"),
        "retry turn should not render the chat template. Messages: {retry_messages:#?}"
    );
}

#[tokio::test]
async fn agent_step_tool_progress_resets_route_next_steps_no_progress_counter() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "route-progress-reset".into(),
        slug: Slug::derive("route-progress-reset"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("route-progress-reset"),
            name: "implement".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("coder")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            ChatResponse {
                text: Some("I need to inspect the project first.".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            },
            progress_tool_response("Writing the implementation now."),
            ChatResponse {
                text: Some("Progress recorded; I still need to finalize.".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            },
            ChatResponse {
                text: Some("I need one more verification pass.".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            },
            route_response(
                "Implementation complete.",
                "pass",
                "Implementation is complete",
                serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
            ),
        ]))
        .with_tool_factory(ProgressToolFactory)
        .build()
        .await
        .unwrap();

    let result = provider
        .routine("route-progress-reset")
        .unwrap()
        .run(test_task(
            Uuid::new_v4(),
            "Add auth",
            "Implement JWT authentication",
        ))
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "Implementation complete.");
}

#[tokio::test]
async fn agent_step_retries_invalid_route_next_steps_until_all_targets_are_handed_off() {
    let model_id = Uuid::new_v4();
    let start_agent_id = Uuid::new_v4();
    let branch_agent_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "retry-invalid-route".into(),
        slug: Slug::derive("retry-invalid-route"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata {
            schedule: None,
            entry_steps: vec![Slug::derive("start")],
            cron_task: None,
        },
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive("start"),
                routine: Slug::derive("retry-invalid-route"),
                name: "start".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("start-agent")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive("left"),
                routine: Slug::derive("retry-invalid-route"),
                name: "left".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("branch-agent")),
                config: serde_json::json!({}),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive("right"),
                routine: Slug::derive("retry-invalid-route"),
                name: "right".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("branch-agent")),
                config: serde_json::json!({}),
                order_index: 2,
            },
            RoutineStepManifest {
                slug: Slug::derive("done"),
                routine: Slug::derive("retry-invalid-route"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 3,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("retry-invalid-route"),
                source_step: Slug::derive("start"),
                target_step: Slug::derive("left"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({"purpose": "left branch"}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-invalid-route"),
                source_step: Slug::derive("start"),
                target_step: Slug::derive("right"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({"purpose": "right branch"}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-invalid-route"),
                source_step: Slug::derive("left"),
                target_step: Slug::derive("done"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-invalid-route"),
                source_step: Slug::derive("right"),
                target_step: Slug::derive("done"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![
            agent(start_agent_id, "start-agent", model_id),
            agent(branch_agent_id, "branch-agent", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let factory = SequentialResponseMockFactory::with_message_recording(vec![
        route_response(
            "start complete",
            "pass",
            "fan out",
            serde_json::json!([
                {"target_step": "left", "handoff": {"work": "left only"}}
            ]),
        ),
        route_response(
            "start complete",
            "pass",
            "fan out",
            serde_json::json!([
                {"target_step": "left", "handoff": {"work": "left handoff"}},
                {"target_step": "right", "handoff": {"work": "right handoff"}}
            ]),
        ),
        route_response(
            "branch complete",
            "pass",
            "branch done",
            serde_json::json!([{"target_step": "done", "handoff": {"work": "finish"}}]),
        ),
        route_response(
            "branch complete",
            "pass",
            "branch done",
            serde_json::json!([{"target_step": "done", "handoff": {"work": "finish"}}]),
        ),
    ]);
    let seen_messages = factory.seen_messages().unwrap();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let mut handle = provider
        .routine("retry-invalid-route")
        .unwrap()
        .run_stream(test_task(Uuid::new_v4(), "Task", "Do branches"))
        .await
        .unwrap();

    let mut step_names = Vec::new();
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::StepStarted { step_name, .. } = event {
            step_names.push(step_name);
        }
    }

    let result = handle.output().await.unwrap();
    assert!(result.passed);
    assert!(step_names.contains(&"left".to_string()));
    assert!(step_names.contains(&"right".to_string()));
    assert_eq!(step_names.last().map(String::as_str), Some("done"));

    let seen_messages = seen_messages.lock().unwrap();
    assert!(
        seen_messages.iter().any(|messages| messages_contain(
            std::slice::from_ref(messages),
            "route_next_steps requires exactly 2 next_steps item(s)"
        )),
        "invalid route call should produce a corrective retry with the validation error. Messages: {seen_messages:#?}"
    );
}

/// Stream events from a single-step routine.
#[tokio::test]
async fn stream_events_single_step() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "stream-test".into(),
        slug: Slug::derive("stream-test"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("stream-test"),
            name: "work".into(),
            step_type: RoutineStepType::Agent,
            council: None,
            agent: Some(Slug::derive("worker")),
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "worker", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![route_response(
            "Streamed output.",
            "pass",
            "Work completed successfully",
            serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
        )]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Do work");
    let mut handle = provider
        .routine("stream-test")
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
                if step_name == "work" {
                    assert_eq!(step_type, "agent");
                    saw_step_started = true;
                }
            }
            RoutineEvent::StepCompleted { result, .. } => {
                assert!(result.passed);
                saw_step_completed = true;
            }
            RoutineEvent::Done { task_id, result } => {
                assert_eq!(task_id, Some(Uuid::nil()));
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
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "code-review".into(),
        slug: Slug::derive("code-review"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(step1_id.to_string()),
                routine: Slug::derive("code-review"),
                name: "implement".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(step2_id.to_string()),
                routine: Slug::derive("code-review"),
                name: "review".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("reviewer")),
                config: serde_json::json!({}),
                order_index: 1,
            },
        ],
        edges: vec![RoutineEdgeManifest {
            routine: Slug::derive("code-review"),
            source_step: Slug::derive(step1_id.to_string()),
            target_step: Slug::derive(step2_id.to_string()),
            condition: RoutineEdgeCondition::Always,
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
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            route_response(
                "Step done.",
                "pass",
                "Implementation step passed",
                serde_json::json!([{
                    "target_step": step2_id.to_string(),
                    "handoff": {"work": "review this"}
                }]),
            ),
            route_response(
                "Step done.",
                "pass",
                "Review step passed",
                serde_json::json!([{"target_step": implicit_done_target(), "handoff": {"work": "finish"}}]),
            ),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Feature", "Add login");

    let mut handle = provider
        .routine("code-review")
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

    assert_eq!(step_names, vec!["implement", "review", "__done"]);

    let result = handle.output().await.unwrap();
    assert!(result.passed);
}

/// A fail route_next_steps verdict from an agent step terminates the routine and does not
/// continue along outgoing edges.
#[tokio::test]
async fn agent_step_route_fail_verdict_terminates_routine() {
    let model_id = Uuid::new_v4();
    let first_agent_id = Uuid::new_v4();
    let second_agent_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let step2_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "agent-fail-stops-routine".into(),
        slug: Slug::derive("agent-fail-stops-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(step1_id.to_string()),
                routine: Slug::derive("agent-fail-stops-routine"),
                name: "first-agent".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("first")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(step2_id.to_string()),
                routine: Slug::derive("agent-fail-stops-routine"),
                name: "second-agent".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("second")),
                config: serde_json::json!({}),
                order_index: 1,
            },
        ],
        edges: vec![RoutineEdgeManifest {
            routine: Slug::derive("agent-fail-stops-routine"),
            source_step: Slug::derive(step1_id.to_string()),
            target_step: Slug::derive(step2_id.to_string()),
            condition: RoutineEdgeCondition::Always,
            metadata: serde_json::json!({}),
        }],
    };

    let manifest = Manifest {
        agents: vec![
            agent(first_agent_id, "first", model_id),
            agent(second_agent_id, "second", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let fail_route_response = route_response(
        "The implementation is not acceptable.",
        "fail",
        "Critical acceptance criteria were missed",
        serde_json::Value::Null,
    );

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            fail_route_response,
            ChatResponse {
                text: Some("This should never run.".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            },
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Do work");
    let mut handle = provider
        .routine("agent-fail-stops-routine")
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

    assert_eq!(step_names, vec!["first-agent"]);

    let result = handle.output().await.unwrap();
    assert!(!result.passed);
    assert_eq!(result.output, "The implementation is not acceptable.");
    assert_eq!(
        result.data.get("verdict").and_then(|v| v.as_str()),
        Some("fail")
    );
    assert_eq!(
        result.data.get("reasoning").and_then(|v| v.as_str()),
        Some("Critical acceptance criteria were missed")
    );
}

/// Gate step: agent → gate (pass) → terminal.
#[tokio::test]
async fn gate_step_pass() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let gate_id = Uuid::new_v4();
    let terminal_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "gated-routine".into(),
        slug: Slug::derive("gated-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(step1_id.to_string()),
                routine: Slug::derive("gated-routine"),
                name: "implement".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(gate_id.to_string()),
                routine: Slug::derive("gated-routine"),
                name: "quality-check".into(),
                step_type: RoutineStepType::Gate,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({ "instructions": "Code must compile and have tests." }),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive(terminal_id.to_string()),
                routine: Slug::derive("gated-routine"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 2,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("gated-routine"),
                source_step: Slug::derive(step1_id.to_string()),
                target_step: Slug::derive(gate_id.to_string()),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("gated-routine"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(terminal_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            route_response(
                "Implementation complete.",
                "pass",
                "Implementation succeeded",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "verify this implementation"}
                }]),
            ),
            route_response(
                "Code looks good.",
                "pass",
                "Criteria were satisfied",
                serde_json::json!([{
                    "target_step": terminal_id.to_string(),
                    "handoff": {"work": "finish the routine"}
                }]),
            ),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Build feature");
    let result = provider
        .routine("gated-routine")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    // Terminal step returns the last result
    assert!(result.passed);
}

#[tokio::test]
async fn gate_always_edge_is_invalid() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let gate_id = Uuid::new_v4();
    let terminal_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "invalid-gate-routing".into(),
        slug: Slug::derive("invalid-gate-routing"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(step1_id.to_string()),
                routine: Slug::derive("invalid-gate-routing"),
                name: "analyze_and_develop".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(gate_id.to_string()),
                routine: Slug::derive("invalid-gate-routing"),
                name: "verify".into(),
                step_type: RoutineStepType::Gate,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({ "instructions": "Acceptance criteria must pass." }),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive(terminal_id.to_string()),
                routine: Slug::derive("invalid-gate-routing"),
                name: "complete".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 2,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("invalid-gate-routing"),
                source_step: Slug::derive(step1_id.to_string()),
                target_step: Slug::derive(gate_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("invalid-gate-routing"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(terminal_id.to_string()),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            route_response(
                "Implementation complete.",
                "pass",
                "Implementation succeeded",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "verify this implementation"}
                }]),
            ),
            route_response(
                "Needs changes.",
                "fail",
                "Criteria were not satisfied",
                serde_json::json!([{
                    "target_step": step1_id.to_string(),
                    "handoff": {"work": "revise the implementation"}
                }]),
            ),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Build feature");
    let err = provider
        .routine("invalid-gate-routing")
        .unwrap()
        .run(task)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("must use on_pass/on_fail"));
}

#[tokio::test]
async fn gate_on_fail_routes_back_before_completion() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let gate_id = Uuid::new_v4();
    let terminal_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "retry-gated-routine".into(),
        slug: Slug::derive("retry-gated-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(step1_id.to_string()),
                routine: Slug::derive("retry-gated-routine"),
                name: "analyze_and_develop".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(gate_id.to_string()),
                routine: Slug::derive("retry-gated-routine"),
                name: "verify".into(),
                step_type: RoutineStepType::Gate,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({ "instructions": "Acceptance criteria must pass." }),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive(terminal_id.to_string()),
                routine: Slug::derive("retry-gated-routine"),
                name: "complete".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 2,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("retry-gated-routine"),
                source_step: Slug::derive(step1_id.to_string()),
                target_step: Slug::derive(gate_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-gated-routine"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(step1_id.to_string()),
                condition: RoutineEdgeCondition::OnFail,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-gated-routine"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(terminal_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            route_response(
                "Implementation complete.",
                "pass",
                "Implementation succeeded",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "verify this implementation"}
                }]),
            ),
            route_response(
                "Needs changes.",
                "fail",
                "Criteria were not satisfied",
                serde_json::json!([{
                    "target_step": step1_id.to_string(),
                    "handoff": {"work": "revise the implementation"}
                }]),
            ),
            route_response(
                "Implementation revised.",
                "pass",
                "Implementation succeeded",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "verify this revision"}
                }]),
            ),
            route_response(
                "Looks good.",
                "pass",
                "Criteria were satisfied",
                serde_json::json!([{
                    "target_step": terminal_id.to_string(),
                    "handoff": {"work": "finish the routine"}
                }]),
            ),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Task", "Build feature");
    let mut handle = provider
        .routine("retry-gated-routine")
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    let mut analyze_starts = 0;
    let mut verify_starts = 0;
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::StepStarted { step_name, .. } = event {
            match step_name.as_str() {
                "analyze_and_develop" => analyze_starts += 1,
                "verify" => verify_starts += 1,
                _ => {}
            }
        }
    }

    let result = handle.output().await.unwrap();

    assert!(result.passed);
    assert_eq!(analyze_starts, 2);
    assert_eq!(verify_starts, 2);
}

#[tokio::test]
async fn gate_on_fail_edge_exhausts_after_max_attempts() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step1_id = Uuid::new_v4();
    let gate_id = Uuid::new_v4();
    let done_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();
    let _retry_edge_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "retry-exhaustion-routine".into(),
        slug: Slug::derive("retry-exhaustion-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(step1_id.to_string()),
                routine: Slug::derive("retry-exhaustion-routine"),
                name: "analyze_and_develop".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(gate_id.to_string()),
                routine: Slug::derive("retry-exhaustion-routine"),
                name: "verify".into(),
                step_type: RoutineStepType::Gate,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({ "instructions": "Acceptance criteria must pass." }),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive(done_id.to_string()),
                routine: Slug::derive("retry-exhaustion-routine"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({ "message": "Done." }),
                order_index: 2,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("retry-exhaustion-routine"),
                source_step: Slug::derive(step1_id.to_string()),
                target_step: Slug::derive(gate_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-exhaustion-routine"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(done_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-exhaustion-routine"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(step1_id.to_string()),
                condition: RoutineEdgeCondition::OnFail,
                metadata: serde_json::json!({ "max_attempts": 1 }),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            route_response(
                "Implementation complete.",
                "pass",
                "Implementation succeeded",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "verify this implementation"}
                }]),
            ),
            route_response(
                "Needs changes.",
                "fail",
                "Criteria were not satisfied",
                serde_json::json!([{
                    "target_step": step1_id.to_string(),
                    "handoff": {"work": "revise the implementation"}
                }]),
            ),
            route_response(
                "Implementation revised.",
                "pass",
                "Implementation succeeded",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "verify this revision"}
                }]),
            ),
            route_response(
                "Still needs changes.",
                "fail",
                "Criteria were not satisfied",
                serde_json::json!([{
                    "target_step": step1_id.to_string(),
                    "handoff": {"work": "revise the implementation again"}
                }]),
            ),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let result = provider
        .routine("retry-exhaustion-routine")
        .unwrap()
        .run(test_task(Uuid::new_v4(), "Task", "Build feature"))
        .await
        .unwrap();

    assert!(!result.passed);
    assert_eq!(result.step_slug, Slug::derive(gate_id.to_string()));
    assert_eq!(result.step_name, "verify");
    assert!(
        result.output.contains("exhausted after 1 attempts"),
        "unexpected output: {}",
        result.output
    );
    assert_eq!(result.data["reason"], "retry_exhausted");
}

#[tokio::test]
async fn gate_execution_error_does_not_activate_on_fail_route() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let implement_id = Uuid::new_v4();
    let gate_id = Uuid::new_v4();
    let done_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "gate-provider-error-stops".into(),
        slug: Slug::derive("gate-provider-error-stops"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(implement_id.to_string()),
                routine: Slug::derive("gate-provider-error-stops"),
                name: "implement".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(gate_id.to_string()),
                routine: Slug::derive("gate-provider-error-stops"),
                name: "review".into(),
                step_type: RoutineStepType::Gate,
                council: None,
                agent: Some(Slug::derive("coder")),
                config: serde_json::json!({ "instructions": "Review implementation quality." }),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive(done_id.to_string()),
                routine: Slug::derive("gate-provider-error-stops"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 2,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("gate-provider-error-stops"),
                source_step: Slug::derive(implement_id.to_string()),
                target_step: Slug::derive(gate_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("gate-provider-error-stops"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(implement_id.to_string()),
                condition: RoutineEdgeCondition::OnFail,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("gate-provider-error-stops"),
                source_step: Slug::derive(gate_id.to_string()),
                target_step: Slug::derive(done_id.to_string()),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider_error = "All providers/models failed. Attempts:\nopenrouter/minimax/minimax-m2.7 streaming attempt 1/3: Provider returned error";
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(FallibleSequentialResponseMockFactory::new(vec![
            Ok(route_response(
                "Implementation complete.",
                "pass",
                "Implementation succeeded",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "review this implementation"}
                }]),
            )),
            Err(provider_error.into()),
            Ok(route_response(
                "This should not run.",
                "pass",
                "Unexpected retry",
                serde_json::json!([{
                    "target_step": gate_id.to_string(),
                    "handoff": {"work": "unexpected"}
                }]),
            )),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let mut handle = provider
        .routine("gate-provider-error-stops")
        .unwrap()
        .run_stream(test_task(Uuid::new_v4(), "Task", "Build feature"))
        .await
        .unwrap();

    let mut step_names = Vec::new();
    let mut saw_gate_step_failed = false;
    while let Some(event) = handle.recv().await {
        match event {
            RoutineEvent::StepStarted { step_name, .. } => step_names.push(step_name),
            RoutineEvent::StepFailed {
                step_name, error, ..
            } if step_name == "review" && error.contains("All providers/models failed") => {
                saw_gate_step_failed = true;
            }
            _ => {}
        }
    }

    let result = handle.output().await.unwrap();

    assert_eq!(step_names, vec!["implement", "review"]);
    assert!(saw_gate_step_failed);
    assert!(!result.passed);
    assert_eq!(result.step_slug, Slug::derive(gate_id.to_string()));
    assert_eq!(result.step_name, "review");
    assert!(result.output.contains("All providers/models failed"));
    assert_eq!(result.data["reason"], "execution_error");
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

    let err = provider.routine(Uuid::new_v4().to_string());

    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("not found"));
}

/// Terminal fail step produces a failed result.
#[tokio::test]
async fn terminal_fail_step() {
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "fail-routine".into(),
        slug: Slug::derive("fail-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("fail-routine"),
            name: "abort".into(),
            step_type: RoutineStepType::TerminalFail,
            council: None,
            agent: None,
            config: serde_json::json!({ "reason": "Blocked by policy." }),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        routines: vec![canonical_routine(routine)],
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
        .routine("fail-routine")
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
    let _council_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "council-routine".into(),
        slug: Slug::derive("council-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("council-routine"),
            name: "council-step".into(),
            step_type: RoutineStepType::Council,
            council: Some(Slug::derive("test-council")),
            agent: None,
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
        routines: vec![canonical_routine(routine)],
        councils: vec![CouncilManifest {
            name: "test-council".into(),
            delegation_strategy: CouncilDelegationStrategy::Decompose,
            leader_agent: Slug::derive("leader"),
            members: vec![CouncilMemberManifest {
                agent: Slug::derive("member"),
                priority: 1,
            }],
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            plain_response("1. Do the thing"),
            plain_response("Did the thing"),
            plain_response("Council synthesis complete."),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Council task", "Build the feature");
    let result = provider
        .routine("council-routine")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    // Council always returns a result (even with mock LLM)
    assert!(!result.output.is_empty());
}

#[tokio::test]
async fn council_broadcast() {
    let model_id = Uuid::new_v4();
    let leader_id = Uuid::new_v4();
    let member_a_id = Uuid::new_v4();
    let member_b_id = Uuid::new_v4();
    let _council_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "broadcast-council-routine".into(),
        slug: Slug::derive("broadcast-council-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("broadcast-council-routine"),
            name: "broadcast-council-step".into(),
            step_type: RoutineStepType::Council,
            council: Some(Slug::derive("broadcast-council")),
            agent: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![
            agent(leader_id, "leader", model_id),
            agent(member_a_id, "member-a", model_id),
            agent(member_b_id, "member-b", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        councils: vec![CouncilManifest {
            name: "broadcast-council".into(),
            delegation_strategy: CouncilDelegationStrategy::Broadcast,
            leader_agent: Slug::derive("leader"),
            members: vec![
                CouncilMemberManifest {
                    agent: Slug::derive("member-a"),
                    priority: 1,
                },
                CouncilMemberManifest {
                    agent: Slug::derive("member-b"),
                    priority: 2,
                },
            ],
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            plain_response("Member A independent assessment"),
            plain_response("Member B independent assessment"),
            plain_response("Broadcast consensus complete."),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Council task", "Build the feature");
    let result = provider
        .routine("broadcast-council-routine")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "Broadcast consensus complete.");
}

#[tokio::test]
async fn council_round_robin() {
    let model_id = Uuid::new_v4();
    let leader_id = Uuid::new_v4();
    let member_a_id = Uuid::new_v4();
    let member_b_id = Uuid::new_v4();
    let _council_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "round-robin-council-routine".into(),
        slug: Slug::derive("round-robin-council-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("round-robin-council-routine"),
            name: "round-robin-council-step".into(),
            step_type: RoutineStepType::Council,
            council: Some(Slug::derive("round-robin-council")),
            agent: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![
            agent(leader_id, "leader", model_id),
            agent(member_a_id, "member-a", model_id),
            agent(member_b_id, "member-b", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        councils: vec![CouncilManifest {
            name: "round-robin-council".into(),
            delegation_strategy: CouncilDelegationStrategy::RoundRobin,
            leader_agent: Slug::derive("leader"),
            members: vec![
                CouncilMemberManifest {
                    agent: Slug::derive("member-a"),
                    priority: 1,
                },
                CouncilMemberManifest {
                    agent: Slug::derive("member-b"),
                    priority: 2,
                },
            ],
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            plain_response("Contribution one"),
            plain_response("Contribution two building on prior context"),
            plain_response("Round robin synthesis complete."),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Council task", "Build the feature");
    let result = provider
        .routine("round-robin-council-routine")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "Round robin synthesis complete.");
}

#[tokio::test]
async fn council_vote() {
    let model_id = Uuid::new_v4();
    let leader_id = Uuid::new_v4();
    let member_a_id = Uuid::new_v4();
    let member_b_id = Uuid::new_v4();
    let _council_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "vote-council-routine".into(),
        slug: Slug::derive("vote-council-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("vote-council-routine"),
            name: "vote-council-step".into(),
            step_type: RoutineStepType::Council,
            council: Some(Slug::derive("vote-council")),
            agent: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![
            agent(leader_id, "leader", model_id),
            agent(member_a_id, "member-a", model_id),
            agent(member_b_id, "member-b", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        councils: vec![CouncilManifest {
            name: "vote-council".into(),
            delegation_strategy: CouncilDelegationStrategy::Vote,
            leader_agent: Slug::derive("leader"),
            members: vec![
                CouncilMemberManifest {
                    agent: Slug::derive("member-a"),
                    priority: 1,
                },
                CouncilMemberManifest {
                    agent: Slug::derive("member-b"),
                    priority: 2,
                },
            ],
        }],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            plain_response("Vote: pass. Reason: solution is sound."),
            plain_response("Vote: pass. Reason: acceptable tradeoffs."),
            plain_response("Vote tallied and accepted."),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = test_task(Uuid::new_v4(), "Council task", "Build the feature");
    let result = provider
        .routine("vote-council-routine")
        .unwrap()
        .run(task)
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(result.output, "Vote tallied and accepted.");
}

/// Cron execution: runs one scheduled routine firing.
#[tokio::test]
async fn scheduled_cron_routine_execution() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "cron-routine".into(),
        slug: Slug::derive("cron-routine"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Cron,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("cron-routine"),
            name: "check".into(),
            step_type: RoutineStepType::Gate,
            council: None,
            agent: Some(Slug::derive("monitor")),
            config: serde_json::json!({ "instructions": "Check system health" }),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "monitor", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    // Mock returns a route_next_steps tool call for the gate step. The cron
    // wrapper should complete this scheduled firing after the DAG run.
    let route_response = route_response(
        "Evaluation complete.",
        "pass",
        "All checks passed",
        serde_json::json!([{
            "target_step": implicit_done_target(),
            "handoff": {"work": "record healthy status"}
        }]),
    );
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![route_response]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = RoutineRun::cron(CronInput {
        task: None,
        project: Some(nenjo::Slug::derive("project")),
        schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_millis(50)),
        start_at: None,
        timeout: Duration::from_secs(5),
    });

    let mut handle = provider
        .routine("cron-routine")
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
    assert!(
        result.passed,
        "cron routine should pass with route_next_steps"
    );
    assert_eq!(
        result.data.get("verdict").and_then(|v| v.as_str()),
        Some("pass"),
        "should have structured verdict data"
    );
}

/// Cron execution: agent route_next_steps routes to following terminal step.
#[tokio::test]
async fn cron_agent_route_next_steps_continues_to_terminal_step() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let agent_step_id = Uuid::new_v4();
    let terminal_step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "cron-agent-terminal".into(),
        slug: Slug::derive("cron-agent-terminal"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Cron,
        metadata: RoutineMetadata::default(),
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive(agent_step_id.to_string()),
                routine: Slug::derive("cron-agent-terminal"),
                name: "inspect".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("monitor")),
                config: serde_json::json!({ "description": "Inspect workspace files" }),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive(terminal_step_id.to_string()),
                routine: Slug::derive("cron-agent-terminal"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 1,
            },
        ],
        edges: vec![RoutineEdgeManifest {
            routine: Slug::derive("cron-agent-terminal"),
            source_step: Slug::derive(agent_step_id.to_string()),
            target_step: Slug::derive(terminal_step_id.to_string()),
            condition: RoutineEdgeCondition::OnPass,
            metadata: serde_json::json!({}),
        }],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "monitor", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let route_response = route_response(
        "Found files",
        "pass",
        "Workspace inspected",
        serde_json::json!([{
            "target_step": terminal_step_id.to_string(),
            "handoff": {"work": "finish"}
        }]),
    );
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![route_response]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = RoutineRun::cron(CronInput {
        task: None,
        project: None,
        schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_millis(50)),
        start_at: None,
        timeout: Duration::from_secs(5),
    });

    let mut handle = provider
        .routine("cron-agent-terminal")
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    let mut cycles_started = 0u32;
    let mut cycles_completed = 0u32;
    let mut step_names = Vec::new();

    while let Some(event) = handle.recv().await {
        match event {
            RoutineEvent::CronCycleStarted { .. } => cycles_started += 1,
            RoutineEvent::CronCycleCompleted { .. } => cycles_completed += 1,
            RoutineEvent::StepStarted { step_name, .. } => step_names.push(step_name),
            _ => {}
        }
    }

    assert_eq!(cycles_started, 1, "should run one scheduled firing");
    assert_eq!(cycles_completed, 1, "should complete one cron cycle");
    assert_eq!(
        step_names,
        vec!["inspect", "done"],
        "route_next_steps should allow the next routine step to run"
    );

    let result = handle.output().await.unwrap();
    assert!(result.passed);
    assert_eq!(result.output, "Found files");
}

/// Cron cancellation: cancel the handle mid-execution and verify it stops.
#[tokio::test]
async fn cron_cancellation() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let step_id = Uuid::new_v4();
    let _routine_id = Uuid::new_v4();
    let routine = RoutineManifest {
        name: "cancel-cron".into(),
        slug: Slug::derive("cancel-cron"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Cron,
        metadata: RoutineMetadata::default(),
        steps: vec![RoutineStepManifest {
            slug: Slug::derive(step_id.to_string()),
            routine: Slug::derive("cancel-cron"),
            name: "poll".into(),
            step_type: RoutineStepType::Terminal,
            council: None,
            agent: None,
            config: serde_json::json!({}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "poller", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockFactory::new("wait"))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let task = RoutineRun::cron(CronInput {
        task: None,
        project: Some(nenjo::Slug::derive("project")),
        schedule: nenjo::routines::types::CronSchedule::Interval(Duration::from_millis(50)),
        start_at: None,
        timeout: Duration::from_secs(30),
    });

    let mut handle = provider
        .routine("cancel-cron")
        .unwrap()
        .run_stream(task)
        .await
        .unwrap();

    // Wait for the scheduled firing to complete, then cancel. The firing may
    // already be complete, because cron execution runs one DAG cycle.
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

    let result = handle.output().await.unwrap();
    assert!(result.passed);
}

#[tokio::test]
async fn agent_step_receives_route_next_steps_tool() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "route-tool-check".into(),
        slug: Slug::derive("route-tool-check"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata {
            schedule: None,
            entry_steps: vec![Slug::derive("work")],
            cron_task: None,
        },
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive("work"),
                routine: Slug::derive("route-tool-check"),
                name: "work".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("worker")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive("done"),
                routine: Slug::derive("route-tool-check"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 1,
            },
        ],
        edges: vec![RoutineEdgeManifest {
            routine: Slug::derive("route-tool-check"),
            source_step: Slug::derive("work"),
            target_step: Slug::derive("done"),
            condition: RoutineEdgeCondition::Always,
            metadata: serde_json::json!({"purpose": "finish the routine"}),
        }],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "worker", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingToolsMockFactory::new(route_response(
        "work complete",
        "pass",
        "ready",
        serde_json::json!([{"target_step": "done", "handoff": {"work": "finish"}}]),
    ));
    let seen_tools = factory.seen_tools();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    provider
        .routine("route-tool-check")
        .unwrap()
        .run(test_task(Uuid::new_v4(), "Task", "Do work"))
        .await
        .unwrap();

    let seen_tools = seen_tools.lock().unwrap();
    let first_tools = seen_tools.first().expect("model should receive tool specs");
    assert!(first_tools.iter().any(|name| name == "route_next_steps"));
}

#[tokio::test]
async fn gate_step_receives_route_next_steps_tool() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "gate-route-tool-check".into(),
        slug: Slug::derive("gate-route-tool-check"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata {
            schedule: None,
            entry_steps: vec![Slug::derive("review")],
            cron_task: None,
        },
        steps: vec![RoutineStepManifest {
            slug: Slug::derive("review"),
            routine: Slug::derive("gate-route-tool-check"),
            name: "review".into(),
            step_type: RoutineStepType::Gate,
            council: None,
            agent: Some(Slug::derive("reviewer")),
            config: serde_json::json!({"instructions": "Accept completed work."}),
            order_index: 0,
        }],
        edges: vec![],
    };

    let manifest = Manifest {
        agents: vec![agent(agent_id, "reviewer", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };
    let factory = RecordingToolsMockFactory::new(route_response(
        "accepted",
        "pass",
        "meets criteria",
        serde_json::json!([{
            "target_step": implicit_done_target(),
            "handoff": {"work": "finish"}
        }]),
    ));
    let seen_tools = factory.seen_tools();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    provider
        .routine("gate-route-tool-check")
        .unwrap()
        .run(test_task(Uuid::new_v4(), "Task", "Review work"))
        .await
        .unwrap();

    let seen_tools = seen_tools.lock().unwrap();
    let first_tools = seen_tools.first().expect("model should receive tool specs");
    assert!(first_tools.iter().any(|name| name == "route_next_steps"));
}

#[tokio::test]
async fn fan_out_and_fan_in_waits_for_all_upstream_steps() {
    let model_id = Uuid::new_v4();
    let start_agent_id = Uuid::new_v4();
    let left_agent_id = Uuid::new_v4();
    let right_agent_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "fanout-fanin".into(),
        slug: Slug::derive("fanout-fanin"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata {
            schedule: None,
            entry_steps: vec![Slug::derive("start")],
            cron_task: None,
        },
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive("start"),
                routine: Slug::derive("fanout-fanin"),
                name: "start".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("start-agent")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive("left"),
                routine: Slug::derive("fanout-fanin"),
                name: "left".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("left-agent")),
                config: serde_json::json!({}),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive("right"),
                routine: Slug::derive("fanout-fanin"),
                name: "right".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("right-agent")),
                config: serde_json::json!({}),
                order_index: 2,
            },
            RoutineStepManifest {
                slug: Slug::derive("done"),
                routine: Slug::derive("fanout-fanin"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 3,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("fanout-fanin"),
                source_step: Slug::derive("start"),
                target_step: Slug::derive("left"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({"purpose": "left branch"}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("fanout-fanin"),
                source_step: Slug::derive("start"),
                target_step: Slug::derive("right"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({"purpose": "right branch"}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("fanout-fanin"),
                source_step: Slug::derive("left"),
                target_step: Slug::derive("done"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("fanout-fanin"),
                source_step: Slug::derive("right"),
                target_step: Slug::derive("done"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![
            agent(start_agent_id, "start-agent", model_id),
            agent(left_agent_id, "left-agent", model_id),
            agent(right_agent_id, "right-agent", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            route_response(
                "start complete",
                "pass",
                "fan out",
                serde_json::json!([
                    {"target_step": "left", "handoff": {"work": "left task"}},
                    {"target_step": "right", "handoff": {"work": "right task"}}
                ]),
            ),
            route_response(
                "branch complete",
                "pass",
                "branch done",
                serde_json::json!([{"target_step": "done", "handoff": {"work": "finish"}}]),
            ),
            route_response(
                "branch complete",
                "pass",
                "branch done",
                serde_json::json!([{"target_step": "done", "handoff": {"work": "finish"}}]),
            ),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let mut handle = provider
        .routine("fanout-fanin")
        .unwrap()
        .run_stream(test_task(Uuid::new_v4(), "Task", "Do branches"))
        .await
        .unwrap();

    let mut step_names = Vec::new();
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::StepStarted { step_name, .. } = event {
            step_names.push(step_name);
        }
    }

    assert_eq!(step_names.first().map(String::as_str), Some("start"));
    assert_eq!(step_names.last().map(String::as_str), Some("done"));
    assert!(step_names.contains(&"left".to_string()));
    assert!(step_names.contains(&"right".to_string()));

    let result = handle.output().await.unwrap();
    assert!(result.passed);
}

#[tokio::test]
async fn fan_in_agent_receives_all_upstream_handoffs() {
    let model_id = Uuid::new_v4();
    let left_agent_id = Uuid::new_v4();
    let right_agent_id = Uuid::new_v4();
    let synth_agent_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "handoff-join".into(),
        slug: Slug::derive("handoff-join"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata {
            schedule: None,
            entry_steps: vec![Slug::derive("left"), Slug::derive("right")],
            cron_task: None,
        },
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive("left"),
                routine: Slug::derive("handoff-join"),
                name: "left".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("left-agent")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive("right"),
                routine: Slug::derive("handoff-join"),
                name: "right".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("right-agent")),
                config: serde_json::json!({}),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive("synthesize"),
                routine: Slug::derive("handoff-join"),
                name: "synthesize".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("synth-agent")),
                config: serde_json::json!({}),
                order_index: 2,
            },
            RoutineStepManifest {
                slug: Slug::derive("done"),
                routine: Slug::derive("handoff-join"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 3,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("handoff-join"),
                source_step: Slug::derive("left"),
                target_step: Slug::derive("synthesize"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({
                    "purpose": "Provide left branch evidence.",
                    "handoff_instructions": "Hand off left evidence only."
                }),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("handoff-join"),
                source_step: Slug::derive("right"),
                target_step: Slug::derive("synthesize"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({
                    "purpose": "Provide right branch evidence.",
                    "handoff_instructions": "Hand off right evidence only."
                }),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("handoff-join"),
                source_step: Slug::derive("synthesize"),
                target_step: Slug::derive("done"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let mut synth_agent = agent(synth_agent_id, "synth-agent", model_id);
    synth_agent.prompt_config.templates.task_execution =
        "{{ routine }}\n\nExecute: {{ task.title }}\n{{ task.description }}".into();

    let manifest = Manifest {
        agents: vec![
            agent(left_agent_id, "left-agent", model_id),
            agent(right_agent_id, "right-agent", model_id),
            synth_agent,
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let factory = SequentialResponseMockFactory::with_message_recording(vec![
        route_response(
            "left complete",
            "pass",
            "left ready",
            serde_json::json!([{
                "target_step": "synthesize",
                "handoff": {"work": "LEFT_HANDOFF_CONTENT"},
                "summary": "left handoff"
            }]),
        ),
        route_response(
            "right complete",
            "pass",
            "right ready",
            serde_json::json!([{
                "target_step": "synthesize",
                "handoff": {"work": "RIGHT_HANDOFF_CONTENT"},
                "summary": "right handoff"
            }]),
        ),
        route_response(
            "synthesis complete",
            "pass",
            "done",
            serde_json::json!([{"target_step": "done", "handoff": {"work": "final"}}]),
        ),
    ]);
    let seen_messages = factory.seen_messages().unwrap();

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let mut handle = provider
        .routine("handoff-join")
        .unwrap()
        .run_stream(test_task(Uuid::new_v4(), "Task", "Do branches"))
        .await
        .unwrap();

    let mut event_log = Vec::new();
    while let Some(event) = handle.recv().await {
        match event {
            RoutineEvent::StepStarted { step_slug, .. } => {
                event_log.push(format!("started:{step_slug}"));
            }
            RoutineEvent::StepCompleted { step_slug, .. } => {
                event_log.push(format!("completed:{step_slug}"));
            }
            _ => {}
        }
    }
    let result = handle.output().await.unwrap();
    assert!(result.passed);

    let synth_started = event_log
        .iter()
        .position(|event| event == "started:synthesize")
        .expect("synthesize should start");
    let left_completed = event_log
        .iter()
        .position(|event| event == "completed:left")
        .expect("left should complete");
    let right_completed = event_log
        .iter()
        .position(|event| event == "completed:right")
        .expect("right should complete");
    assert!(
        synth_started > left_completed && synth_started > right_completed,
        "joined synthesize step started before all upstream steps completed: {event_log:#?}"
    );

    let seen_messages = seen_messages.lock().unwrap();
    assert!(
        messages_contain(&seen_messages, "<handoffs>"),
        "joined agent should receive routine.handoffs. Messages: {seen_messages:#?}"
    );
    assert!(
        messages_contain(&seen_messages, "source_step=\"left\"")
            && messages_contain(&seen_messages, "source_step=\"right\""),
        "joined agent should receive one structured handoff from each upstream step. Messages: {seen_messages:#?}"
    );
    assert!(
        messages_contain(&seen_messages, "LEFT_HANDOFF_CONTENT")
            && messages_contain(&seen_messages, "RIGHT_HANDOFF_CONTENT"),
        "joined agent should receive both upstream handoffs. Messages: {seen_messages:#?}"
    );
    assert!(
        !messages_contain(&seen_messages, "&lt;handoffs&gt;"),
        "joined agent should not receive handoffs as escaped XML. Messages: {seen_messages:#?}"
    );
    assert!(
        !messages_contain(&seen_messages, "Hand off left evidence only.")
            && !messages_contain(&seen_messages, "Hand off right evidence only."),
        "joined agent should receive handoff content, not source-step handoff instructions. Messages: {seen_messages:#?}"
    );
}

#[tokio::test]
async fn gate_on_fail_retry_edge_does_not_block_first_downstream_pass() {
    let model_id = Uuid::new_v4();
    let planner_id = Uuid::new_v4();
    let implementer_id = Uuid::new_v4();
    let reviewer_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "retry-loop-first-pass".into(),
        slug: Slug::derive("retry-loop-first-pass"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata {
            schedule: None,
            entry_steps: vec![Slug::derive("plan")],
            cron_task: None,
        },
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive("plan"),
                routine: Slug::derive("retry-loop-first-pass"),
                name: "plan".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("planner")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive("implement"),
                routine: Slug::derive("retry-loop-first-pass"),
                name: "implement".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("implementer")),
                config: serde_json::json!({}),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive("review"),
                routine: Slug::derive("retry-loop-first-pass"),
                name: "review".into(),
                step_type: RoutineStepType::Gate,
                council: None,
                agent: Some(Slug::derive("reviewer")),
                config: serde_json::json!({}),
                order_index: 2,
            },
            RoutineStepManifest {
                slug: Slug::derive("done"),
                routine: Slug::derive("retry-loop-first-pass"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 3,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("retry-loop-first-pass"),
                source_step: Slug::derive("plan"),
                target_step: Slug::derive("implement"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({"purpose": "handoff plan"}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-loop-first-pass"),
                source_step: Slug::derive("implement"),
                target_step: Slug::derive("review"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({"purpose": "review implementation"}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-loop-first-pass"),
                source_step: Slug::derive("review"),
                target_step: Slug::derive("done"),
                condition: RoutineEdgeCondition::OnPass,
                metadata: serde_json::json!({"purpose": "finish"}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("retry-loop-first-pass"),
                source_step: Slug::derive("review"),
                target_step: Slug::derive("implement"),
                condition: RoutineEdgeCondition::OnFail,
                metadata: serde_json::json!({"purpose": "revise implementation"}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![
            agent(planner_id, "planner", model_id),
            agent(implementer_id, "implementer", model_id),
            agent(reviewer_id, "reviewer", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![
            route_response(
                "plan complete",
                "pass",
                "ready to implement",
                serde_json::json!([{"target_step": "implement", "handoff": {"work": "build it"}}]),
            ),
            route_response(
                "implementation complete",
                "pass",
                "ready for review",
                serde_json::json!([{"target_step": "review", "handoff": {"work": "review it"}}]),
            ),
            route_response(
                "review passed",
                "pass",
                "ship it",
                serde_json::json!([{"target_step": "done", "handoff": {"work": "complete"}}]),
            ),
        ]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let mut handle = provider
        .routine("retry-loop-first-pass")
        .unwrap()
        .run_stream(test_task(Uuid::new_v4(), "Task", "Build feature"))
        .await
        .unwrap();

    let mut step_names = Vec::new();
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::StepStarted { step_name, .. } = event {
            step_names.push(step_name);
        }
    }

    let result = handle.output().await.unwrap();
    assert!(result.passed);
    assert_eq!(step_names, vec!["plan", "implement", "review", "done"]);
}

#[tokio::test]
async fn route_next_steps_fail_verdict_stops_routine() {
    let model_id = Uuid::new_v4();
    let first_agent_id = Uuid::new_v4();
    let second_agent_id = Uuid::new_v4();

    let routine = RoutineManifest {
        name: "route-fail-stops".into(),
        slug: Slug::derive("route-fail-stops"),
        description: None,
        trigger: nenjo::manifest::RoutineTrigger::Task,
        metadata: RoutineMetadata {
            schedule: None,
            entry_steps: vec![Slug::derive("first")],
            cron_task: None,
        },
        steps: vec![
            RoutineStepManifest {
                slug: Slug::derive("first"),
                routine: Slug::derive("route-fail-stops"),
                name: "first".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("first-agent")),
                config: serde_json::json!({}),
                order_index: 0,
            },
            RoutineStepManifest {
                slug: Slug::derive("second"),
                routine: Slug::derive("route-fail-stops"),
                name: "second".into(),
                step_type: RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive("second-agent")),
                config: serde_json::json!({}),
                order_index: 1,
            },
            RoutineStepManifest {
                slug: Slug::derive("done"),
                routine: Slug::derive("route-fail-stops"),
                name: "done".into(),
                step_type: RoutineStepType::Terminal,
                council: None,
                agent: None,
                config: serde_json::json!({}),
                order_index: 2,
            },
        ],
        edges: vec![
            RoutineEdgeManifest {
                routine: Slug::derive("route-fail-stops"),
                source_step: Slug::derive("first"),
                target_step: Slug::derive("second"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
            RoutineEdgeManifest {
                routine: Slug::derive("route-fail-stops"),
                source_step: Slug::derive("second"),
                target_step: Slug::derive("done"),
                condition: RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            },
        ],
    };

    let manifest = Manifest {
        agents: vec![
            agent(first_agent_id, "first-agent", model_id),
            agent(second_agent_id, "second-agent", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        routines: vec![canonical_routine(routine)],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(SequentialResponseMockFactory::new(vec![route_response(
            "blocked",
            "fail",
            "cannot continue",
            serde_json::Value::Null,
        )]))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let mut handle = provider
        .routine("route-fail-stops")
        .unwrap()
        .run_stream(test_task(Uuid::new_v4(), "Task", "Do work"))
        .await
        .unwrap();

    let mut step_names = Vec::new();
    while let Some(event) = handle.recv().await {
        if let RoutineEvent::StepStarted { step_name, .. } = event {
            step_names.push(step_name);
        }
    }

    assert_eq!(step_names, vec!["first"]);
    let result = handle.output().await.unwrap();
    assert!(!result.passed);
    assert_eq!(result.output, "blocked");
    assert_eq!(
        result.data.get("verdict").and_then(|value| value.as_str()),
        Some("fail")
    );
}

// ===========================================================================
// Sub-agent tool injection tests
// ===========================================================================

/// Parent agents expose installed-agent delegation tools when orchestration is enabled.
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

    // Ephemeral sub-agent tools are injected into the per-run clone, but
    // installed-agent delegation tools are part of the parent runner surface.
    let runner = provider
        .agent("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(tool_names.contains(&"list_delegatable_agents"));
    assert!(tool_names.contains(&"delegate_to"));
    assert!(!tool_names.contains(&"spawn_sub_agents"));
    assert!(!tool_names.contains(&"send_sub_agents"));
    assert!(!tool_names.contains(&"inspect_sub_agents"));
}

/// Single-agent manifests still expose delegation discovery; the tool reports no targets at execution.
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

    let runner = provider.agent("solo").await.unwrap().build().await.unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(tool_names.contains(&"list_delegatable_agents"));
    assert!(tool_names.contains(&"delegate_to"));
    assert!(!tool_names.contains(&"spawn_sub_agents"));
}

#[tokio::test]
async fn worktree_scoped_agent_keeps_extra_runtime_tools() {
    let model_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();

    let manifest = Manifest {
        agents: vec![agent(agent_id, "worker", model_id)],
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

    let work_dir = tempfile::tempdir().unwrap();
    let runner = provider
        .agent("worker")
        .await
        .unwrap()
        .with_tool(ProgressTool)
        .with_work_dir(work_dir.path())
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let tool_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(
        tool_names.contains(&"progress_tool"),
        "extra runtime tool should survive worktree tool rebuild. Tools: {:?}",
        tool_names
    );
}
