//! UseAbilityTool — runs a sub-execution with an ability's prompt and tool scope.

use std::sync::Arc;

use anyhow::Result;
use tracing::debug;

use nenjo_tools::{Tool, ToolCategory, ToolResult};

use super::instance::AgentInstance;
use super::prompts::PromptConfig;
use super::runner::turn_loop;
use crate::manifest::AbilityManifest;
use crate::types::TaskType;

/// Tool that executes a named ability as a sub-agent turn loop.
///
/// The ability inherits the caller's identity (system prompt, model, memory)
/// but uses the ability's own developer prompt and scoped tools.
pub struct UseAbilityTool {
    instance: Arc<AgentInstance>,
}

impl UseAbilityTool {
    pub fn new(instance: Arc<AgentInstance>) -> Self {
        Self { instance }
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

        // Build the sub-execution instance.
        let sub_instance = build_ability_instance(&self.instance, ability);

        let task = TaskType::Chat {
            user_message: task_description.to_string(),
            history: vec![],
            project_id: uuid::Uuid::nil(),
        };

        let prompts = sub_instance.build_prompts(&task);

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
fn build_ability_instance(caller: &AgentInstance, ability: &AbilityManifest) -> AgentInstance {
    // Inherit system prompt, override developer prompt.
    let prompt_config = PromptConfig {
        system_prompt: caller.prompt_config.system_prompt.clone(),
        developer_prompt: ability.prompt.clone(),
        templates: Default::default(),
        memory_profile: caller.prompt_config.memory_profile.clone(),
    };

    // Abilities are additive — inherit all parent tools, just remove
    // use_ability to prevent recursion.
    let tools: Vec<Arc<dyn Tool>> = caller
        .tools
        .iter()
        .filter(|t| t.name() != "use_ability")
        .cloned()
        .collect();

    // Build a prompt context without abilities (no recursion).
    let mut prompt_context = caller.prompt_context.clone();
    prompt_context.available_abilities = vec![];
    prompt_context.agent_name = format!("{}:{}", caller.name, ability.name);

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
        memory_xml: caller.memory_xml.clone(),
        documents_xml: caller.documents_xml.clone(),
    }
}
