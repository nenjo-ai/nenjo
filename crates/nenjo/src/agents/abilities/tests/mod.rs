//! Shared fixtures for ability and operation-control behavior tests.

mod broker;
mod completion;
mod controls;
mod execution;
mod instance;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

use nenjo_models::traits::{
    ChatRequest, ChatResponse, ConversationMessage, ModelProvider, TokenUsage, ToolCall,
};

use crate::agents::async_ops::{
    AsyncOpId, AsyncOpKind, AsyncOpManager, AsyncOpSignal, AsyncOpWaitFilter, StartAsyncOp,
    build_async_operation_tools,
};
use crate::agents::delegation::DELEGATE_TO_TOOL_NAME;
use crate::agents::instance::{
    AgentExecutionMode, AgentInstance, AgentModel, AgentPromptState, AgentRuntime,
};
use crate::agents::prompts::PromptContext;
use crate::agents::runner::turn_loop;
use crate::agents::runner::types::{AsyncOperationTranscriptEvent, TurnEvent};
use crate::concurrency::{AdmissionPermit, AdmissionPool, ExecutionContext};
use crate::config::AgentConfig;
use crate::context::{ContextRenderer, types::RenderContextBlock};
use crate::input::{AgentRun, ChatInput};
use crate::manifest::{
    AbilityManifest, AbilityPromptConfig, AgentManifest, DomainManifest, DomainPromptConfig,
    Manifest, PromptConfig,
};
use crate::provider::{ErasedProvider, ModelProviderFactory, Provider, ToolFactory};
use crate::tools::{
    AsyncControl, AsyncControls, SEND_INPUT_TOOL_NAME, Tool, ToolCategory, ToolOrigin, ToolResult,
    ToolSecurity,
};
use crate::types::ActiveDomain;

use super::broker::{ListAssignedAbilitiesTool, UseAbilityTool};
use super::child_tools::{AbilityFinish, AbilityFinishStatus, FinishAbilityTool};
use super::events::bridge_ability_transcript;
use super::execution::{AbilityOperation, run_ability_operation};
use super::instance::build_ability_instance;
use super::registry::AbilityRegistry;
use super::*;

struct NoopProvider;

#[async_trait::async_trait]
impl ModelProvider for NoopProvider {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        panic!("chat should not be called in ability prompt tests");
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

struct SequentialProvider {
    responses: Vec<ChatResponse>,
    next: AtomicUsize,
    seen_messages: Mutex<Vec<Vec<ConversationMessage>>>,
}

#[async_trait::async_trait]
impl ModelProvider for SequentialProvider {
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
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .responses
            .get(index)
            .unwrap_or_else(|| self.responses.last().unwrap())
            .clone())
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

struct TestModelFactory;

struct QueuedAbilityProvider {
    inner: Arc<SequentialProvider>,
    capacity: AdmissionPool,
}

#[async_trait::async_trait]
impl ModelProvider for QueuedAbilityProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let _permit = self.capacity.acquire().await?;
        self.inner.chat(request, model, temperature).await
    }

    fn supports_native_tools(&self) -> bool {
        true
    }
}

struct BookmarkTool {
    capacity: AdmissionPool,
    occupied: mpsc::UnboundedSender<AdmissionPermit>,
}

#[async_trait::async_trait]
impl Tool for BookmarkTool {
    fn name(&self) -> &str {
        "mcp_get_users_bookmarks"
    }

    fn description(&self) -> &str {
        "Return bookmarks while another request occupies model capacity."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Mcp
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        self.occupied.send(self.capacity.acquire().await?)?;
        Ok(json_tool(
            serde_json::json!({"data": [{"id": "bookmark-1"}]}),
        ))
    }
}

struct BookmarkToolFactory(Arc<BookmarkTool>);

#[async_trait::async_trait]
impl ToolFactory for BookmarkToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        vec![self.0.clone()]
    }
}

impl ModelProviderFactory for TestModelFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(NoopProvider))
    }
}

struct TestTool {
    name: &'static str,
    origin: ToolOrigin,
}

struct OtherTerminalTool;

#[async_trait::async_trait]
impl Tool for OtherTerminalTool {
    fn name(&self) -> &str {
        "other_terminal"
    }

    fn description(&self) -> &str {
        "An unrelated terminal tool used to verify completion selection."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn is_terminal(&self) -> bool {
        true
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: "unrelated terminal output".into(),
            error: None,
        })
    }
}

#[async_trait::async_trait]
impl Tool for TestTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.name
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        self.origin
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: self.name.to_string().into(),
            error: None,
        })
    }
}

struct TestToolFactory;

