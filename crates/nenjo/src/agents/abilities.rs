//! Ability invocation tools.
//!
//! Each assigned ability is exposed as its own tool using the configured
//! `tool_name`. The caller delegates work through a single `task` argument.

use std::sync::Arc;

use anyhow::Result;
use nenjo_models::ModelProvider;
use tokio::sync::mpsc;
use tracing::debug;

use crate::tools::{Tool, ToolCategory, ToolResult};

use super::instance::{AgentInstance, AgentPromptState, AgentRuntime};
use super::runner::turn_loop;
use super::runner::types::TurnEvent;
use crate::input::{AgentRun, ChatInput};
use crate::manifest::{AbilityManifest, Manifest, PromptConfig, PromptTemplates};
use crate::provider::{ErasedProvider, ProviderRuntime, ToolFactory};

/// A single assigned ability exposed as a first-class tool.
pub struct AssignedAbilityTool<P: ProviderRuntime = ErasedProvider> {
    tool_name: String,
    ability: AbilityManifest,
    instance: Arc<AgentInstance<P>>,
    manifest: Arc<Manifest>,
    description: String,
}

impl<P: ProviderRuntime> AssignedAbilityTool<P> {
    /// Create a tool that invokes the assigned ability through a sub-agent run.
    pub fn new(
        ability: AbilityManifest,
        instance: Arc<AgentInstance<P>>,
        manifest: Arc<Manifest>,
    ) -> Self {
        let mut description_parts = Vec::new();
        if let Some(summary) = ability
            .description
            .as_ref()
            .filter(|text| !text.trim().is_empty())
        {
            description_parts.push(summary.trim().to_string());
        } else {
            description_parts.push(format!("Execute the '{}' ability.", ability.name));
        }
        if !ability.activation_condition.trim().is_empty() {
            description_parts.push(format!("Use when: {}", ability.activation_condition.trim()));
        }
        description_parts.push(
            "Provide a self-contained task: include all relevant code, artifacts, constraints, and context the ability needs. After the ability returns, base your response on its result."
                .to_string(),
        );
        let description = description_parts.join(" ");
        Self {
            tool_name: ability.tool_name.clone(),
            ability,
            instance,
            manifest,
            description,
        }
    }
}

pub(crate) fn ability_tool_name(ability: &AbilityManifest) -> String {
    ability.tool_name.clone()
}

