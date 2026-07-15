//! Ability invocation tools.
//!
//! Assigned abilities are exposed through a stable broker pair:
//! `list_assigned_abilities` discovers available abilities and `use_ability` invokes
//! one by its model-facing ability id.

use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, bail};
use nenjo_models::ModelProvider;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::debug;

use crate::tools::{
    INSPECT_OPERATIONS_TOOL_NAME, InspectOperationsArgs, SEND_OPERATION_INPUT_TOOL_NAME,
    STOP_OPERATIONS_TOOL_NAME, SendOperationInputArgs, StopOperationsArgs, Tool, ToolCategory,
    ToolOrigin, ToolResult, WAIT_OPERATIONS_TOOL_NAME, WaitOperationsArgs,
    deserialize_u64_from_json_number, deserialize_usize_from_json_number,
    inspect_operations_parameters_schema, send_operation_input_parameters_schema,
    stop_operations_parameters_schema, wait_operations_parameters_schema,
};

use super::async_ops::{
    AsyncOpChildHandle, AsyncOpId, AsyncOpKind, AsyncOpManager, AsyncOpSignal, AsyncOpWaitFilter,
    StartAsyncOp, truncate,
};
use super::delegation::DELEGATE_TO_TOOL_NAME;
use super::instance::{AgentInstance, AgentPromptState, AgentRuntime};
use super::runner::types::{AsyncOperationTranscriptEvent, TurnEvent};
use super::runner::{build_instruction_messages, turn_loop};
use crate::input::{AgentRun, ChatInput};
use crate::manifest::{AbilityManifest, PromptConfig, PromptTemplates};
use crate::provider::{ErasedProvider, ProviderRuntime, ToolFactory};

pub const LIST_ASSIGNED_ABILITIES_TOOL_NAME: &str = "list_assigned_abilities";
pub const USE_ABILITY_TOOL_NAME: &str = "use_ability";
pub const INSPECT_ABILITIES_TOOL_NAME: &str = "inspect_abilities";
pub const SEND_ABILITIES_TOOL_NAME: &str = "send_abilities";
pub const STOP_ABILITIES_TOOL_NAME: &str = "stop_abilities";
pub const WAIT_TOOL_NAME: &str = "wait";

static ABILITY_OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
struct AbilityEntry {
    ability_id: String,
    ability: AbilityManifest,
}

#[derive(Debug, Clone)]
struct AbilityRegistry {
    entries: Vec<AbilityEntry>,
    by_id: BTreeMap<String, usize>,
}

impl AbilityRegistry {
    fn new(abilities: &[AbilityManifest]) -> Result<Self> {
        let mut entries: Vec<AbilityEntry> = Vec::with_capacity(abilities.len());
        let mut by_id: BTreeMap<String, usize> = BTreeMap::new();
        for ability in abilities {
            let ability_id = ability_id(ability);
            if let Some(existing) = by_id.get(&ability_id) {
                let existing = &entries[*existing].ability;
                bail!(
                    "duplicate ability_id '{ability_id}' for abilities '{}' and '{}'",
                    existing.name,
                    ability.name
                );
            }
            by_id.insert(ability_id.clone(), entries.len());
            entries.push(AbilityEntry {
                ability_id,
                ability: ability.clone(),
            });
        }
        Ok(Self { entries, by_id })
    }

    fn get(&self, ability_id: &str) -> Option<&AbilityManifest> {
        self.by_id
            .get(ability_id)
            .and_then(|index| self.entries.get(*index))
            .map(|entry| &entry.ability)
    }
}

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
    fn new(registry: Arc<AbilityRegistry>) -> Self {
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

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        let abilities: Vec<_> = self
            .registry
            .entries
            .iter()
            .map(|entry| AbilityListItem {
                name: &entry.ability_id,
                description: entry.ability.description.as_deref(),
                activation_condition: &entry.ability.activation_condition,
            })
            .collect();
        let output = serde_json::to_string(&serde_json::json!({ "abilities": abilities }))
            .context("failed to serialize ability list")?;
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

/// Invoke one assigned ability by slug name.
pub struct UseAbilityTool<P: ProviderRuntime = ErasedProvider> {
    registry: Arc<AbilityRegistry>,
    instance: Arc<AgentInstance<P>>,
}

impl<P: ProviderRuntime> UseAbilityTool<P> {
    fn new(registry: Arc<AbilityRegistry>, instance: Arc<AgentInstance<P>>) -> Self {
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

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let ability_name = match args["name"]
            .as_str()
            .or_else(|| args["ability_id"].as_str())
        {
            Some(id) if !id.trim().is_empty() => id.trim(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("ability name is required".into()),
                });
            }
        };
        let task_description = match args["input"].as_str() {
            Some(t) if !t.is_empty() => t,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("input is required".into()),
                });
            }
        };
        let Some(ability) = self.registry.get(ability_name) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("unknown ability '{ability_name}'")),
            });
        };
        start_ability_operation(&self.instance, ability, ability_name, task_description).await
    }
}

struct InspectAbilitiesTool {
    async_ops: AsyncOpManager,
}

struct SendAbilitiesTool {
    async_ops: AsyncOpManager,
}

struct StopAbilitiesTool {
    async_ops: AsyncOpManager,
}

struct InspectOperationsTool {
    async_ops: AsyncOpManager,
}

