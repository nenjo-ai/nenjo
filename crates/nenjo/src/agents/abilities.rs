//! Ability invocation tools.
//!
//! Each assigned ability is exposed as its own tool using the configured
//! `tool_name`. The caller delegates work through a single `task` argument.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::debug;

use nenjo_tools::{Tool, ToolCategory, ToolResult};

use super::instance::AgentInstance;
use super::runner::turn_loop;
use super::runner::types::TurnEvent;
use crate::manifest::{AbilityManifest, Manifest, PromptConfig, PromptTemplates};
use crate::types::TaskType;

/// A single assigned ability exposed as a first-class tool.
pub struct AssignedAbilityTool {
    ability: AbilityManifest,
    instance: Arc<AgentInstance>,
    manifest: Arc<Manifest>,
    description: String,
}

impl AssignedAbilityTool {
    pub fn new(
        ability: AbilityManifest,
        instance: Arc<AgentInstance>,
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
        let description = description_parts.join(" ");
        Self {
            ability,
            instance,
            manifest,
            description,
        }
    }
}

pub fn ability_tool_name(ability: &AbilityManifest) -> String {
    ability.tool_name.clone()
}

#[async_trait::async_trait]
impl Tool for AssignedAbilityTool {
    #[allow(clippy::misnamed_getters)]
    fn name(&self) -> &str {
        &self.ability.tool_name
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
                    "description": "The delegated task for this ability to handle"
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
            agent = self.instance.name,
            "Activating ability"
        );

        // Build the sub-execution instance.
        let sub_instance = build_ability_instance(&self.instance, ability, &self.manifest).await;

        let caller_history_snapshot = turn_loop::current_chat_history().unwrap_or_default();
        let task = TaskType::Chat {
            user_message: task_description.to_string(),
            history: vec![],
            project_id: uuid::Uuid::nil(),
        };
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

        let tool_names: Vec<&str> = sub_instance.tools.iter().map(|t| t.name()).collect();
        debug!(
            ability = ability.name,
            agent = self.instance.name,
            tool_count = sub_instance.tools.len(),
            tools = ?tool_names,
            "Ability sub-agent prompt"
        );
        debug!("{prompts}");

        // Build messages for the sub-execution.
        let mut messages = Vec::new();

        if sub_instance
            .provider
            .supports_developer_role(&sub_instance.model)
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

