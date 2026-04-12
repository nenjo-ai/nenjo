//! AbilityTool — each assigned ability becomes a dedicated tool.
//!
//! Instead of a single `use_ability` tool that dispatches by name, each ability
//! assigned to an agent is registered as its own tool:
//! `ability/{dotted.path.name}`.
//! This gives the LLM direct visibility into each ability via the tool list.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::debug;

use nenjo_tools::{Tool, ToolCategory, ToolResult};

use super::instance::AgentInstance;
use super::prompts::PromptConfig;
use super::runner::turn_loop;
use super::runner::types::TurnEvent;
use crate::manifest::{AbilityManifest, Manifest};
use crate::mcp::PlatformToolResolver;
use crate::types::TaskType;

/// Tool prefix for ability tools.
pub const ABILITY_TOOL_PREFIX: &str = "ability/";

/// A tool bound to a specific ability. One instance per assigned ability.
///
/// The ability inherits the caller's identity (system prompt, model, memory)
/// but uses the ability's own developer prompt and scoped tools. When a
/// platform resolver is available, the ability's `platform_scopes` are used
/// to resolve additional scope-gated tools for the sub-execution.
pub struct AbilityTool {
    ability: AbilityManifest,
    tool_name: String,
    instance: Arc<AgentInstance>,
    manifest: Arc<Manifest>,
    platform_resolver: Option<Arc<dyn PlatformToolResolver>>,
}

impl AbilityTool {
    pub fn new(
        ability: AbilityManifest,
        instance: Arc<AgentInstance>,
        manifest: Arc<Manifest>,
        platform_resolver: Option<Arc<dyn PlatformToolResolver>>,
    ) -> Self {
        let tool_name = ability_tool_name(&ability);
        Self {
            ability,
            tool_name,
            instance,
            manifest,
            platform_resolver,
        }
    }
}

pub fn ability_tool_name(ability: &AbilityManifest) -> String {
    let dotted = if ability.path.is_empty() {
        ability.name.clone()
    } else {
        format!("{}.{}", ability.path.replace('/', "."), ability.name)
    };
    format!("{ABILITY_TOOL_PREFIX}{dotted}")
}

