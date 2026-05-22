//! Integration tests for the native sub-agent runtime.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::Result;
use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, PromptTemplates,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider, ToolFactory};
use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use nenjo_tool_api::{Tool, ToolCall, ToolCategory, ToolResult};
use tokio::sync::Notify;
use uuid::Uuid;

fn model(id: Uuid) -> ModelManifest {
    ModelManifest {
        id,
        name: "test-model".into(),
        description: None,
        model: "mock-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        base_url: None,
    }
}

fn agent(id: Uuid, name: &str, model_id: Uuid) -> AgentManifest {
    AgentManifest {
        id,
        name: name.into(),
        description: Some(format!("{name} agent")),
        prompt_config: PromptConfig {
            system_prompt: format!("You are the {name} agent."),
            templates: PromptTemplates {
                task_execution: "Execute: {{ task.title }}".into(),
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

fn project() -> ProjectManifest {
    ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        settings: serde_json::Value::Null,
    }
}

#[derive(Clone, Default)]
struct CapturedRequests {
    tool_names: Arc<Mutex<Vec<Vec<String>>>>,
    messages: Arc<Mutex<Vec<Vec<String>>>>,
}

impl CapturedRequests {
    fn tool_names(&self) -> Vec<Vec<String>> {
        self.tool_names.lock().unwrap().clone()
    }

    fn messages(&self) -> Vec<Vec<String>> {
        self.messages.lock().unwrap().clone()
    }
}

struct FixedLlm {
    response: String,
    captured: CapturedRequests,
}

#[async_trait::async_trait]
impl ModelProvider for FixedLlm {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        self.captured.tool_names.lock().unwrap().push(
            request
                .tools
                .unwrap_or_default()
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
        );
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

struct FixedFactory {
    response: String,
    captured: CapturedRequests,
}

impl FixedFactory {
    fn new(response: &str, captured: CapturedRequests) -> Self {
        Self {
            response: response.into(),
            captured,
        }
    }
}

impl ModelProviderFactory for FixedFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(FixedLlm {
            response: self.response.clone(),
            captured: self.captured.clone(),
        }))
    }
}

struct SubAgentScriptLlm {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
    captured: CapturedRequests,
}

#[async_trait::async_trait]
impl ModelProvider for SubAgentScriptLlm {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let tool_names: Vec<String> = request
            .tools
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        self.captured
            .tool_names
            .lock()
            .unwrap()
            .push(tool_names.clone());
        self.captured.messages.lock().unwrap().push(
            request
                .messages
                .iter()
                .map(|message| message.content.clone())
                .collect(),
        );

        if tool_names.iter().any(|name| name == "update_parent_agent") {
            let call = self.child_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(if call == 0 {
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "child-update".into(),
                        name: "update_parent_agent".into(),
                        arguments: serde_json::json!({
                            "summary": "Read auth/session code.",
                            "details": "No blocker yet."
                        })
                        .to_string(),
                    }],
                    usage: TokenUsage::default(),
                }
            } else {
                ChatResponse {
                    text: Some(
                        r#"{"summary":"Security review complete.","issues":[],"confidence":"high"}"#
                            .into(),
                    ),
                    tool_calls: vec![],
                    usage: TokenUsage::default(),
                }
            });
        }

        let call = self.parent_calls.fetch_add(1, Ordering::SeqCst);
        Ok(match call {
            0 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "spawn".into(),
                    name: "spawn_sub_agents".into(),
                    arguments: serde_json::json!({
                        "agents": [{
                            "agent": "reviewer",
                            "slug": "security_review",
                            "prompt": "Act as a focused security review worker. Be concise.",
                            "task": {
                                "description": "Review auth/session changes.",
                                "goal": "Identify security issues in the changed auth/session behavior.",
                                "acceptance_criteria": [
                                    "Check for privilege escalation risk.",
                                    "Return a structured result with confidence."
                                ]
                            },
                            "context": {"files": ["crates/auth/src/session.rs"]},
                            "result_format": {
                                "fields": [
                                    {"name": "summary", "type": "string"},
                                    {"name": "issues", "type": "list"},
                                    {"name": "confidence", "type": "string"}
                                ]
                            }
                        }]
                    })
                    .to_string(),
                }],
                usage: TokenUsage::default(),
            },
            1 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "wait".into(),
                    name: "wait".into(),
                    arguments: serde_json::json!({"seconds": 1}).to_string(),
                }],
                usage: TokenUsage::default(),
            },
            _ => ChatResponse {
                text: Some("parent complete".into()),
                tool_calls: vec![],
                usage: TokenUsage::default(),
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

struct SubAgentScriptFactory {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
    captured: CapturedRequests,
}

impl SubAgentScriptFactory {
    fn new(captured: CapturedRequests) -> Self {
        Self {
            parent_calls: Arc::new(AtomicUsize::new(0)),
            child_calls: Arc::new(AtomicUsize::new(0)),
            captured,
        }
    }
}

impl ModelProviderFactory for SubAgentScriptFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(SubAgentScriptLlm {
            parent_calls: self.parent_calls.clone(),
            child_calls: self.child_calls.clone(),
            captured: self.captured.clone(),
        }))
    }
}