struct StopOperationsTool {
    async_ops: AsyncOpManager,
}

struct SendOperationInputTool {
    async_ops: AsyncOpManager,
}

struct AbilityWaitTool {
    async_ops: AsyncOpManager,
}

struct WaitOperationsTool {
    async_ops: AsyncOpManager,
}

#[derive(Debug, Deserialize)]
struct InspectAbilitiesArgs {
    #[serde(default)]
    operations: Vec<String>,
    #[serde(default)]
    include_transcript: bool,
    #[serde(
        default = "default_inspect_limit",
        deserialize_with = "deserialize_usize_from_json_number"
    )]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct SendAbilitiesArgs {
    #[serde(default)]
    operations: Vec<String>,
    message: String,
}

#[derive(Debug, Deserialize)]
struct StopAbilitiesArgs {
    #[serde(default)]
    operations: Vec<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WaitArgs {
    #[serde(
        default = "default_wait_seconds",
        deserialize_with = "deserialize_u64_from_json_number"
    )]
    seconds: u64,
    reason: Option<String>,
}

fn default_inspect_limit() -> usize {
    30
}

fn default_wait_seconds() -> u64 {
    10
}

#[async_trait::async_trait]
impl Tool for InspectAbilitiesTool {
    fn name(&self) -> &str {
        INSPECT_ABILITIES_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Inspect running or recently completed ability operations by operation_id. Include transcript deltas when you need evidence or nested tool activity."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operations": {"type": "array", "items": {"type": "string"}},
                "include_transcript": {"type": "boolean"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            },
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: InspectAbilitiesArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!({
            "abilities": self.async_ops.inspect(
                parsed.operations,
                Some(AsyncOpKind::Ability),
                parsed.include_transcript,
                parsed.limit,
            ).await
        })))
    }
}

#[async_trait::async_trait]
impl Tool for SendAbilitiesTool {
    fn name(&self) -> &str {
        SEND_ABILITIES_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Send input to one or more ability operations that asked the parent agent a question."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operations": {"type": "array", "items": {"type": "string"}},
                "message": {"type": "string"}
            },
            "required": ["operations", "message"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: SendAbilitiesArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!({
            "sent": self.async_ops.send_input(parsed.operations, parsed.message).await
        })))
    }
}

#[async_trait::async_trait]
impl Tool for StopAbilitiesTool {
    fn name(&self) -> &str {
        STOP_ABILITIES_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Stop one or more running ability operations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operations": {"type": "array", "items": {"type": "string"}},
                "reason": {"type": "string"}
            },
            "required": ["operations"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: StopAbilitiesArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!({
            "stopped": self.async_ops.stop(
                parsed.operations,
                Some(AsyncOpKind::Ability),
                parsed.reason,
                super::runner::turn_loop::current_events_tx(),
            ).await
        })))
    }
}

#[async_trait::async_trait]
impl Tool for InspectOperationsTool {
    fn name(&self) -> &str {
        INSPECT_OPERATIONS_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Inspect running or recently completed async operations by operation_id. Use this after wait_operations reports completion or failure when you need the final output payload or recent transcript. For media jobs, inspect the existing media operation instead of starting a new generation job for the same user request."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        inspect_operations_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: InspectOperationsArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!({
            "operations": self.async_ops.inspect(
                parsed.operations,
                parsed.kind,
                parsed.include_transcript,
                parsed.limit,
            ).await
        })))
    }
}

#[async_trait::async_trait]
impl Tool for StopOperationsTool {
    fn name(&self) -> &str {
        STOP_OPERATIONS_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Stop one or more running async operations. Filter by kind to avoid stopping unrelated work."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        stop_operations_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: StopOperationsArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!({
            "stopped": self.async_ops.stop(
                parsed.operations,
                parsed.kind,
                parsed.reason,
                super::runner::turn_loop::current_events_tx(),
            ).await
        })))
    }
}

#[async_trait::async_trait]
impl Tool for SendOperationInputTool {
    fn name(&self) -> &str {
        SEND_OPERATION_INPUT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Send input to one or more async operations that asked the parent agent a question."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        send_operation_input_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: SendOperationInputArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!({
            "sent": self.async_ops.send_input(parsed.operations, parsed.message).await
        })))
    }
}

#[async_trait::async_trait]
impl Tool for AbilityWaitTool {
    fn name(&self) -> &str {
        WAIT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Yield briefly while async operations continue running, then return queued operation signals."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "seconds": {"type": "integer", "minimum": 1, "maximum": 30},
                "reason": {"type": "string"}
            },
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: WaitArgs = serde_json::from_value(args)?;
        let _reason = parsed.reason;
        if let Some(turn_input) = super::runner::turn_loop::current_turn_input() {
            let result = tokio::select! {
                result = self.async_ops.wait(parsed.seconds, AsyncOpWaitFilter::all()) => result,
                _ = turn_input.notified() => super::async_ops::AsyncOpWaitResult {
                    elapsed_seconds: 0,
                    woken_by: "user_message",
                    updates: Vec::new(),
                },
            };
            return Ok(json_tool(serde_json::json!(result)));
        }
        Ok(json_tool(serde_json::json!(
            self.async_ops
                .wait(parsed.seconds, AsyncOpWaitFilter::all())
                .await
        )))
    }
}