#[async_trait::async_trait]
impl<P> Tool for AssignedAbilityTool<P>
where
    P: ProviderRuntime,
{
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A self-contained delegated task. Include all relevant user-provided context, code snippets, files, constraints, and expected output so the ability can complete the task without access to the caller conversation."
                }
            },
            "required": ["task"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let task_description = match args["task"].as_str() {
            Some(t) if !t.is_empty() => t,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("task is required".into()),
                });
            }
        };
        let ability = &self.ability;

        debug!(
            ability = ability.name,
            agent = self.instance.name(),
            "Activating ability"
        );

        // Build the sub-execution instance.
        let sub_instance = build_ability_instance(&self.instance, ability, &self.manifest).await;

        let caller_history_snapshot = turn_loop::current_chat_history().unwrap_or_default();
        let task = AgentRun::chat(ChatInput {
            message: task_description.to_string(),
            history: vec![],
            project_id: None,
        });
        if let Some(parent_tx) = turn_loop::current_events_tx() {
            debug!(
                ability = ability.name,
                ability_tool_name = ability.tool_name,
                "Emitting AbilityStarted"
            );
            let _ = parent_tx.send(TurnEvent::AbilityStarted {
                ability_tool_name: ability.tool_name.clone(),
                ability_name: ability.name.clone(),
                task_input: task_description.to_string(),
                caller_history: caller_history_snapshot,
            });
        }

        let prompts = sub_instance.build_prompts(&task);

        let tool_names: Vec<&str> = sub_instance
            .runtime
            .tools
            .iter()
            .map(|t| t.name())
            .collect();
        debug!(
            ability = ability.name,
            agent = self.instance.name(),
            tool_count = sub_instance.runtime.tools.len(),
            tools = ?tool_names,
            "Ability sub-agent prompt"
        );
        debug!("{prompts}");

        // Build messages for the sub-execution.
        let mut messages = Vec::new();

        if sub_instance
            .model
            .model_provider
            .supports_developer_role(&sub_instance.model.model_name)
            && !prompts.developer.is_empty()
        {
            messages.push(nenjo_models::ChatMessage::system(&prompts.system));
            messages.push(nenjo_models::ChatMessage::developer(&prompts.developer));
        } else {
            let combined = if prompts.developer.is_empty() {
                prompts.system
            } else {
                format!("{}\n\n{}", prompts.system, prompts.developer)
            };
            messages.push(nenjo_models::ChatMessage::system(&combined));
        }

        if let crate::input::AgentRunKind::Chat(chat) = &task.kind {
            messages.extend(chat.history.iter().cloned());
        }

        let user_message = if prompts.user_message.is_empty() {
            task_description.to_string()
        } else {
            prompts.user_message
        };
        debug!(
            ability = ability.name,
            user_message = %user_message,
            "Ability sub-agent user message"
        );
        messages.push(nenjo_models::ChatMessage::user(&user_message));

        let parent_events_tx = turn_loop::current_events_tx();
        let (nested_tx, mut nested_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let ability_tool_name = ability.tool_name.clone();
        let bridge = parent_events_tx.map(|parent_tx| {
            let ability_tool_name = ability_tool_name.clone();
            tokio::spawn(async move {
                while let Some(event) = nested_rx.recv().await {
                    match event {
                        TurnEvent::AbilityStarted { .. } => {
                            let _ = parent_tx.send(event);
                        }
                        TurnEvent::DelegationStarted { .. } => {
                            let _ = parent_tx.send(event);
                        }
                        TurnEvent::ToolCallStart {
                            parent_tool_name,
                            calls,
                        } => {
                            let _ = parent_tx.send(TurnEvent::ToolCallStart {
                                parent_tool_name: parent_tool_name
                                    .or_else(|| Some(ability_tool_name.clone())),
                                calls,
                            });
                        }
                        TurnEvent::ToolCallEnd {
                            parent_tool_name,
                            tool_call_id,
                            tool_name,
                            tool_args,
                            result,
                        } => {
                            let _ = parent_tx.send(TurnEvent::ToolCallEnd {
                                parent_tool_name: parent_tool_name
                                    .or_else(|| Some(ability_tool_name.clone())),
                                tool_call_id,
                                tool_name,
                                tool_args,
                                result,
                            });
                        }
                        TurnEvent::AbilityCompleted { .. } => {
                            let _ = parent_tx.send(event);
                        }
                        TurnEvent::DelegationCompleted { .. } => {
                            let _ = parent_tx.send(event);
                        }
                        TurnEvent::MessageCompacted { .. } => {
                            let _ = parent_tx.send(event);
                        }
                        TurnEvent::TranscriptMessage { .. } => {}
                        _ => {}
                    }
                }
            })
        });

        // Run the sub turn loop with nested events enabled.
        let result = turn_loop::run(&sub_instance, messages, Some(nested_tx), None).await;

        if let Some(bridge) = bridge {
            let _ = bridge.await;
        }

        match result {
            Ok(output) => {
                turn_loop::record_nested_token_usage(output.input_tokens, output.output_tokens);
                if let Some(parent_tx) = turn_loop::current_events_tx() {
                    debug!(
                        ability = ability.name,
                        ability_tool_name = ability.tool_name,
                        "Emitting AbilityCompleted success=true"
                    );
                    let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                        ability_tool_name: ability.tool_name.clone(),
                        ability_name: ability.name.clone(),
                        success: true,
                        final_output: output.text.clone(),
                    });
                }
                Ok(ToolResult {
                    success: true,
                    output: output.text,
                    error: None,
                })
            }
            Err(e) => {
                let error = format!("ability execution failed: {e}");
                if let Some(parent_tx) = turn_loop::current_events_tx() {
                    debug!(
                        ability = ability.name,
                        ability_tool_name = ability.tool_name,
                        "Emitting AbilityCompleted success=false"
                    );
                    let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                        ability_tool_name: ability.tool_name.clone(),
                        ability_name: ability.name.clone(),
                        success: false,
                        final_output: error.clone(),
                    });
                }
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error),
                })
            }
        }
    }
}

pub(crate) fn build_ability_tools<P>(
    abilities: &[AbilityManifest],
    instance: Arc<AgentInstance<P>>,
    manifest: Arc<Manifest>,
) -> Vec<Arc<dyn Tool>>
where
    P: ProviderRuntime,
{
    abilities
        .iter()
        .cloned()
        .map(|ability| {
            Arc::new(AssignedAbilityTool::new(
                ability,
                instance.clone(),
                manifest.clone(),
            )) as Arc<dyn Tool>
        })
        .collect()
}

pub(crate) fn is_ability_tool(name: &str, abilities: &[AbilityManifest]) -> bool {
    abilities.iter().any(|ability| ability.tool_name == name)
}