        if let TaskType::Chat { history, .. } = &task {
            messages.extend(history.iter().cloned());
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
                            tool_name,
                            result,
                        } => {
                            let _ = parent_tx.send(TurnEvent::ToolCallEnd {
                                parent_tool_name: parent_tool_name
                                    .or_else(|| Some(ability_tool_name.clone())),
                                tool_name,
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

pub fn build_ability_tools(
    abilities: &[AbilityManifest],
    instance: Arc<AgentInstance>,
    manifest: Arc<Manifest>,
) -> Vec<Arc<dyn Tool>> {
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

pub fn is_ability_tool(name: &str, abilities: &[AbilityManifest]) -> bool {
    abilities.iter().any(|ability| ability.tool_name == name)
}

/// Build a temporary AgentInstance for the ability sub-execution.
///
/// Resolves the ability's `mcp_server_ids` from the manifest and merges them
/// into the sub-instance's prompt context.
async fn build_ability_instance(
    caller: &AgentInstance,
    ability: &AbilityManifest,
    manifest: &Manifest,
) -> AgentInstance {
    // Inherit system prompt, override developer prompt.
    let prompt_config = PromptConfig {
        system_prompt: caller.prompt_config.system_prompt.clone(),
        developer_prompt: ability.prompt_config.developer_prompt.clone(),
        templates: PromptTemplates {
            chat_task: "{{ chat.message }}".into(),
            ..Default::default()
        },
        memory_profile: caller.prompt_config.memory_profile.clone(),
    };

    let mut caller_tools: Vec<Arc<dyn Tool>> = caller
        .tools
        .iter()
        .filter(|tool| {
            !caller
                .prompt_context
                .available_abilities
                .iter()
                .any(|ability| ability.tool_name == tool.name())
        })
        .cloned()
        .collect();

    let mut merged_scopes = caller
        .source_manifest
        .as_ref()
        .map(|agent| agent.platform_scopes.clone())
        .unwrap_or_else(|| caller.prompt_context.platform_scopes.clone());
    for scope in &ability.platform_scopes {
        if !merged_scopes.contains(scope) {
            merged_scopes.push(scope.clone());
        }
    }

    let mut merged_mcp_server_ids = caller
        .source_manifest
        .as_ref()
        .map(|agent| agent.mcp_server_ids.clone())
        .unwrap_or_default();
    for server_id in &ability.mcp_server_ids {
        if !merged_mcp_server_ids.contains(server_id) {
            merged_mcp_server_ids.push(*server_id);
        }
    }

    let mut tools = if let (Some(agent), Some(tool_factory)) = (
        caller.source_manifest.as_ref(),
        caller.tool_factory.as_ref(),
    ) {
        let mut scoped_agent = agent.clone();
        scoped_agent.platform_scopes = merged_scopes.clone();
        scoped_agent.mcp_server_ids = merged_mcp_server_ids.clone();
        tool_factory
            .create_tools_with_security(&scoped_agent, caller.security.clone())
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
    let mut prompt_context = caller.prompt_context.clone();
    prompt_context.available_abilities = vec![];
    prompt_context.agent_name = format!("{}:{}", caller.name, ability.name);
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

    AgentInstance {
        name: format!("{}:{}", caller.name, ability.name),
        description: ability
            .description
            .clone()
            .unwrap_or_else(|| caller.description.clone()),
        agent_id: caller.agent_id,
        model: caller.model.clone(),
        model_id: caller.model_id,
        temperature: caller.temperature,
        prompt_config,
        prompt_context,
        provider: caller.provider.clone(),
        tools,
        security: caller.security.clone(),
        agent_config: caller.agent_config.clone(),
        context_renderer: caller.context_renderer.clone(),
        source_manifest: caller.source_manifest.as_ref().map(|agent| {
            let mut scoped_agent = agent.clone();
            scoped_agent.platform_scopes = merged_scopes.clone();
            scoped_agent.mcp_server_ids = merged_mcp_server_ids;
            scoped_agent
        }),
        tool_factory: caller.tool_factory.clone(),
        memory_vars: caller.memory_vars.clone(),
        resource_vars: caller.resource_vars.clone(),
        documents_xml: caller.documents_xml.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::prompts::PromptContext;
    use crate::config::AgentConfig;
    use crate::context::{ContextRenderer, types::RenderContextBlock};
    use crate::manifest::{
        AbilityPromptConfig, AgentManifest, DomainManifest, DomainPromptConfig, PromptConfig,
    };
    use crate::provider::ToolFactory;
    use crate::types::ActiveDomain;
    use anyhow::Result;
    use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider};
    use nenjo_tools::security::SecurityPolicy;
    use nenjo_tools::{ToolCategory, ToolResult};

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
            self.create_tools_with_security(agent, Arc::new(SecurityPolicy::default()))
                .await
        }

        async fn create_tools_with_security(
            &self,
            agent: &AgentManifest,
            _security: Arc<SecurityPolicy>,
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

    fn test_instance_with_active_domain() -> AgentInstance {
        AgentInstance {
            name: "nenji".into(),
            description: "system agent".into(),
            agent_id: Some(uuid::Uuid::new_v4()),
            model: "mock".into(),
            model_id: uuid::Uuid::new_v4(),
            temperature: 0.2,
            prompt_config: PromptConfig {
                system_prompt: "caller system".into(),
                developer_prompt: "caller developer".into(),
                templates: Default::default(),
                memory_profile: Default::default(),
            },
            prompt_context: PromptContext {
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
            provider: Arc::new(NoopProvider),
            tools: vec![],
            security: Arc::new(SecurityPolicy::default()),
            agent_config: AgentConfig::default(),
            context_renderer: ContextRenderer::from_blocks(&[]),
            source_manifest: Some(AgentManifest {
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
            }),
            tool_factory: Some(Arc::new(TestToolFactory)),
            memory_vars: Default::default(),
            resource_vars: Default::default(),
            documents_xml: String::new(),
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
        };

        let sub_instance = build_ability_instance(&caller, &ability, &Manifest::default()).await;
        let prompts = sub_instance.build_prompts(&TaskType::Chat {
            user_message: "build an agent".into(),
            history: vec![],
            project_id: uuid::Uuid::nil(),
        });

        assert_eq!(prompts.system, "caller system");
        assert_eq!(prompts.developer, "ability developer");
        assert!(!prompts.developer.contains("domain addon"));
        assert!(!sub_instance.prompt_context.append_active_domain_addon);
        let tool_names: Vec<_> = sub_instance.tools.iter().map(|tool| tool.name()).collect();
        assert!(tool_names.contains(&"list_agents"));
        assert!(tool_names.contains(&"create_agent"));
    }

    #[tokio::test]
    async fn ability_sub_instance_renders_context_blocks_and_user_message() {
        let mut caller = test_instance_with_active_domain();
        caller.prompt_config.system_prompt = "{{ nenjo.core.methodology }}".into();
        caller.context_renderer = ContextRenderer::from_blocks(&[
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
        };

        let sub_instance = build_ability_instance(&caller, &ability, &Manifest::default()).await;
        let prompts = sub_instance.build_prompts(&TaskType::Chat {
            user_message: "build an agent".into(),
            history: vec![],
            project_id: uuid::Uuid::nil(),
        });

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
        caller.tools = vec![
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
        };

        let sub_instance = build_ability_instance(&caller, &ability, &Manifest::default()).await;
        let tool_names: Vec<_> = sub_instance.tools.iter().map(|tool| tool.name()).collect();

        assert_eq!(
            tool_names.iter().filter(|name| **name == "shell").count(),
            1
        );
        assert!(tool_names.contains(&"remember_fact"));
    }
}