#[async_trait::async_trait]
impl Tool for WaitOperationsTool {
    fn name(&self) -> &str {
        WAIT_OPERATIONS_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Wait while async operations continue running, then return queued operation signals. For video or other media generation, call this with kind=media after the media tool returns job_started; repeat wait_operations until the media operation completes or fails instead of calling the generation tool again for the same request."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        wait_operations_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: WaitOperationsArgs = serde_json::from_value(args)?;
        let _reason = parsed.reason;
        if let Some(turn_input) = super::runner::turn_loop::current_turn_input() {
            let result = tokio::select! {
                result = self.async_ops.wait(parsed.seconds, AsyncOpWaitFilter::kind(parsed.kind)) => result,
                _ = turn_input.notified() => super::async_ops::AsyncOpWaitResult {
                    elapsed_seconds: 0,
                    woken_by: "user_message",
                    updates: Vec::new(),
                },
            };
            return Ok(json_tool(serde_json::json!(result)));
        }
        Ok(json_tool(serde_json::json!(
            self.async_ops
                .wait(parsed.seconds, AsyncOpWaitFilter::kind(parsed.kind))
                .await
        )))
    }
}

struct UpdateAbilityParentTool {
    handle: AsyncOpChildHandle,
}

struct AskAbilityParentTool {
    handle: AsyncOpChildHandle,
}

#[async_trait::async_trait]
impl Tool for UpdateAbilityParentTool {
    fn name(&self) -> &str {
        "update_parent_agent"
    }

    fn description(&self) -> &str {
        "Send a compact ability progress update to the parent agent without waking it immediately."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "details": {"type": "string"}
            },
            "required": ["summary"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        #[derive(Deserialize)]
        struct Args {
            summary: String,
            details: Option<String>,
        }
        let parsed: Args = serde_json::from_value(args)?;
        if let Some(cancel) = self.handle.cancel_token()
            && cancel.is_cancelled()
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("ability operation was stopped".into()),
            });
        }
        self.handle
            .progress(
                parsed.summary,
                parsed.details,
                super::runner::turn_loop::current_events_tx(),
            )
            .await;
        Ok(json_tool(serde_json::json!({ "status": "delivered" })))
    }
}

#[async_trait::async_trait]
impl Tool for AskAbilityParentTool {
    fn name(&self) -> &str {
        "ask_parent_agent"
    }

    fn description(&self) -> &str {
        "Ask the parent agent for input and wait until it responds with send_abilities."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {"type": "string"},
                "context": {"type": "string"}
            },
            "required": ["question"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        #[derive(Deserialize)]
        struct Args {
            question: String,
            context: Option<String>,
        }
        let parsed: Args = serde_json::from_value(args)?;
        match self
            .handle
            .ask(
                parsed.question,
                parsed.context,
                super::runner::turn_loop::current_events_tx(),
            )
            .await
        {
            Some(message) => Ok(json_tool(serde_json::json!({ "message": message }))),
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("parent did not provide input before the operation ended".into()),
            }),
        }
    }
}

fn ability_child_tools(handle: AsyncOpChildHandle) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(UpdateAbilityParentTool {
            handle: handle.clone(),
        }),
        Arc::new(AskAbilityParentTool { handle }),
    ]
}

async fn start_ability_operation<P>(
    instance: &Arc<AgentInstance<P>>,
    ability: &AbilityManifest,
    ability_id: &str,
    task_description: &str,
) -> Result<ToolResult>
where
    P: ProviderRuntime,
{
    debug!(
        ability = ability.name,
        agent = instance.name(),
        "Activating ability"
    );

    let caller_history_snapshot = turn_loop::current_chat_history().unwrap_or_default();
    let parent_events_tx = turn_loop::current_events_tx();
    let operation_id = next_ability_operation_id(ability_id);
    let started = instance
        .runtime
        .async_ops
        .start(
            StartAsyncOp {
                id: operation_id.clone(),
                kind: AsyncOpKind::Ability,
                label: ability.name.clone(),
                parent_operation_id: None,
                parent_tool_name: Some(USE_ABILITY_TOOL_NAME.into()),
                started_summary: task_description.to_string(),
                model_visible: true,
            },
            parent_events_tx.clone(),
        )
        .await;

    let instance = instance.clone();
    let ability = ability.clone();
    let task_description = task_description.to_string();
    let child_handle = started.child.clone();
    let op_handle = started.handle.clone();
    let join_events_tx = parent_events_tx.clone();
    let join_instance = instance.clone();
    let call_id = operation_id.to_string();
    let join = tokio::spawn(async move {
        run_ability_operation(AbilityOperation {
            instance: join_instance,
            ability,
            call_id,
            task_description,
            caller_history_snapshot,
            child_handle,
            op_handle,
            parent_events_tx: join_events_tx,
        })
        .await;
    });
    instance
        .runtime
        .async_ops
        .attach_join(&operation_id, join)
        .await;

    Ok(json_tool(serde_json::json!({
        "ability": ability_id,
        "operation_id": operation_id.to_string(),
        "status": "running",
        "control_tools": {
            "inspect": INSPECT_ABILITIES_TOOL_NAME,
            "send_input": SEND_ABILITIES_TOOL_NAME,
            "stop": STOP_ABILITIES_TOOL_NAME,
            "wait": ability_wait_tool_name(&instance.runtime)
        }
    })))
}

