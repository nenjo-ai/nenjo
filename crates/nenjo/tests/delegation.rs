//! Integration tests for the native sub-agent runtime.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::Result;
use nenjo::Slug;
use nenjo::manifest::{
    AbilityManifest, AbilityPromptConfig, AgentManifest, Manifest, ModelManifest, ProjectManifest,
    PromptConfig, PromptTemplates, model_manifest_slug,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider, ToolFactory};
use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use nenjo_tool_api::{Tool, ToolCall, ToolCategory, ToolResult};
use tokio::sync::Notify;
use uuid::Uuid;

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
                task_execution: "Execute: {{ task.title }}".into(),
                chat_task: "{{ chat.message }}".into(),
                gate_eval: String::new(),
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

fn project() -> ProjectManifest {
    ProjectManifest {
        name: "test-project".into(),
        slug: Slug::derive("test-project"),
        description: None,
        settings: serde_json::Value::Null,
    }
}

fn ability(name: &str) -> AbilityManifest {
    AbilityManifest {
        name: name.into(),
        path: None,
        description: Some(format!("{name} ability")),
        activation_condition: format!("Use {name} when useful."),
        prompt_config: AbilityPromptConfig {
            developer_prompt: format!("Run {name}."),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
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
                    provider_tool_calls: vec![],
                    usage: TokenUsage::default(),
                }
            } else {
                ChatResponse {
                    text: Some(
                        r#"{"summary":"Security review complete.","issues":[],"confidence":"high"}"#
                            .into(),
                    ),
                    tool_calls: vec![],
                    provider_tool_calls: vec![],
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
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            1 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "wait".into(),
                    name: "wait".into(),
                    arguments: serde_json::json!({"seconds": 1}).to_string(),
                }],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            _ => ChatResponse {
                text: Some("parent complete".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
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

struct DelegateScriptLlm {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
    captured: CapturedRequests,
}

#[async_trait::async_trait]
impl ModelProvider for DelegateScriptLlm {
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
            self.child_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(ChatResponse {
                text: Some("delegated review complete".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            });
        }

        let call = self.parent_calls.fetch_add(1, Ordering::SeqCst);
        Ok(match call {
            0 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "list-delegatable".into(),
                    name: "list_delegatable_agents".into(),
                    arguments: serde_json::json!({}).to_string(),
                }],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            1 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "delegate".into(),
                    name: "delegate_to".into(),
                    arguments: serde_json::json!({
                        "agent": "reviewer",
                        "task": "Review the auth/session change and return findings first."
                    })
                    .to_string(),
                }],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            2 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "wait".into(),
                    name: "wait".into(),
                    arguments: serde_json::json!({"seconds": 1}).to_string(),
                }],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            3 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "wait-again".into(),
                    name: "wait".into(),
                    arguments: serde_json::json!({"seconds": 1}).to_string(),
                }],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            _ => ChatResponse {
                text: Some("parent saw delegated result".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
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

struct DelegateScriptFactory {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
    captured: CapturedRequests,
}

impl DelegateScriptFactory {
    fn new(captured: CapturedRequests) -> Self {
        Self {
            parent_calls: Arc::new(AtomicUsize::new(0)),
            child_calls: Arc::new(AtomicUsize::new(0)),
            captured,
        }
    }
}

impl ModelProviderFactory for DelegateScriptFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(DelegateScriptLlm {
            parent_calls: self.parent_calls.clone(),
            child_calls: self.child_calls.clone(),
            captured: self.captured.clone(),
        }))
    }
}

struct NestedAbilityDelegateLlm {
    parent_calls: Arc<AtomicUsize>,
    delegated_child_calls: Arc<AtomicUsize>,
    captured: CapturedRequests,
}

#[async_trait::async_trait]
impl ModelProvider for NestedAbilityDelegateLlm {
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