#[async_trait::async_trait]
impl Tool for AbilityTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.ability.activation_condition
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task or question for this ability to handle"
                }
            },
            "required": ["task"]
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

        debug!(
            ability = self.ability.name,
            agent = self.instance.name,
            "Activating ability"
        );

        // Resolve platform tools for the ability's scopes.
        let ability_tools = if !self.ability.platform_scopes.is_empty() {
            if let Some(ref resolver) = self.platform_resolver {
                resolver.resolve_tools(&self.ability.platform_scopes).await
            } else {
                debug!(
                    ability = self.ability.name,
                    "No platform resolver — ability scopes will not resolve to tools"
                );
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Build the sub-execution instance.
        let sub_instance =
            build_ability_instance(&self.instance, &self.ability, &self.manifest, ability_tools);

        let caller_history_snapshot = turn_loop::current_chat_history().unwrap_or_default();
        let task = TaskType::Chat {
            user_message: task_description.to_string(),
            history: vec![],
            project_id: uuid::Uuid::nil(),
        };
        if let Some(parent_tx) = turn_loop::current_events_tx() {
            debug!(
                ability = self.ability.name,
                ability_tool_name = self.tool_name,
                "Emitting AbilityStarted"
            );
            let _ = parent_tx.send(TurnEvent::AbilityStarted {
                ability_tool_name: self.tool_name.clone(),
                ability_name: self.ability.name.clone(),
                task_input: task_description.to_string(),
                caller_history: caller_history_snapshot,
            });
        }

        let prompts = sub_instance.build_prompts(&task);

        let tool_names: Vec<&str> = sub_instance.tools.iter().map(|t| t.name()).collect();
        debug!(
            ability = self.ability.name,
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
            ability = self.ability.name,
            user_message = %user_message,
            "Ability sub-agent user message"
        );
        messages.push(nenjo_models::ChatMessage::user(&user_message));

        let parent_events_tx = turn_loop::current_events_tx();
        let (nested_tx, mut nested_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let ability_tool_name = self.tool_name.clone();
        let bridge = parent_events_tx.map(|parent_tx| {
            let ability_tool_name = ability_tool_name.clone();
            tokio::spawn(async move {
                while let Some(event) = nested_rx.recv().await {
                    match event {
                        TurnEvent::AbilityStarted { .. } => {
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
                        TurnEvent::MessageCompacted { .. } => {
                            let _ = parent_tx.send(event);
                        }
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
                if let Some(parent_tx) = turn_loop::current_events_tx() {
                    debug!(
                        ability = self.ability.name,
                        ability_tool_name = self.tool_name,
                        "Emitting AbilityCompleted success=true"
                    );
                    let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                        ability_tool_name: self.tool_name.clone(),
                        ability_name: self.ability.name.clone(),
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
                        ability = self.ability.name,
                        ability_tool_name = self.tool_name,
                        "Emitting AbilityCompleted success=false"
                    );
                    let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                        ability_tool_name: self.tool_name.clone(),
                        ability_name: self.ability.name.clone(),
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

/// Build a temporary AgentInstance for the ability sub-execution.
///
/// Resolves the ability's `mcp_server_ids` and `platform_scopes`
/// from the manifest and merges them into the sub-instance's prompt context.
/// The `ability_tools` are platform tools resolved from the ability's scopes
/// via the `PlatformToolResolver` — they are merged into the parent's base
/// tools (deduped by name).
fn build_ability_instance(
    caller: &AgentInstance,
    ability: &AbilityManifest,
    manifest: &Manifest,
    ability_tools: Vec<Arc<dyn Tool>>,
) -> AgentInstance {
    // Inherit system prompt, override developer prompt.
    let prompt_config = PromptConfig {
        system_prompt: caller.prompt_config.system_prompt.clone(),
        developer_prompt: ability.prompt.clone(),
        templates: Default::default(),
        memory_profile: caller.prompt_config.memory_profile.clone(),
    };

    // Inherit the parent's base tools (file_edit, shell, etc.) but strip
    // platform MCP tools — the ability's own scopes determine which platform
    // tools are available. Also remove ability tools to prevent recursion.
    let mut tools: Vec<Arc<dyn Tool>> = caller
        .tools
        .iter()
        .filter(|t| {
            !t.name().starts_with(ABILITY_TOOL_PREFIX)
                && !t.name().starts_with("app.nenjo.platform/")
        })
        .cloned()
        .collect();

    // Add the ability's scope-resolved platform tools.
    tools.extend(ability_tools);

    // Build a prompt context without abilities (no recursion).
    let mut prompt_context = caller.prompt_context.clone();
    prompt_context.available_abilities = vec![];
    prompt_context.agent_name = format!("{}:{}", caller.name, ability.name);
    prompt_context.append_active_domain_addon = false;

    // Merge the ability's platform_scopes into the sub-instance so scope-gated
    // tools and MCP integration rendering see the expanded set.
    for scope in &ability.platform_scopes {
        if !prompt_context.platform_scopes.contains(scope) {
            prompt_context.platform_scopes.push(scope.clone());
        }
    }

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
    use crate::context::ContextRenderer;
    use crate::types::{ActiveDomain, DomainPromptConfig, DomainSessionManifest};
    use anyhow::Result;
    use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider};
    use nenjo_tools::security::SecurityPolicy;

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
                    is_system: false,
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
                    manifest: DomainSessionManifest {
                        prompt: DomainPromptConfig {
                            system_addon: Some("domain addon".into()),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    turn_number: 0,
                    artifact_draft: serde_json::json!({}),
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
            memory_vars: Default::default(),
            resource_vars: Default::default(),
            documents_xml: String::new(),
        }
    }

    #[test]
    fn ability_sub_instance_uses_ability_prompt_without_domain_addon() {
        let caller = test_instance_with_active_domain();
        let ability = AbilityManifest {
            id: uuid::Uuid::new_v4(),
            name: "agent_builder".into(),
            path: "nenjo/platform".into(),
            display_name: Some("Agent Builder".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt: "ability developer".into(),
            platform_scopes: vec!["agents:write".into()],
            mcp_server_ids: vec![],
            tool_filter: serde_json::json!({}),
            is_system: true,
        };

        let sub_instance = build_ability_instance(&caller, &ability, &Manifest::default(), vec![]);
        let prompts = sub_instance.build_prompts(&TaskType::Chat {
            user_message: "build an agent".into(),
            history: vec![],
            project_id: uuid::Uuid::nil(),
        });

        assert_eq!(prompts.system, "caller system");
        assert_eq!(prompts.developer, "ability developer");
        assert!(!prompts.developer.contains("domain addon"));
        assert!(!sub_instance.prompt_context.append_active_domain_addon);
    }
}