struct AbilityOperation<P: ProviderRuntime> {
    instance: Arc<AgentInstance<P>>,
    ability: AbilityManifest,
    call_id: String,
    task_description: String,
    caller_history_snapshot: Vec<nenjo_models::ChatMessage>,
    child_handle: AsyncOpChildHandle,
    op_handle: super::async_ops::AsyncOpHandle,
    parent_events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
}

async fn run_ability_operation<P>(operation: AbilityOperation<P>)
where
    P: ProviderRuntime,
{
    let AbilityOperation {
        instance,
        ability,
        call_id,
        task_description,
        caller_history_snapshot,
        child_handle,
        op_handle,
        parent_events_tx,
    } = operation;

    let mut sub_instance = build_ability_instance(&instance, &ability).await;
    let cancel_token = op_handle.cancel_token();
    sub_instance.runtime.execution_cancel = cancel_token.clone();
    sub_instance.runtime.async_ops = AsyncOpManager::with_cancel(cancel_token.clone());
    sub_instance
        .runtime
        .tools
        .extend(ability_child_tools(child_handle.clone()));

    let task = AgentRun::chat(ChatInput {
        message: task_description.clone(),
        history: vec![],
        project: None,
        template_override: None,
    });
    if let Some(parent_tx) = parent_events_tx.clone() {
        debug!(
            ability = ability.name,
            ability_tool_name = USE_ABILITY_TOOL_NAME,
            "Emitting AbilityStarted"
        );
        let _ = parent_tx.send(TurnEvent::AbilityStarted {
            call_id: call_id.clone(),
            ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
            ability_name: ability.name.clone(),
            task_input: task_description.clone(),
            caller_history: caller_history_snapshot,
        });
    }

    let prompts = match sub_instance.build_prompts(&task) {
        Ok(prompts) => prompts,
        Err(error) => {
            let error = format!("ability prompt build failed: {error}");
            op_handle
                .complete(
                    AsyncOpSignal::Failed {
                        error: truncate(&error, 500),
                    },
                    parent_events_tx.clone(),
                )
                .await;
            if let Some(parent_tx) = parent_events_tx {
                debug!(
                    ability = ability.name,
                    ability_tool_name = USE_ABILITY_TOOL_NAME,
                    "Emitting AbilityCompleted success=false"
                );
                let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                    call_id,
                    ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
                    ability_name: ability.name.clone(),
                    success: false,
                    final_output: error,
                });
            }
            return;
        }
    };

    let tool_names: Vec<&str> = sub_instance
        .runtime
        .tools
        .iter()
        .map(|t| t.name())
        .collect();
    debug!(
        ability = ability.name,
        agent = instance.name(),
        tool_count = sub_instance.runtime.tools.len(),
        tools = ?tool_names,
        "Ability sub-agent prompt"
    );
    debug!("{prompts}");

    // Build messages for the sub-execution.
    let supports_developer_role = sub_instance
        .model
        .model_provider
        .supports_developer_role(&sub_instance.model.model_name);
    let mut messages =
        build_instruction_messages(&prompts.system, &prompts.developer, supports_developer_role);

    if let crate::input::AgentRunKind::Chat(chat) = &task.kind {
        messages.extend(chat.history.iter().cloned());
    }

    let user_message = if prompts.user_message.is_empty() {
        task_description.clone()
    } else {
        prompts.user_message
    };
    debug!(
        ability = ability.name,
        user_message = %user_message,
        "Ability sub-agent user message"
    );
    messages.push(nenjo_models::ChatMessage::user(&user_message));

    let (nested_tx, mut nested_rx) = mpsc::unbounded_channel::<TurnEvent>();
    let bridge_op_handle = op_handle.clone();
    let bridge_events_tx = parent_events_tx.clone();
    let bridge_call_id = call_id.clone();
    let bridge = parent_events_tx.clone().map(|parent_tx| {
        tokio::spawn(async move {
            while let Some(event) = nested_rx.recv().await {
                bridge_ability_transcript(&bridge_op_handle, &event, bridge_events_tx.clone())
                    .await;
                match event {
                    TurnEvent::AbilityStarted { .. } => {
                        let _ = parent_tx.send(event);
                    }
                    TurnEvent::ToolCallStart {
                        batch_id,
                        parent_tool_name,
                        calls,
                    } => {
                        let _ = parent_tx.send(TurnEvent::ToolCallStart {
                            batch_id,
                            parent_tool_name: parent_tool_name
                                .or_else(|| Some(bridge_call_id.clone())),
                            calls,
                        });
                    }
                    TurnEvent::ToolCallEnd {
                        batch_id,
                        parent_tool_name,
                        tool_call_id,
                        tool_name,
                        tool_args,
                        result,
                        metadata,
                    } => {
                        let _ = parent_tx.send(TurnEvent::ToolCallEnd {
                            batch_id,
                            parent_tool_name: parent_tool_name
                                .or_else(|| Some(bridge_call_id.clone())),
                            tool_call_id,
                            tool_name,
                            tool_args,
                            result,
                            metadata,
                        });
                    }
                    TurnEvent::AbilityCompleted { .. } => {
                        let _ = parent_tx.send(event);
                    }
                    TurnEvent::MessageCompacted { .. } => {
                        let _ = parent_tx.send(event);
                    }
                    TurnEvent::ModelRequestStarted {
                        request_id,
                        parent_call_id,
                        provider,
                        model,
                    } => {
                        let _ = parent_tx.send(TurnEvent::ModelRequestStarted {
                            request_id,
                            parent_call_id: parent_call_id.or_else(|| Some(bridge_call_id.clone())),
                            provider,
                            model,
                        });
                    }
                    TurnEvent::AssistantTextDelta { .. } => {
                        let _ = parent_tx.send(event);
                    }
                    TurnEvent::AssistantResponse { .. } => {
                        let _ = parent_tx.send(event);
                    }
                    TurnEvent::ModelRequestCompleted {
                        request_id,
                        parent_call_id,
                    } => {
                        let _ = parent_tx.send(TurnEvent::ModelRequestCompleted {
                            request_id,
                            parent_call_id: parent_call_id.or_else(|| Some(bridge_call_id.clone())),
                        });
                    }
                    TurnEvent::AsyncOperationEvent { .. }
                    | TurnEvent::AsyncOperationTranscript { .. } => {
                        let _ = parent_tx.send(event);
                    }
                    TurnEvent::TranscriptMessage { .. } => {}
                    _ => {}
                }
            }
        })
    });

    // Run the sub turn loop with nested events enabled.
    let result = tokio::select! {
        _ = cancel_token.cancelled() => {
            Err(anyhow::anyhow!("ability operation stopped"))
        }
        result = turn_loop::run(&sub_instance, messages, Some(nested_tx), None, None, false) => result,
    };

    if let Some(bridge) = bridge {
        let _ = bridge.await;
    }

    match result {
        Ok(output) => {
            turn_loop::record_nested_token_usage(output.input_tokens, output.output_tokens);
            op_handle
                .complete(
                    AsyncOpSignal::Completed {
                        summary: truncate(&output.text, 500),
                        output: Some(serde_json::json!({
                            "result_preview": output.text,
                        })),
                    },
                    parent_events_tx.clone(),
                )
                .await;
            if let Some(parent_tx) = parent_events_tx.clone() {
                debug!(
                    ability = ability.name,
                    ability_tool_name = USE_ABILITY_TOOL_NAME,
                    "Emitting AbilityCompleted success=true"
                );
                let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                    call_id: call_id.clone(),
                    ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
                    ability_name: ability.name.clone(),
                    success: true,
                    final_output: output.text.clone(),
                });
            }
        }
        Err(e) => {
            let error = format!("ability execution failed: {e}");
            op_handle
                .complete(
                    AsyncOpSignal::Failed {
                        error: truncate(&error, 500),
                    },
                    parent_events_tx.clone(),
                )
                .await;
            if let Some(parent_tx) = parent_events_tx {
                debug!(
                    ability = ability.name,
                    ability_tool_name = USE_ABILITY_TOOL_NAME,
                    "Emitting AbilityCompleted success=false"
                );
                let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                    call_id,
                    ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
                    ability_name: ability.name.clone(),
                    success: false,
                    final_output: error.clone(),
                });
            }
        }
    }
}

