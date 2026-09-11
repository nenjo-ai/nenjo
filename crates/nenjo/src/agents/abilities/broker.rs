//! Discover and invoke assigned abilities through a stable tool surface.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::agents::instance::AgentInstance;
use crate::manifest::AbilityManifest;
use crate::provider::{ErasedProvider, ProviderRuntime};
use crate::tools::{Tool, ToolCategory, ToolOrigin, ToolResult};

use super::execution::start_ability_operation;
use super::registry::AbilityRegistry;
use super::{LIST_ASSIGNED_ABILITIES_TOOL_NAME, USE_ABILITY_TOOL_NAME};

#[derive(Debug, Serialize)]
struct AbilityListItem<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    activation_condition: &'a str,
}

/// Discover abilities assigned to the current agent.
pub struct ListAssignedAbilitiesTool {
    registry: Arc<AbilityRegistry>,
}

impl ListAssignedAbilitiesTool {
    /// Share the same validated assignment registry with the invocation tool.
    pub(super) fn new(registry: Arc<AbilityRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl Tool for ListAssignedAbilitiesTool {
    fn name(&self) -> &str {
        LIST_ASSIGNED_ABILITIES_TOOL_NAME
    }

    fn description(&self) -> &str {
        "List abilities assigned to this agent. Use this before invoking an ability when the task may require a specialized capability beyond the base tools."
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

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    /// Return every assigned ability in manifest order using the stable wire shape.
    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        let abilities: Vec<_> = self
            .registry
            .iter()
            .map(|ability| AbilityListItem {
                name: &ability.name,
                description: ability.description.as_deref(),
                activation_condition: &ability.activation_condition,
            })
            .collect();
        let output = serde_json::to_string(&serde_json::json!({ "abilities": abilities }))
            .context("failed to serialize ability list")?;
        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

/// Invoke one assigned ability by the name returned by discovery.
pub struct UseAbilityTool<P: ProviderRuntime = ErasedProvider> {
    registry: Arc<AbilityRegistry>,
    instance: Arc<AgentInstance<P>>,
}

impl<P: ProviderRuntime> UseAbilityTool<P> {
    /// Bind invocation to this caller's runtime and validated assignments.
    pub(super) fn new(registry: Arc<AbilityRegistry>, instance: Arc<AgentInstance<P>>) -> Self {
        Self { registry, instance }
    }
}

#[async_trait::async_trait]
impl<P> Tool for UseAbilityTool<P>
where
    P: ProviderRuntime,
{
    fn name(&self) -> &str {
        USE_ABILITY_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Invoke one ability assigned to this agent by name. Use list_assigned_abilities to discover available ability names before calling this tool."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The stable ability name returned by list_assigned_abilities."
                },
                "input": {
                    "type": "string",
                    "description": "A self-contained delegated task. Include all relevant user-provided context, code snippets, files, constraints, and expected output so the ability can complete the task without access to the caller conversation."
                },
                "reason": {
                    "type": "string",
                    "description": "Why this ability is appropriate for the task."
                }
            },
            "required": ["name", "input"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    /// Validate the task/name (including the legacy `ability_id` alias) before starting work.
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let ability_name = match args["name"]
            .as_str()
            .or_else(|| args["ability_id"].as_str())
        {
            Some(id) if !id.trim().is_empty() => id.trim(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some("ability name is required".into()),
                });
            }
        };
        let task_description = match args["input"].as_str() {
            Some(t) if !t.is_empty() => t,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some("input is required".into()),
                });
            }
        };
        let Some(ability) = self.registry.get(ability_name) else {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(format!("unknown ability '{ability_name}'")),
            });
        };
        start_ability_operation(&self.instance, ability, ability_name, task_description).await
    }
}

/// Build the fixed broker pair, rejecting duplicate assigned ability names.
pub(crate) fn build_ability_tools<P>(
    abilities: &[AbilityManifest],
    instance: Arc<AgentInstance<P>>,
) -> Result<Vec<Arc<dyn Tool>>>
where
    P: ProviderRuntime,
{
    let registry = Arc::new(AbilityRegistry::new(abilities)?);
    Ok(vec![
        Arc::new(ListAssignedAbilitiesTool::new(registry.clone())) as Arc<dyn Tool>,
        Arc::new(UseAbilityTool::new(registry, instance)) as Arc<dyn Tool>,
    ])
}

/// Recognize only the broker tools when rebuilding an agent's ability assignments.
pub(crate) fn is_ability_tool(name: &str) -> bool {
    matches!(
        name,
        LIST_ASSIGNED_ABILITIES_TOOL_NAME | USE_ABILITY_TOOL_NAME
    )
}