        if tool_names.iter().any(|name| name == "update_parent_agent") {
            return Ok(ChatResponse {
                text: Some("ability completed inside delegated child".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            });
        }

        if tool_names.iter().any(|name| name == "use_ability") {
            let call = self.delegated_child_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(match call {
                0 => ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "use-ability".into(),
                        name: "use_ability".into(),
                        arguments: serde_json::json!({
                            "ability": "security_review",
                            "task": "Review the delegated change and summarize the finding."
                        })
                        .to_string(),
                    }],
                    provider_tool_calls: vec![],
                    usage: TokenUsage::default(),
                },
                1 => ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "wait-ability".into(),
                        name: "wait".into(),
                        arguments: serde_json::json!({"seconds": 1}).to_string(),
                    }],
                    provider_tool_calls: vec![],
                    usage: TokenUsage::default(),
                },
                _ => ChatResponse {
                    text: Some("delegated child observed ability completion".into()),
                    tool_calls: vec![],
                    provider_tool_calls: vec![],
                    usage: TokenUsage::default(),
                },
            });
        }

        let call = self.parent_calls.fetch_add(1, Ordering::SeqCst);
        Ok(match call {
            0 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "delegate".into(),
                    name: "delegate_to".into(),
                    arguments: serde_json::json!({
                        "agent": "reviewer",
                        "task": "Use your assigned security_review ability and report the result."
                    })
                    .to_string(),
                }],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            1 | 2 => ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: format!("wait-delegation-{call}"),
                    name: "wait_operations".into(),
                    arguments: serde_json::json!({
                        "seconds": 1,
                        "kind": "delegation"
                    })
                    .to_string(),
                }],
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
            },
            _ => ChatResponse {
                text: Some("parent saw delegated ability result".into()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
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

struct NestedAbilityDelegateFactory {
    parent_calls: Arc<AtomicUsize>,
    delegated_child_calls: Arc<AtomicUsize>,
    captured: CapturedRequests,
}

impl NestedAbilityDelegateFactory {
    fn new(captured: CapturedRequests) -> Self {
        Self {
            parent_calls: Arc::new(AtomicUsize::new(0)),
            delegated_child_calls: Arc::new(AtomicUsize::new(0)),
            captured,
        }
    }
}

impl ModelProviderFactory for NestedAbilityDelegateFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(NestedAbilityDelegateLlm {
            parent_calls: self.parent_calls.clone(),
            delegated_child_calls: self.delegated_child_calls.clone(),
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
                    provider_tool_calls: vec![],
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
                    provider_tool_calls: vec![],
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
        .with_model_factory(FixedFactory::new("ok", captured.clone()))
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent("coder")
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
            .any(|tool| tool.name == "delegate_to")
    );
    runner.chat("coordinate review").await.unwrap();

    let first_tools = captured.tool_names().remove(0);
    for expected in [
        "spawn_sub_agents",
        "send_sub_agents",
        "inspect_sub_agents",
        "stop_sub_agents",
        "list_delegatable_agents",
        "delegate_to",
        "send_operation_input",
        "wait_operations",
        "wait",
    ] {
        assert!(
            first_tools.iter().any(|name| name == expected),
            "{first_tools:?}"
        );
    }
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
    let runner = provider.agent("solo").await.unwrap().build().await.unwrap();

    runner.chat("work").await.unwrap();
    let first_tools = captured.tool_names().remove(0);
    for expected in [
        "spawn_sub_agents",
        "send_sub_agents",
        "inspect_sub_agents",
        "stop_sub_agents",
        "list_delegatable_agents",
        "delegate_to",
        "send_operation_input",
        "wait_operations",
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
        .agent("coder")
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
            names.iter().any(|name| name == "platform_echo"),
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
async fn delegate_to_runs_installed_agent_with_own_capabilities_and_child_tools() {
    let captured = CapturedRequests::default();
    let model_id = Uuid::new_v4();
    let mut reviewer = agent(Uuid::new_v4(), "reviewer", model_id);
    reviewer.abilities = vec!["security_review".into()];
    let manifest = Manifest {
        agents: vec![agent(Uuid::new_v4(), "coder", model_id), reviewer],
        abilities: vec![ability("security_review")],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(DelegateScriptFactory::new(captured.clone()))
        .with_tool_factory(PlatformToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let output = runner.chat("delegate review").await.unwrap();
    assert_eq!(output.text, "parent saw delegated result");

    let tool_results = output
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    let list_result = tool_results
        .iter()
        .find(|content| content.contains(r#""tool_call_id":"list-delegatable""#))
        .expect("list_delegatable_agents tool result");
    let list_payload: serde_json::Value = serde_json::from_str(
        serde_json::from_str::<serde_json::Value>(list_result).unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let first_agent = &list_payload["agents"][0];
    assert_eq!(first_agent["slug"], "reviewer");
    assert_eq!(first_agent["description"], "reviewer agent");
    assert!(first_agent.get("name").is_none());
    assert!(first_agent.get("abilities").is_none());
    assert!(first_agent.get("platform_scopes").is_none());
    assert!(
        tool_results
            .iter()
            .any(|content| content.contains(r#"\"kind\":\"delegation\""#)
                && content.contains("delegated review complete")),
        "{tool_results:?}"
    );

    let child_tool_sets = captured
        .tool_names()
        .into_iter()
        .filter(|names| names.iter().any(|name| name == "update_parent_agent"))
        .collect::<Vec<_>>();
    assert_eq!(child_tool_sets.len(), 1, "{child_tool_sets:?}");
    let child_tools = &child_tool_sets[0];
    for expected in [
        "ask_parent_agent",
        "platform_echo",
        "list_assigned_abilities",
        "use_ability",
        "wait",
    ] {
        assert!(
            child_tools.iter().any(|name| name == expected),
            "{child_tools:?}"
        );
    }
    for forbidden in [
        "delegate_to",
        "list_delegatable_agents",
        "spawn_sub_agents",
        "send_sub_agents",
        "inspect_sub_agents",
        "stop_sub_agents",
        "inspect_operations",
        "send_operation_input",
        "stop_operations",
        "respond_to_user",
    ] {
        assert!(
            !child_tools.iter().any(|name| name == forbidden),
            "{child_tools:?}"
        );
    }

    let child_messages = captured
        .messages()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert!(
        child_messages
            .iter()
            .any(|message| message.contains("Delegated work boundary:")
                && message.contains("outside your role")),
        "{child_messages:?}"
    );
}

#[tokio::test]
async fn delegated_child_can_invoke_assigned_ability_and_wait_for_it() {
    let captured = CapturedRequests::default();
    let model_id = Uuid::new_v4();
    let mut reviewer = agent(Uuid::new_v4(), "reviewer", model_id);
    reviewer.abilities = vec!["security_review".into()];
    let manifest = Manifest {
        agents: vec![agent(Uuid::new_v4(), "coder", model_id), reviewer],
        abilities: vec![ability("security_review")],
        models: vec![model(model_id)],
        projects: vec![project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(NestedAbilityDelegateFactory::new(captured.clone()))
        .with_tool_factory(PlatformToolFactory)
        .build()
        .await
        .unwrap();
    let runner = provider
        .agent("coder")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let output = runner.chat("delegate ability review").await.unwrap();
    assert_eq!(output.text, "parent saw delegated ability result");

    let tool_sets = captured.tool_names();
    assert!(
        tool_sets
            .iter()
            .any(|names| names.iter().any(|name| name == "wait_operations")),
        "{tool_sets:?}"
    );
    assert!(
        tool_sets
            .iter()
            .any(|names| names.iter().any(|name| name == "use_ability")
                && names.iter().any(|name| name == "wait")),
        "{tool_sets:?}"
    );
    assert!(
        tool_sets.iter().any(
            |names| names.iter().any(|name| name == "update_parent_agent")
                && !names.iter().any(|name| name == "delegate_to")
        ),
        "{tool_sets:?}"
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
        .agent("coder")
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
        .agent("coder")
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
        .agent("alpha")
        .await
        .unwrap()
        .with_config(config)
        .build()
        .await
        .unwrap();

    runner.chat("work").await.unwrap();
    let first_tools = captured.tool_names().remove(0);
    assert_eq!(
        first_tools,
        vec![
            "list_knowledge_packs",
            "inspect_operations",
            "send_operation_input",
            "stop_operations",
            "wait_operations",
            "respond_to_user"
        ]
    );
}