#[async_trait::async_trait]
impl ToolFactory for TestToolFactory {
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        self.create_tools_with_security(agent, Arc::new(ToolSecurity::default()))
            .await
    }

    async fn create_tools_with_security(
        &self,
        agent: &AgentManifest,
        _security: Arc<ToolSecurity>,
    ) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(TestTool {
            name: "shell",
            origin: ToolOrigin::Host,
        })];
        if agent
            .platform_scopes
            .iter()
            .any(|scope| scope == "agents:read" || scope == "agents:write")
        {
            tools.push(Arc::new(TestTool {
                name: "list_agents",
                origin: ToolOrigin::Platform,
            }));
        }
        if agent
            .platform_scopes
            .iter()
            .any(|scope| scope == "agents:write")
        {
            tools.push(Arc::new(TestTool {
                name: "create_agent",
                origin: ToolOrigin::Platform,
            }));
        }
        tools
    }
}

fn test_sdk_provider() -> ErasedProvider {
    test_sdk_provider_with_tools(Arc::new(TestToolFactory))
}

fn test_sdk_provider_with_tools(tool_factory: Arc<dyn ToolFactory>) -> ErasedProvider {
    Provider::new_inner(
        Arc::new(Manifest::default()),
        crate::provider::ProviderServices {
            model_factory: Arc::new(TestModelFactory),
            tool_factory,
            memory: None,
            agent_config: AgentConfig::default(),
            root_admission: None,
            render_ctx_extra: Default::default(),
            argument_bindings: Default::default(),
            knowledge: Default::default(),
            artifact_input_preparer: None,
            routine_execution_config: Default::default(),
        },
    )
}

fn test_instance_with_active_domain() -> AgentInstance {
    let execution_cancel = tokio_util::sync::CancellationToken::new();
    let async_ops = AsyncOpManager::with_cancel(execution_cancel.clone());

    AgentInstance {
        manifest: AgentManifest {
            name: "nenji".into(),
            slug: crate::Slug::derive("nenji"),
            description: Some("system agent".into()),
            prompt_config: PromptConfig {
                system_prompt: "caller system".into(),
                developer_prompt: "caller developer".into(),
                memory_profile: Default::default(),
            },
            color: None,
            model: Some(crate::Slug::derive("mock")),
            domains: vec![],
            platform_scopes: vec!["agents:read".into()],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            abilities: vec![],
            prompt_locked: false,
            source_type: None,
            metadata: serde_json::json!({}),
        },
        model_manifest: crate::manifest::ModelManifest {
            name: "mock".into(),
            slug: crate::Slug::derive("mock"),
            description: None,
            model: "mock".into(),
            model_provider: "mock".into(),
            temperature: Some(0.2),
            context_window: None,
            base_url: None,
            native_tools: vec![],
            capabilities: Vec::new(),
            input_modalities: Vec::new(),
            output_modalities: Vec::new(),
            execution_modes: Vec::new(),
        },
        model: AgentModel {
            model_name: "mock".into(),
            model_slug: crate::Slug::derive("mock"),
            temperature: 0.2,
            model_provider: Arc::new(NoopProvider),
        },
        prompt: AgentPromptState {
            context: PromptContext {
                current_project: crate::manifest::ProjectManifest {
                    name: String::new(),
                    slug: crate::Slug::derive("project"),
                    description: None,
                    settings: serde_json::Value::Null,
                },
                active_domain: Some(ActiveDomain {
                    session_id: uuid::Uuid::new_v4(),
                    manifest: DomainManifest {
                        slug: crate::Slug::derive("creator"),
                        name: "creator".into(),
                        path: "nenjo/creator".into(),
                        description: None,
                        command: "#creator".into(),
                        platform_scopes: vec![],
                        abilities: vec![],
                        mcp_servers: vec![],
                        script_tools: Vec::new(),
                        media: Vec::new(),
                        prompt_config: DomainPromptConfig {
                            developer_prompt_addon: Some("domain addon".into()),
                        },
                    },
                }),
                append_active_domain_addon: true,
                render_ctx_extra: Default::default(),
                argument_bindings: Default::default(),
            },
            renderer: ContextRenderer::from_blocks(&[]),
            memory_context: Default::default(),
        },
        runtime: AgentRuntime {
            tools: vec![],
            security: Arc::new(ToolSecurity::default()),
            config: AgentConfig::default(),
            provider_runtime: Some(test_sdk_provider()),
            sub_agent_ctx: None,
            async_ops,
            execution_cancel,
            execution_mode: AgentExecutionMode::Parent,
            hook_runtime: None,
            current_session_id: None,
        },
    }
}