async fn bridge_ability_transcript(
    handle: &super::async_ops::AsyncOpHandle,
    event: &TurnEvent,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
) {
    match event {
        TurnEvent::ToolCallStart { calls, .. } => {
            for call in calls {
                handle
                    .transcript(
                        AsyncOperationTranscriptEvent::ToolCall {
                            tool: call.tool_name.clone(),
                            summary: call
                                .text_preview
                                .clone()
                                .unwrap_or_else(|| truncate(&call.tool_args, 240)),
                        },
                        events_tx.clone(),
                    )
                    .await;
            }
        }
        TurnEvent::ToolCallEnd {
            tool_name, result, ..
        } => {
            handle
                .transcript(
                    AsyncOperationTranscriptEvent::ToolResult {
                        tool: tool_name.clone(),
                        success: result.success,
                        summary: truncate(
                            result.error.as_deref().unwrap_or(result.output.as_str()),
                            240,
                        ),
                    },
                    events_tx,
                )
                .await;
        }
        TurnEvent::TranscriptMessage { message } => {
            let summary = truncate(&message.content, 240);
            let transcript = match message.role.as_str() {
                "user" => AsyncOperationTranscriptEvent::Input { summary },
                "assistant" => AsyncOperationTranscriptEvent::AssistantMessage { summary },
                "tool" => AsyncOperationTranscriptEvent::ToolResult {
                    tool: "tool".into(),
                    success: true,
                    summary,
                },
                _ => return,
            };
            handle.transcript(transcript, events_tx).await;
        }
        TurnEvent::AbilityStarted { .. }
        | TurnEvent::AbilityCompleted { .. }
        | TurnEvent::ModelRequestStarted { .. }
        | TurnEvent::AssistantTextDelta { .. }
        | TurnEvent::AssistantResponse { .. }
        | TurnEvent::ModelRequestCompleted { .. }
        | TurnEvent::HookStarted { .. }
        | TurnEvent::HookActivated { .. }
        | TurnEvent::HookCompleted { .. }
        | TurnEvent::SubAgentEvent { .. }
        | TurnEvent::SubAgentTranscript { .. }
        | TurnEvent::AsyncOperationEvent { .. }
        | TurnEvent::AsyncOperationTranscript { .. }
        | TurnEvent::MessageCompacted { .. }
        | TurnEvent::Paused
        | TurnEvent::Resumed
        | TurnEvent::Done { .. } => {}
    }
}