struct DropFlag {
    dropped: Arc<AtomicBool>,
}

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

struct AbortObservedLlm {
    parent_calls: Arc<AtomicUsize>,
    child_started: Arc<Notify>,
    child_dropped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl ModelProvider for AbortObservedLlm {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let tool_names: Vec<String> = request
            .tools
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        if tool_names.iter().any(|name| name == "update_parent_agent") {
            let _guard = DropFlag {
                dropped: self.child_dropped.clone(),
            };
            self.child_started.notify_waiters();
            std::future::pending::<Result<ChatResponse>>().await
        } else {
            let call = self.parent_calls.fetch_add(1, Ordering::SeqCst);
            Ok(if call == 0 {
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "spawn".into(),
                        name: "spawn_sub_agents".into(),
                        arguments: serde_json::json!({
                            "agents": [{
                                "agent": "reviewer",
                                "slug": "blocked_review",
                                "task": {
                                    "description": "Wait until cancelled.",
                                    "goal": "Exercise cancellation.",
                                    "acceptance_criteria": ["Start child execution."]
                                }
                            }]
                        })
                        .to_string(),
                    }],
                    usage: TokenUsage::default(),
                }
            } else {
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "wait".into(),
                        name: "wait".into(),
                        arguments: serde_json::json!({"seconds": 30}).to_string(),
                    }],
                    usage: TokenUsage::default(),
                }
            })
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

struct AbortObservedFactory {
    parent_calls: Arc<AtomicUsize>,
    child_started: Arc<Notify>,
    child_dropped: Arc<AtomicBool>,
}

impl AbortObservedFactory {
    fn new(child_started: Arc<Notify>, child_dropped: Arc<AtomicBool>) -> Self {
        Self {
            parent_calls: Arc::new(AtomicUsize::new(0)),
            child_started,
            child_dropped,
        }
    }
}

impl ModelProviderFactory for AbortObservedFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(AbortObservedLlm {
            parent_calls: self.parent_calls.clone(),
            child_started: self.child_started.clone(),
            child_dropped: self.child_dropped.clone(),
        }))
    }
}

struct PlatformEchoTool;

#[async_trait::async_trait]
impl Tool for PlatformEchoTool {
    fn name(&self) -> &str {
        "platform_echo"
    }

    fn description(&self) -> &str {
        "A stand-in for platform/provider tools."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        })
    }
}

struct PlatformToolFactory;

#[async_trait::async_trait]
impl ToolFactory for PlatformToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(PlatformEchoTool)]
    }
}