/// Build a temporary AgentInstance for the ability sub-execution.
///
/// Resolves the ability's `mcp_server_ids` from the manifest and merges them
/// into the sub-instance's prompt context.
async fn build_ability_instance<P>(
    caller: &AgentInstance<P>,
    ability: &AbilityManifest,
    manifest: &Manifest,
) -> AgentInstance<P>
where
    P: ProviderRuntime,
{
    // Inherit system prompt, override developer prompt.
    let prompt_config = PromptConfig {
        system_prompt: caller.prompt_config().system_prompt.clone(),
        developer_prompt: ability.prompt_config.developer_prompt.clone(),
        templates: PromptTemplates {
            chat_task: "{{ chat.message }}".into(),
            ..Default::default()
        },
        memory_profile: caller.prompt_config().memory_profile.clone(),
    };

    let mut caller_tools: Vec<Arc<dyn Tool>> = caller
        .runtime
        .tools
        .iter()
        .filter(|tool| {
            !caller
                .prompt
                .context
                .available_abilities
                .iter()
                .any(|ability| ability.tool_name == tool.name())
        })
        .cloned()
        .collect();

    let mut merged_scopes = caller.manifest.platform_scopes.clone();
    for scope in &ability.platform_scopes {
        if !merged_scopes.contains(scope) {
            merged_scopes.push(scope.clone());
        }
    }

    let mut merged_mcp_server_ids = caller.manifest.mcp_server_ids.clone();
    for server_id in &ability.mcp_server_ids {
        if !merged_mcp_server_ids.contains(server_id) {
            merged_mcp_server_ids.push(*server_id);
        }
    }

    let mut scoped_security = (*caller.runtime.security).clone();
    for env_name in ability_runtime_env_names(ability) {
        if !scoped_security
            .forwarded_env_names
            .iter()
            .any(|existing| existing == &env_name)
        {
            scoped_security.forwarded_env_names.push(env_name);
        }
    }
    let scoped_security = Arc::new(scoped_security);

    let mut tools = if let Some(provider) = caller.runtime.provider_runtime.as_ref() {
        let mut scoped_agent = caller.manifest.clone();
        scoped_agent.platform_scopes = merged_scopes.clone();
        scoped_agent.mcp_server_ids = merged_mcp_server_ids.clone();
        provider
            .tool_factory()
            .create_tools_with_security(&scoped_agent, scoped_security.clone())
            .await
    } else {
        Vec::new()
    };

    let mut tool_names: std::collections::HashSet<String> =
        tools.iter().map(|tool| tool.name().to_string()).collect();
    for tool in caller_tools.drain(..) {
        if tool_names.insert(tool.name().to_string()) {
            tools.push(tool);
        }
    }

    // Build a prompt context without abilities (no recursion).
    let mut prompt_context = caller.prompt.context.clone();
    prompt_context.available_abilities = vec![];
    prompt_context.agent_name = format!("{}:{}", caller.name(), ability.name);
    prompt_context.append_active_domain_addon = false;
    prompt_context.platform_scopes = merged_scopes.clone();

    // Resolve and merge the ability's MCP server info from the manifest.
    for server in manifest
        .mcp_servers
        .iter()
        .filter(|s| ability.mcp_server_ids.contains(&s.id))
    {
        let entry = (
            server.display_name.clone(),
            server.description.clone().unwrap_or_default(),
        );
        if !prompt_context
            .mcp_server_info
            .iter()
            .any(|e| e.0 == entry.0)
        {
            prompt_context.mcp_server_info.push(entry);
        }
    }

    let mut scoped_manifest = caller.manifest.clone();
    scoped_manifest.name = format!("{}:{}", caller.name(), ability.name);
    scoped_manifest.description = Some(
        ability
            .description
            .clone()
            .unwrap_or_else(|| caller.description().to_string()),
    );
    scoped_manifest.prompt_config = prompt_config;
    scoped_manifest.platform_scopes = merged_scopes;
    scoped_manifest.mcp_server_ids = merged_mcp_server_ids;

    AgentInstance {
        manifest: scoped_manifest,
        model: caller.model.clone(),
        prompt: AgentPromptState {
            context: prompt_context,
            renderer: caller.prompt.renderer.clone(),
            memory_vars: caller.prompt.memory_vars.clone(),
            artifact_vars: caller.prompt.artifact_vars.clone(),
        },
        runtime: AgentRuntime {
            tools,
            security: scoped_security,
            config: caller.runtime.config.clone(),
            provider_runtime: caller.runtime.provider_runtime.clone(),
        },
    }
}