pub(crate) fn build_ability_tools<P>(
    abilities: &[AbilityManifest],
    instance: Arc<AgentInstance<P>>,
) -> Result<Vec<Arc<dyn Tool>>>
where
    P: ProviderRuntime,
{
    let registry = Arc::new(AbilityRegistry::new(abilities)?);
    let async_ops = instance.runtime.async_ops.clone();
    let mut tools = vec![
        Arc::new(ListAssignedAbilitiesTool::new(registry.clone())) as Arc<dyn Tool>,
        Arc::new(UseAbilityTool::new(registry, instance.clone())) as Arc<dyn Tool>,
        Arc::new(InspectAbilitiesTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(SendAbilitiesTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(StopAbilitiesTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
    ];
    if instance.runtime.config.max_delegation_depth == 0 {
        tools.push(Arc::new(AbilityWaitTool { async_ops }) as Arc<dyn Tool>);
    } else if !instance.runtime.execution_mode.can_orchestrate() {
        tools.push(Arc::new(AbilityWaitTool { async_ops }) as Arc<dyn Tool>);
    }
    Ok(tools)
}

fn ability_wait_tool_name<P>(runtime: &AgentRuntime<P>) -> &'static str
where
    P: ProviderRuntime,
{
    if runtime.execution_mode.can_orchestrate() {
        WAIT_OPERATIONS_TOOL_NAME
    } else {
        WAIT_TOOL_NAME
    }
}

pub(crate) fn build_async_operation_tools(async_ops: AsyncOpManager) -> Vec<Arc<dyn Tool>> {
    let mut tools = vec![
        Arc::new(InspectOperationsTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(SendOperationInputTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(StopOperationsTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
    ];
    tools.push(Arc::new(WaitOperationsTool { async_ops }) as Arc<dyn Tool>);
    tools
}

pub(crate) fn is_ability_tool(name: &str) -> bool {
    matches!(
        name,
        LIST_ASSIGNED_ABILITIES_TOOL_NAME
            | USE_ABILITY_TOOL_NAME
            | INSPECT_ABILITIES_TOOL_NAME
            | SEND_ABILITIES_TOOL_NAME
            | STOP_ABILITIES_TOOL_NAME
            | WAIT_TOOL_NAME
    )
}

fn ability_id(ability: &AbilityManifest) -> String {
    ability.name.clone()
}

fn next_ability_operation_id(ability_id: &str) -> AsyncOpId {
    let slug = crate::Slug::derive_with_fallback(ability_id, "ability");
    let sequence = ABILITY_OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    AsyncOpId::new(format!("ability_{slug}_{sequence}"))
}

fn json_tool(value: serde_json::Value) -> ToolResult {
    ToolResult {
        success: true,
        output: value.to_string(),
        error: None,
    }
}

/// Build a temporary AgentInstance for the ability sub-execution.
///
async fn build_ability_instance<P>(
    caller: &AgentInstance<P>,
    ability: &AbilityManifest,
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
        .filter(|tool| tool.origin() == ToolOrigin::Host && tool.name() != DELEGATE_TO_TOOL_NAME)
        .cloned()
        .collect();

    let mut ability_scopes = Vec::new();
    for scope in &ability.platform_scopes {
        if !ability_scopes.contains(scope) {
            ability_scopes.push(scope.clone());
        }
    }

    let mut merged_mcp_servers = Vec::new();
    for server_id in &ability.mcp_servers {
        if !merged_mcp_servers.contains(server_id) {
            merged_mcp_servers.push(server_id.clone());
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
        scoped_agent.platform_scopes = ability_scopes.clone();
        scoped_agent.mcp_servers = merged_mcp_servers.clone();
        scoped_agent.media = ability.media.clone();
        scoped_agent.abilities.clear();
        scoped_agent.domains.clear();
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

    // Build a prompt context without ability recursion.
    let mut prompt_context = caller.prompt.context.clone();
    prompt_context.agent_name = format!("{}:{}", caller.name(), ability.name);
    prompt_context.active_domain = None;
    prompt_context.append_active_domain_addon = false;

    let mut scoped_manifest = caller.manifest.clone();
    scoped_manifest.name = format!("{}:{}", caller.name(), ability.name);
    scoped_manifest.description = Some(
        ability
            .description
            .clone()
            .unwrap_or_else(|| caller.description().to_string()),
    );
    scoped_manifest.prompt_config = prompt_config;
    scoped_manifest.platform_scopes = ability_scopes;
    scoped_manifest.mcp_servers = merged_mcp_servers;
    scoped_manifest.media = ability.media.clone();
    scoped_manifest.abilities.clear();
    scoped_manifest.domains.clear();

    let execution_cancel = caller.runtime.execution_cancel.child_token();
    let async_ops = AsyncOpManager::with_cancel(execution_cancel.clone());

    AgentInstance {
        manifest: scoped_manifest,
        model_manifest: caller.model_manifest.clone(),
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
            sub_agent_ctx: caller.runtime.sub_agent_ctx.clone(),
            async_ops,
            execution_cancel,
            execution_mode: caller.runtime.execution_mode,
            hook_runtime: None,
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
    use crate::agents::AgentExecutionMode;
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
        origin: ToolOrigin,
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
        Provider::new_inner(
            Arc::new(Manifest::default()),
            crate::provider::ProviderServices {
                model_factory: Arc::new(TestModelFactory),
                tool_factory: Arc::new(TestToolFactory),
                memory: None,
                agent_config: AgentConfig::default(),
                render_ctx_extra: Default::default(),
                argument_bindings: Default::default(),
                knowledge: Default::default(),
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
                    templates: Default::default(),
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
                heartbeat: None,
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
            },
            model: AgentModel {
                model_name: "mock".into(),
                model_slug: crate::Slug::derive("mock"),
                temperature: 0.2,
                model_provider: Arc::new(NoopProvider),
            },
            prompt: AgentPromptState {
                context: PromptContext {
                    agent_name: "nenji".into(),
                    agent_description: "system agent".into(),
                    current_project: crate::manifest::ProjectManifest {
                        name: String::new(),
                        slug: crate::Slug::derive("project"),
                        description: None,
                        settings: serde_json::Value::Null,
                    },
                    active_domain: Some(ActiveDomain {
                        session_id: uuid::Uuid::new_v4(),
                        domain_slug: crate::Slug::derive("creator"),
                        domain_name: "creator".into(),
                        manifest: DomainManifest {
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
                memory_vars: Default::default(),
                artifact_vars: Default::default(),
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
            },
        }
    }

    #[tokio::test]
    async fn ability_sub_instance_uses_ability_prompt_without_domain_addon() {
        let mut caller = test_instance_with_active_domain();
        caller.manifest.abilities = vec!["caller_ability".into()];
        caller.manifest.domains = vec![crate::Slug::derive("creator")];
        let ability = AbilityManifest {
            name: "agent_builder".into(),
            path: Some("nenjo/platform".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "ability developer".into(),
            },
            platform_scopes: vec!["agents:write".into()],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability).await;
        let prompts = sub_instance
            .build_prompts(&AgentRun::chat(ChatInput {
                message: "build an agent".into(),
                history: vec![],
                project: None,
                template_override: None,
            }))
            .unwrap();

        assert_eq!(prompts.system, "caller system");
        assert_eq!(prompts.developer, "ability developer");
        assert!(!prompts.developer.contains("domain addon"));
        assert!(!sub_instance.prompt.context.append_active_domain_addon);
        assert!(sub_instance.prompt.context.active_domain.is_none());
        assert_eq!(sub_instance.manifest.platform_scopes, vec!["agents:write"]);
        assert!(sub_instance.manifest.abilities.is_empty());
        assert!(sub_instance.manifest.domains.is_empty());
        let tool_names: Vec<_> = sub_instance
            .runtime
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect();
        assert!(tool_names.contains(&"list_agents"));
        assert!(tool_names.contains(&"create_agent"));
        assert!(!tool_names.contains(&DELEGATE_TO_TOOL_NAME));
    }

    #[tokio::test]
    async fn ability_sub_instance_does_not_inherit_caller_scopes_or_assignments() {
        let mut caller = test_instance_with_active_domain();
        caller.manifest.platform_scopes = vec!["agents:read".into()];
        caller.manifest.abilities = vec!["caller_ability".into()];
        caller.manifest.domains = vec![crate::Slug::derive("creator")];
        let ability = AbilityManifest {
            name: "isolated".into(),
            path: Some("nenjo/platform".into()),
            description: Some("Runs isolated".into()),
            activation_condition: "When isolation is needed".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "ability developer".into(),
            },
            platform_scopes: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability).await;
        let tool_names: Vec<_> = sub_instance
            .runtime
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect();

        assert!(!tool_names.contains(&"list_agents"));
        assert!(!tool_names.contains(&"create_agent"));
        assert!(sub_instance.manifest.platform_scopes.is_empty());
        assert!(sub_instance.manifest.abilities.is_empty());
        assert!(sub_instance.manifest.domains.is_empty());
        assert!(sub_instance.prompt.context.active_domain.is_none());
    }

    #[tokio::test]
    async fn ability_sub_instance_renders_context_blocks_and_user_message() {
        let mut caller = test_instance_with_active_domain();
        caller.manifest.prompt_config.system_prompt = "{{ pkg.nenjo.core.methodology }}".into();
        caller.prompt.renderer = ContextRenderer::from_blocks(&[
            RenderContextBlock {
                name: "methodology".into(),
                path: "pkg/nenjo/core".into(),
                template: "<methodology>{{ agent.name }}</methodology>".into(),
                package_name: None,
                package_version: None,
            },
            RenderContextBlock {
                name: "tool_usage".into(),
                path: "pkg/nenjo/core".into(),
                template: "<tool_usage>{{ agent.name }}</tool_usage>".into(),
                package_name: None,
                package_version: None,
            },
        ]);
        let ability = AbilityManifest {
            name: "agent_builder".into(),
            path: Some("nenjo/platform".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "{{ pkg.nenjo.core.tool_usage }}".into(),
            },
            platform_scopes: vec!["agents:write".into()],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability).await;
        let prompts = sub_instance
            .build_prompts(&AgentRun::chat(ChatInput {
                message: "build an agent".into(),
                history: vec![],
                project: None,
                template_override: None,
            }))
            .unwrap();

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
            Arc::new(TestTool {
                name: "shell",
                origin: ToolOrigin::Host,
            }),
            Arc::new(TestTool {
                name: "remember_fact",
                origin: ToolOrigin::Host,
            }),
            Arc::new(TestTool {
                name: DELEGATE_TO_TOOL_NAME,
                origin: ToolOrigin::Host,
            }),
        ];
        let ability = AbilityManifest {
            name: "agent_builder".into(),
            path: Some("nenjo/platform".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "ability developer".into(),
            },
            platform_scopes: vec!["agents:write".into()],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability).await;
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
        assert!(!tool_names.contains(&DELEGATE_TO_TOOL_NAME));
    }

    #[tokio::test]
    async fn ability_sub_instance_does_not_inherit_caller_mcp_tools() {
        let mut caller = test_instance_with_active_domain();
        caller.manifest.mcp_servers = vec![crate::Slug::derive("caller-mcp")];
        caller.runtime.tools = vec![
            Arc::new(TestTool {
                name: "shell",
                origin: ToolOrigin::Host,
            }),
            Arc::new(TestTool {
                name: "caller_mcp_tool",
                origin: ToolOrigin::Mcp,
            }),
        ];
        let ability = AbilityManifest {
            name: "agent_builder".into(),
            path: Some("nenjo/platform".into()),
            description: Some("Builds agents".into()),
            activation_condition: "When building agents".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "ability developer".into(),
            },
            platform_scopes: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let sub_instance = build_ability_instance(&caller, &ability).await;
        let tool_names: Vec<_> = sub_instance
            .runtime
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect();

        assert!(tool_names.contains(&"shell"));
        assert!(!tool_names.contains(&"caller_mcp_tool"));
        assert!(sub_instance.manifest.mcp_servers.is_empty());
    }

    #[test]
    fn use_ability_schema_requires_self_contained_input() {
        let ability = AbilityManifest {
            name: "review".into(),
            path: Some("review".into()),
            description: Some("Reviews code".into()),
            activation_condition: "When code review is needed".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "review code".into(),
            },
            platform_scopes: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };
        let registry = Arc::new(AbilityRegistry::new(&[ability]).unwrap());
        let tool = UseAbilityTool::new(registry, Arc::new(test_instance_with_active_domain()));

        let description = tool.description();
        let schema = tool.parameters_schema();
        let task_description = schema["properties"]["input"]["description"]
            .as_str()
            .unwrap_or_default();

        assert!(description.contains("Invoke one ability"));
        assert!(task_description.contains("self-contained delegated task"));
        assert!(task_description.contains("code snippets"));
        assert!(task_description.contains("without access to the caller conversation"));
    }

    #[tokio::test]
    async fn list_assigned_abilities_returns_all_assigned_ability_metadata() {
        let review = AbilityManifest {
            name: "Code Review".into(),
            path: Some("review".into()),
            description: Some("Reviews code".into()),
            activation_condition: "When code review is needed".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "review code".into(),
            },
            platform_scopes: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };
        let docs = AbilityManifest {
            name: "Search Docs!".into(),
            path: Some("docs".into()),
            description: None,
            activation_condition: "When documentation lookup is needed".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "search docs".into(),
            },
            platform_scopes: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };
        let registry = Arc::new(AbilityRegistry::new(&[review, docs]).unwrap());
        let tool = ListAssignedAbilitiesTool::new(registry);

        let result = tool.execute(serde_json::json!({})).await.unwrap();
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();

        assert!(result.success);
        assert_eq!(output["abilities"][0]["name"], "Code Review");
        assert!(output["abilities"][0].get("ability_id").is_none());
        assert!(output["abilities"][0].get("display_name").is_none());
        assert_eq!(
            output["abilities"][0]["activation_condition"],
            "When code review is needed"
        );
        assert_eq!(output["abilities"][1]["name"], "Search Docs!");
    }

    #[test]
    fn ability_tool_filter_does_not_match_manifest_resource_list_tool() {
        assert!(is_ability_tool("list_assigned_abilities"));
        assert!(is_ability_tool("use_ability"));
        assert!(!is_ability_tool("inspect_operations"));
        assert!(!is_ability_tool("stop_operations"));
        assert!(!is_ability_tool("wait_operations"));
        assert!(!is_ability_tool("list_abilities"));
    }

    #[test]
    fn async_operation_tools_are_generic_harness_tools() {
        let tools = build_async_operation_tools(AsyncOpManager::new());
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();

        assert!(names.contains(&"inspect_operations"));
        assert!(names.contains(&"send_operation_input"));
        assert!(names.contains(&"stop_operations"));
        assert!(names.contains(&"wait_operations"));
    }

    #[test]
    fn inspect_abilities_args_accept_whole_float_limit_from_model_args() {
        let args: InspectAbilitiesArgs = serde_json::from_value(serde_json::json!({
            "operations": ["ability_build_agent_2"],
            "include_transcript": true,
            "limit": 5.0
        }))
        .unwrap();

        assert_eq!(args.limit, 5);
    }

    #[test]
    fn duplicate_ability_ids_are_rejected() {
        let first = AbilityManifest {
            name: "code_review".into(),
            path: Some("frontend".into()),
            description: None,
            activation_condition: "frontend".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "frontend".into(),
            },
            platform_scopes: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };
        let second = AbilityManifest {
            name: "code_review".into(),
            path: Some("backend".into()),
            description: None,
            activation_condition: "backend".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "backend".into(),
            },
            platform_scopes: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let error = AbilityRegistry::new(&[first, second]).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("duplicate ability_id 'code_review'")
        );
    }
}