#[tokio::test]
async fn parent_tools_are_available_during_execution() {
    let captured = CapturedRequests::default();
    let model_id = Uuid::new_v4();
    let manifest = Manifest {
        agents: vec![agent(Uuid::new_v4(), "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(FixedFactory::new("ok", captured.clone()))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent_by_name("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    assert!(
        runner
            .instance()
            .tool_specs()
            .iter()
            .all(|tool| tool.name != "delegate_to")
    );
    runner.chat("coordinate review").await.unwrap();

    let first_tools = captured.tool_names().remove(0);
    for expected in [
        "spawn_sub_agents",
        "send_sub_agents",
        "inspect_sub_agents",
        "stop_sub_agents",
        "wait",
    ] {
        assert!(
            first_tools.iter().any(|name| name == expected),
            "{first_tools:?}"
        );
    }
    assert!(!first_tools.iter().any(|name| name == "delegate_to"));
}

#[tokio::test]
async fn parent_tools_are_injected_for_ephemeral_sub_agents() {
    let captured = CapturedRequests::default();
    let model_id = Uuid::new_v4();
    let manifest = Manifest {
        agents: vec![agent(Uuid::new_v4(), "solo", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(FixedFactory::new("ok", captured.clone()))
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

    runner.chat("work").await.unwrap();
    let first_tools = captured.tool_names().remove(0);
    for expected in [
        "spawn_sub_agents",
        "send_sub_agents",
        "inspect_sub_agents",
        "stop_sub_agents",
        "wait",
    ] {
        assert!(
            first_tools.iter().any(|name| name == expected),
            "{first_tools:?}"
        );
    }
}

#[tokio::test]
async fn spawn_child_waits_and_returns_slug_based_digest() {
    let captured = CapturedRequests::default();
    let model_id = Uuid::new_v4();
    let manifest = Manifest {
        agents: vec![agent(Uuid::new_v4(), "coder", model_id)],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };
    let factory = SubAgentScriptFactory::new(captured.clone());

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(factory)
        .with_tool_factory(PlatformToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent_by_name("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let output = runner.chat("coordinate a review").await.unwrap();
    assert_eq!(output.text, "parent complete");

    let tool_results = output
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    assert!(
        tool_results
            .iter()
            .any(|content| content.contains(r#"\"slug\":\"security_review\""#)),
        "{tool_results:?}"
    );
    assert!(
        tool_results
            .iter()
            .any(|content| content.contains(r#"\"kind\":\"completed\""#)),
        "{tool_results:?}"
    );
    assert!(
        !tool_results
            .iter()
            .any(|content| content.contains("target_agent_id")),
        "{tool_results:?}"
    );

    let all_tool_names = captured.tool_names();
    assert!(
        all_tool_names
            .iter()
            .any(|names| names.iter().any(|name| name == "platform_echo")
                && names.iter().any(|name| name == "spawn_sub_agents")),
        "parent request should include provider tools and sub-agent management tools: {all_tool_names:?}"
    );
    let child_tool_sets = all_tool_names
        .iter()
        .filter(|names| names.iter().any(|name| name == "update_parent_agent"))
        .collect::<Vec<_>>();
    assert!(!child_tool_sets.is_empty(), "{all_tool_names:?}");
    for names in child_tool_sets {
        assert!(
            names.iter().any(|name| name == "ask_parent_agent"),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "platform_echo"),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "spawn_sub_agents"),
            "{names:?}"
        );
        assert!(!names.iter().any(|name| name == "wait"), "{names:?}");
    }

    let child_messages = captured
        .messages()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let child_prompt_seen = child_messages
        .iter()
        .any(|message| message.contains("Act as a focused security review worker."));
    let child_task_seen = child_messages.iter().any(|message| {
        message.contains("Return your final answer as a single JSON object")
            && message.contains("confidence")
            && message.contains("Acceptance criteria and output instructions:")
    });
    assert!(
        child_prompt_seen,
        "child prompt instructions missing: {child_messages:?}"
    );
    assert!(
        child_task_seen,
        "child task/result instructions missing: {child_messages:?}"
    );
    assert!(
        !captured
            .messages()
            .into_iter()
            .flatten()
            .any(|message| message.contains("You are the reviewer agent.")),
        "child execution should use the parent-authored task directly, not the child manifest prompt"
    );
}

#[tokio::test]
async fn sub_agent_events_stream_to_parent_observers() {
    let captured = CapturedRequests::default();
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
        .with_model_factory(SubAgentScriptFactory::new(captured))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent_by_name("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let mut handle = runner.chat_stream("coordinate a review").await.unwrap();
    let mut observed = Vec::new();
    let mut transcript_events = Vec::new();
    while let Some(event) = handle.recv().await {
        match event {
            nenjo::TurnEvent::SubAgentEvent {
                slug,
                agent_name,
                kind,
                summary,
                model_visible,
            } => observed.push((slug, agent_name, kind, summary, model_visible)),
            nenjo::TurnEvent::SubAgentTranscript {
                slug,
                agent_name,
                event,
            } => transcript_events.push((slug, agent_name, event.kind().to_string())),
            nenjo::TurnEvent::Done { .. } => break,
            _ => {}
        }
    }
    let output = handle.output().await.unwrap();
    assert_eq!(output.text, "parent complete");
    assert!(
        observed.iter().any(|(slug, agent, kind, _, visible)| {
            slug == "security_review" && agent == "reviewer" && kind == "completed" && !visible
        }),
        "{observed:?}"
    );
    assert!(
        transcript_events.iter().any(|(slug, agent, kind)| {
            slug == "security_review" && agent == "reviewer" && kind == "tool_call"
        }),
        "{transcript_events:?}"
    );
}

#[tokio::test]
async fn parent_abort_cancels_live_child_execution() {
    let child_started = Arc::new(Notify::new());
    let child_dropped = Arc::new(AtomicBool::new(false));
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
        .with_model_factory(AbortObservedFactory::new(
            child_started.clone(),
            child_dropped.clone(),
        ))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent_by_name("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let handle = runner.chat_stream("start blocked child").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), child_started.notified())
        .await
        .expect("child model call should start");
    handle.abort();
    drop(handle);

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !child_dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("aborting parent should drop/cancel child model future");
}

#[tokio::test]
async fn max_depth_zero_disables_parent_tools() {
    let captured = CapturedRequests::default();
    let model_id = Uuid::new_v4();
    let manifest = Manifest {
        agents: vec![
            agent(Uuid::new_v4(), "alpha", model_id),
            agent(Uuid::new_v4(), "beta", model_id),
        ],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let config = nenjo::AgentConfig {
        max_delegation_depth: 0,
        ..Default::default()
    };
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(FixedFactory::new("ok", captured.clone()))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent_by_name("alpha")
        .await
        .unwrap()
        .with_config(config)
        .build()
        .await
        .unwrap();

    runner.chat("work").await.unwrap();
    assert!(captured.tool_names().remove(0).is_empty());
}