fn ability_runtime_env_names(ability: &AbilityManifest) -> Vec<String> {
    ability
        .metadata
        .pointer("/runtime/env_names")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .filter(|name| {
                    let mut chars = name.chars();
                    let first = chars.next().unwrap_or_default();
                    (first.is_ascii_alphabetic() || first == '_')
                        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::instance::AgentModel;
    use crate::agents::prompts::PromptContext;
    use crate::config::AgentConfig;
    use crate::context::{ContextRenderer, types::RenderContextBlock};
    use crate::manifest::{
        AbilityPromptConfig, AgentManifest, DomainManifest, DomainPromptConfig, Manifest,
        PromptConfig,
    };
    use crate::provider::{ErasedProvider, ModelProviderFactory, Provider, ToolFactory};
    use crate::tools::{ToolCategory, ToolResult, ToolSecurity};
    use crate::types::ActiveDomain;
    use anyhow::Result;
    use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider};

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

    struct TestModelFactory;

    impl ModelProviderFactory for TestModelFactory {
        fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
            Ok(Arc::new(NoopProvider))
        }
    }

    struct TestTool {
        name: &'static str,
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

        async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: self.name.to_string(),
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
            let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(TestTool { name: "shell" })];
            if agent
                .platform_scopes
                .iter()
                .any(|scope| scope == "agents:read")
            {
                tools.push(Arc::new(TestTool {
                    name: "list_agents",
                }));
            }
            if agent
                .platform_scopes
                .iter()
                .any(|scope| scope == "agents:write")
            {
                tools.push(Arc::new(TestTool {
                    name: "create_agent",
                }));
            }
            tools
        }
    }

    fn test_sdk_provider() -> ErasedProvider {
        Provider::new_inner(
            Arc::new(Manifest::default()),
            Arc::new(TestModelFactory),
            Arc::new(TestToolFactory),
            None,
            AgentConfig::default(),
            Default::default(),
            Default::default(),
        )
    }

    fn test_instance_with_active_domain() -> AgentInstance {
        AgentInstance {
            manifest: AgentManifest {
                id: uuid::Uuid::new_v4(),
                name: "nenji".into(),
                description: Some("system agent".into()),
                prompt_config: PromptConfig {
                    system_prompt: "caller system".into(),
                    developer_prompt: "caller developer".into(),
                    templates: Default::default(),
                    memory_profile: Default::default(),
                },
                color: None,
                model_id: Some(uuid::Uuid::new_v4()),
                domain_ids: vec![],
                platform_scopes: vec!["agents:read".into()],
                mcp_server_ids: vec![],
                ability_ids: vec![],
                prompt_locked: false,
                heartbeat: None,
            },
            model: AgentModel {
                model_name: "mock".into(),
                id: uuid::Uuid::new_v4(),
                temperature: 0.2,
                model_provider: Arc::new(NoopProvider),
            },
            prompt: AgentPromptState {
                context: PromptContext {
                    agent_name: "nenji".into(),
                    agent_description: "system agent".into(),
                    available_agents: vec![],
                    available_routines: vec![],
                    current_project: crate::manifest::ProjectManifest {
                        id: uuid::Uuid::nil(),
                        name: String::new(),
                        slug: String::new(),
                        description: None,
                        settings: serde_json::Value::Null,
                    },
                    available_abilities: vec![],
                    available_domains: vec![],
                    mcp_server_info: vec![],
                    platform_scopes: vec!["agents:read".into()],
                    active_domain: Some(ActiveDomain {
                        session_id: uuid::Uuid::new_v4(),
                        domain_id: uuid::Uuid::new_v4(),
                        domain_name: "creator".into(),
                        manifest: DomainManifest {
                            id: uuid::Uuid::new_v4(),
                            name: "creator".into(),
                            path: "nenjo/creator".into(),
                            display_name: "Creator".into(),
                            description: None,
                            command: "#creator".into(),
                            platform_scopes: vec![],
                            ability_ids: vec![],
                            mcp_server_ids: vec![],
                            prompt_config: DomainPromptConfig {
                                developer_prompt_addon: Some("domain addon".into()),
                            },
                        },
                    }),
                    append_active_domain_addon: true,
                    docs_base_dir: None,
                    render_ctx_extra: Default::default(),
                },
                renderer: ContextRenderer::from_blocks(&[]),
                memory_vars: Default::default(),
                artifact_vars: Default::default(),
            },
            runtime: AgentRuntime {
                tools: vec![],
                security: Arc::new(ToolSecurity::default()),
                config: AgentConfig::default(),
                provider_runtime: Some(test_sdk_provider()),
            },
        }
    }

    #[tokio::test]
    async fn ability_sub_instance_uses_ability_prompt_without_domain_addon() {
        let caller = test_instance_with_active_domain();
        let ability = AbilityManifest {
            id: uuid::Uuid::new_v4(),
            name: "agent_builder".into(),
            tool_name: "design_agent".into(),
            path: "nenjo/platform".into(),
            display_name: Some("Agent Builder".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "ability developer".into(),
            },
            platform_scopes: vec!["agents:write".into()],
            mcp_server_ids: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability, &Manifest::default()).await;
        let prompts = sub_instance.build_prompts(&AgentRun::chat(ChatInput {
            message: "build an agent".into(),
            history: vec![],
            project_id: None,
        }));

        assert_eq!(prompts.system, "caller system");
        assert_eq!(prompts.developer, "ability developer");
        assert!(!prompts.developer.contains("domain addon"));
        assert!(!sub_instance.prompt.context.append_active_domain_addon);
        let tool_names: Vec<_> = sub_instance
            .runtime
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect();
        assert!(tool_names.contains(&"list_agents"));
        assert!(tool_names.contains(&"create_agent"));
    }

    #[tokio::test]
    async fn ability_sub_instance_renders_context_blocks_and_user_message() {
        let mut caller = test_instance_with_active_domain();
        caller.manifest.prompt_config.system_prompt = "{{ nenjo.core.methodology }}".into();
        caller.prompt.renderer = ContextRenderer::from_blocks(&[
            RenderContextBlock {
                name: "methodology".into(),
                path: "nenjo/core".into(),
                template: "<methodology>{{ agent.role }}</methodology>".into(),
            },
            RenderContextBlock {
                name: "tool_usage".into(),
                path: "nenjo/core".into(),
                template: "<tool_usage>{{ agent.role }}</tool_usage>".into(),
            },
        ]);
        let ability = AbilityManifest {
            id: uuid::Uuid::new_v4(),
            name: "agent_builder".into(),
            tool_name: "design_agent".into(),
            path: "nenjo/platform".into(),
            display_name: Some("Agent Builder".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "{{ nenjo.core.tool_usage }}".into(),
            },
            platform_scopes: vec!["agents:write".into()],
            mcp_server_ids: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability, &Manifest::default()).await;
        let prompts = sub_instance.build_prompts(&AgentRun::chat(ChatInput {
            message: "build an agent".into(),
            history: vec![],
            project_id: None,
        }));

        assert_eq!(
            prompts.system,
            "<methodology>nenji:agent_builder</methodology>"
        );
        assert_eq!(
            prompts.developer,
            "<tool_usage>nenji:agent_builder</tool_usage>"
        );
        assert_eq!(prompts.user_message, "build an agent");
    }

    #[tokio::test]
    async fn ability_sub_instance_preserves_non_factory_tools_without_duplicates() {
        let mut caller = test_instance_with_active_domain();
        caller.runtime.tools = vec![
            Arc::new(TestTool { name: "shell" }),
            Arc::new(TestTool {
                name: "remember_fact",
            }),
        ];
        let ability = AbilityManifest {
            id: uuid::Uuid::new_v4(),
            name: "agent_builder".into(),
            tool_name: "design_agent".into(),
            path: "nenjo/platform".into(),
            display_name: Some("Agent Builder".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "ability developer".into(),
            },
            platform_scopes: vec!["agents:write".into()],
            mcp_server_ids: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability, &Manifest::default()).await;
        let tool_names: Vec<_> = sub_instance
            .runtime
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect();

        assert_eq!(
            tool_names.iter().filter(|name| **name == "shell").count(),
            1
        );
        assert!(tool_names.contains(&"remember_fact"));
    }

    #[test]
    fn ability_tool_schema_requires_self_contained_task_input() {
        let ability = AbilityManifest {
            id: uuid::Uuid::new_v4(),
            name: "review".into(),
            tool_name: "code_review".into(),
            path: "review".into(),
            display_name: Some("Code Review".into()),
            description: Some("Reviews code".into()),
            activation_condition: "When code review is needed".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "review code".into(),
            },
            platform_scopes: vec![],
            mcp_server_ids: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };
        let tool = AssignedAbilityTool::new(
            ability,
            Arc::new(test_instance_with_active_domain()),
            Arc::new(Manifest::default()),
        );

        let description = tool.description();
        let schema = tool.parameters_schema();
        let task_description = schema["properties"]["task"]["description"]
            .as_str()
            .unwrap_or_default();

        assert!(description.contains("self-contained task"));
        assert!(task_description.contains("self-contained delegated task"));
        assert!(task_description.contains("code snippets"));
        assert!(task_description.contains("without access to the caller conversation"));
    }
}
