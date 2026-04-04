//! UseAbilityTool — runs a sub-execution with an ability's prompt and tool scope.

use std::sync::Arc;

use anyhow::Result;
use tracing::debug;

use nenjo_tools::{Tool, ToolCategory, ToolResult};

use super::instance::AgentInstance;
use super::prompts::PromptConfig;
use super::runner::turn_loop;
use crate::manifest::{AbilityManifest, Manifest};
use crate::mcp::PlatformToolResolver;
use crate::types::TaskType;

/// Tool that executes a named ability as a sub-agent turn loop.
///
/// The ability inherits the caller's identity (system prompt, model, memory)
/// but uses the ability's own developer prompt and scoped tools. When a
/// platform resolver is available, the ability's `platform_scopes` are used
/// to resolve additional scope-gated tools for the sub-execution.
pub struct UseAbilityTool {
    instance: Arc<AgentInstance>,
    manifest: Arc<Manifest>,
    platform_resolver: Option<Arc<dyn PlatformToolResolver>>,
}

impl UseAbilityTool {
    pub fn new(
        instance: Arc<AgentInstance>,
        manifest: Arc<Manifest>,
        platform_resolver: Option<Arc<dyn PlatformToolResolver>>,
    ) -> Self {
        Self {
            instance,
            manifest,
            platform_resolver,
        }
    }
}

#[async_trait::async_trait]
impl Tool for UseAbilityTool {
    fn name(&self) -> &str {
        "use_ability"
    }

    fn description(&self) -> &str {
        "Activate a named ability to handle a specialized task. \
         The ability runs with its own instructions and scoped tools."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ability_name": {
                    "type": "string",
                    "description": "Name of the ability to activate"
                },
                "task": {
                    "type": "string",
                    "description": "The task or question for the ability to handle"
                }
            },
            "required": ["ability_name", "task"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let ability_name = match args["ability_name"].as_str() {
            Some(name) if !name.is_empty() => name,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("ability_name is required".into()),
                });
            }
        };

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

        // Look up the ability.
        let ability = match self
            .instance
            .prompt_context
            .available_abilities
            .iter()
            .find(|a| a.name == ability_name)
        {
            Some(a) => a,
            None => {
                let available: Vec<&str> = self
                    .instance
                    .prompt_context
                    .available_abilities
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "ability '{ability_name}' not found. Available: {available:?}"
                    )),
                });
            }
        };

        debug!(
            ability = ability_name,
            agent = self.instance.name,
            "Activating ability"
        );

        // Resolve platform tools for the ability's scopes.
        let ability_tools = if !ability.platform_scopes.is_empty() {
            if let Some(ref resolver) = self.platform_resolver {
                resolver.resolve_tools(&ability.platform_scopes).await
            } else {
                debug!(
                    ability = ability_name,
                    "No platform resolver — ability scopes will not resolve to tools"
                );
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Build the sub-execution instance.
        let sub_instance =
            build_ability_instance(&self.instance, ability, &self.manifest, ability_tools);

        let task = TaskType::Chat {
            user_message: task_description.to_string(),
            history: vec![],
            project_id: uuid::Uuid::nil(),
        };

        let prompts = sub_instance.build_prompts(&task);

        let tool_names: Vec<&str> = sub_instance.tools.iter().map(|t| t.name()).collect();
        debug!(
            ability = ability_name,
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

        let user_message = if prompts.user_message.is_empty() {
            task_description.to_string()
        } else {
            prompts.user_message
        };
        debug!(
            ability = ability_name,
            user_message = %user_message,
            "Ability sub-agent user message"
        );
        messages.push(nenjo_models::ChatMessage::user(&user_message));

        // Run the sub turn loop (no events — nested execution).
        match turn_loop::run(&sub_instance, messages, None, None).await {
            Ok(output) => Ok(ToolResult {
                success: true,
                output: output.text,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("ability execution failed: {e}")),
            }),
        }
    }
}

/// Build a temporary AgentInstance for the ability sub-execution.
///
/// Resolves the ability's `skill_ids`, `mcp_server_ids`, and `platform_scopes`
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
    // tools are available. Also remove use_ability to prevent recursion.
    let mut tools: Vec<Arc<dyn Tool>> = caller
        .tools
        .iter()
        .filter(|t| t.name() != "use_ability" && !t.name().starts_with("app.nenjo.platform/"))
        .cloned()
        .collect();

    // Add the ability's scope-resolved platform tools.
    tools.extend(ability_tools);

    // Build a prompt context without abilities (no recursion).
    let mut prompt_context = caller.prompt_context.clone();
    prompt_context.available_abilities = vec![];
    prompt_context.agent_name = format!("{}:{}", caller.name, ability.name);

    // Merge the ability's platform_scopes into the sub-instance so scope-gated
    // tools and MCP integration rendering see the expanded set.
    for scope in &ability.platform_scopes {
        if !prompt_context.platform_scopes.contains(scope) {
            prompt_context.platform_scopes.push(scope.clone());
        }
    }

    // Resolve and merge the ability's skills from the manifest.
    for skill in manifest
        .skills
        .iter()
        .filter(|s| ability.skill_ids.contains(&s.id))
    {
        if !prompt_context.skills.iter().any(|s| s.id == skill.id) {
            prompt_context.skills.push(skill.clone());
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
