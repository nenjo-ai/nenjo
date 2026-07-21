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
    AsyncControl, AsyncControls, AsyncOperationStartReceipt, INSPECT_TOOL_NAME,
    InspectOperationsArgs, SEND_INPUT_TOOL_NAME, STOP_TOOL_NAME, SendOperationInputArgs,
    StopOperationsArgs, Tool, ToolCategory, ToolOrigin, ToolResult, WAIT_TOOL_NAME,
    WaitOperationsArgs, inspect_operations_parameters_schema,
    send_operation_input_parameters_schema, stop_operations_parameters_schema,
    wait_operations_parameters_schema,
};

use super::async_ops::{
    AsyncOpChildHandle, AsyncOpId, AsyncOpKind, AsyncOpManager, AsyncOpSignal, AsyncOpWaitFilter,
    StartAsyncOp, truncate,
};
use super::delegation::DELEGATE_TO_TOOL_NAME;
use super::instance::{AgentExecutionMode, AgentInstance, AgentPromptState, AgentRuntime};
use super::runner::types::{AsyncOperationTranscriptEvent, TurnEvent};
use super::runner::{build_instruction_messages, turn_loop};
use crate::input::{AgentRun, ChatInput};
use crate::manifest::{AbilityManifest, PromptConfig, PromptTemplates};
use crate::provider::{ErasedProvider, ProviderRuntime, ToolContext, ToolFactory};

pub const LIST_ASSIGNED_ABILITIES_TOOL_NAME: &str = "list_assigned_abilities";
pub const USE_ABILITY_TOOL_NAME: &str = "use_ability";
pub(crate) const FINISH_ABILITY_TOOL_NAME: &str = "finish";

const ABILITY_COMPLETION_GUIDANCE: &str = "Complete this ability execution through the `finish` tool. Ordinary assistant prose does not end an ability. Use status `completed` only after the requested work and its required verification are complete. Use status `failed` when the work cannot be completed, and explain the concrete blocker. Use `ask_parent_agent` instead when parent input could unblock the work.";

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

#[derive(Debug, Serialize)]
struct AbilityOperationStarted<'a> {
    ability: &'a str,
    #[serde(flatten)]
    operation: AsyncOperationStartReceipt,
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

struct InspectOperationsTool {
    async_ops: AsyncOpManager,
}

struct StopOperationsTool {
    async_ops: AsyncOpManager,
}

struct SendOperationInputTool {
    async_ops: AsyncOpManager,
}

struct WaitOperationsTool {
    async_ops: AsyncOpManager,
}

#[async_trait::async_trait]
impl Tool for InspectOperationsTool {
    fn name(&self) -> &str {
        INSPECT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Inspect running or recently completed async operations by operation_id. Use this after wait reports completion or failure when you need the final output payload or recent transcript."
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

    async fn is_available_to_model(&self) -> bool {
        self.async_ops
            .has_model_visible_control(AsyncControl::Inspect)
            .await
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
        STOP_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Stop one or more running async operations. Filter by kind to avoid stopping unrelated work. Do not stop and restart an operation solely because an internal tool call failed while its status remains running."
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

    async fn is_available_to_model(&self) -> bool {
        self.async_ops
            .has_model_visible_control(AsyncControl::Stop)
            .await
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
        SEND_INPUT_TOOL_NAME
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

    async fn is_available_to_model(&self) -> bool {
        self.async_ops
            .has_model_visible_control(AsyncControl::SendInput)
            .await
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: SendOperationInputArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!({
            "sent": self.async_ops.send_input(parsed.operations, parsed.message).await
        })))
    }
}

#[async_trait::async_trait]
impl Tool for WaitOperationsTool {
    fn name(&self) -> &str {
        WAIT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Wait while async operations continue running, then return queued operation signals. A recoverable_tool_error keeps the operation running and recommends waiting rather than stopping and reinvoking it. For video or other media generation, call this with kind=media after the media tool returns job_started; repeat wait until the media operation completes or fails instead of calling the generation tool again for the same request."
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

    async fn is_available_to_model(&self) -> bool {
        self.async_ops
            .has_model_visible_control(AsyncControl::Wait)
            .await
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: WaitOperationsArgs = serde_json::from_value(args)?;
        let _reason = parsed.reason;
        if let Some(turn_input) = super::runner::turn_loop::current_turn_input() {
            let result = tokio::select! {
                result = self.async_ops.wait(parsed.seconds, AsyncOpWaitFilter::control(AsyncControl::Wait, parsed.kind)) => result,
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
                .wait(
                    parsed.seconds,
                    AsyncOpWaitFilter::control(AsyncControl::Wait, parsed.kind),
                )
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

struct FinishAbilityTool;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AbilityFinishStatus {
    Completed,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
struct AbilityFinish {
    status: AbilityFinishStatus,
    summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
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
        "Ask the parent agent for input and wait until it responds with send_input."
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

#[async_trait::async_trait]
impl Tool for FinishAbilityTool {
    fn name(&self) -> &str {
        FINISH_ABILITY_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Finish this ability operation. Use completed only after all requested actions and verification have succeeded. Use failed when the ability cannot complete the request. Ordinary assistant prose does not finish an ability."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["completed", "failed"]
                },
                "summary": {
                    "type": "string",
                    "minLength": 1,
                    "description": "A concise account of the completed result or concrete failure."
                },
                "result": {
                    "description": "Optional structured result for the parent agent."
                }
            },
            "required": ["status", "summary"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    fn is_terminal(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let mut finish: AbilityFinish = serde_json::from_value(args)?;
        finish.summary = finish.summary.trim().to_string();
        if finish.summary.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("summary is required".into()),
            });
        }
        Ok(json_tool(serde_json::to_value(finish)?))
    }
}

fn ability_child_tools(handle: AsyncOpChildHandle) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(UpdateAbilityParentTool {
            handle: handle.clone(),
        }),
        Arc::new(AskAbilityParentTool { handle }),
        Arc::new(FinishAbilityTool),
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
    let controls = AsyncControls::new(AsyncControl::Inspect)
        .with(AsyncControl::SendInput)
        .with(AsyncControl::Stop)
        .with(AsyncControl::Wait);
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
                controls,
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
    started.handle.attach_join(join, parent_events_tx).await;

    Ok(json_tool(serde_json::to_value(AbilityOperationStarted {
        ability: ability_id,
        operation: AsyncOperationStartReceipt::new(
            operation_id.to_string(),
            AsyncOpKind::Ability,
            controls,
        ),
    })?))
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
                        output: None,
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
    messages.push(nenjo_models::ChatMessage::developer(
        ABILITY_COMPLETION_GUIDANCE.to_string(),
    ));

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
        result = turn_loop::run(
            &sub_instance,
            messages,
            Some(nested_tx),
            None,
            None,
            turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        ) => result,
    };

    if let Some(bridge) = bridge {
        let _ = bridge.await;
    }

    match result {
        Ok(output) => {
            turn_loop::record_nested_token_usage(output.input_tokens, output.output_tokens);
            match serde_json::from_str::<AbilityFinish>(&output.text) {
                Ok(finish) => {
                    let summary = truncate(&finish.summary, 500);
                    let output = Some(serde_json::json!({
                        "summary": finish.summary,
                        "result": finish.result,
                    }));
                    let (signal, success) = match finish.status {
                        AbilityFinishStatus::Completed => (
                            AsyncOpSignal::Completed {
                                summary: summary.clone(),
                                output,
                            },
                            true,
                        ),
                        AbilityFinishStatus::Failed => (
                            AsyncOpSignal::Failed {
                                error: summary.clone(),
                                output,
                            },
                            false,
                        ),
                    };
                    op_handle.complete(signal, parent_events_tx.clone()).await;
                    if let Some(parent_tx) = parent_events_tx.clone() {
                        debug!(
                            ability = ability.name,
                            ability_tool_name = USE_ABILITY_TOOL_NAME,
                            success,
                            "Emitting AbilityCompleted"
                        );
                        let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                            call_id: call_id.clone(),
                            ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
                            ability_name: ability.name.clone(),
                            success,
                            final_output: summary,
                        });
                    }
                }
                Err(error) => {
                    let error = format!("ability finish result was invalid: {error}");
                    op_handle
                        .complete(
                            AsyncOpSignal::Failed {
                                error: truncate(&error, 500),
                                output: None,
                            },
                            parent_events_tx.clone(),
                        )
                        .await;
                    if let Some(parent_tx) = parent_events_tx.clone() {
                        let _ = parent_tx.send(TurnEvent::AbilityCompleted {
                            call_id: call_id.clone(),
                            ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
                            ability_name: ability.name.clone(),
                            success: false,
                            final_output: error,
                        });
                    }
                }
            }
        }
        Err(e) => {
            let error = format!("ability execution failed: {e}");
            op_handle
                .complete(
                    AsyncOpSignal::Failed {
                        error: truncate(&error, 500),
                        output: None,
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
            if !result.success {
                handle
                    .recoverable_tool_error(
                        tool_name.clone(),
                        result
                            .error
                            .clone()
                            .unwrap_or_else(|| result.output.clone()),
                        events_tx.clone(),
                    )
                    .await;
            }
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
    Ok(vec![
        Arc::new(ListAssignedAbilitiesTool::new(registry.clone())) as Arc<dyn Tool>,
        Arc::new(UseAbilityTool::new(registry, instance.clone())) as Arc<dyn Tool>,
    ])
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
        LIST_ASSIGNED_ABILITIES_TOOL_NAME | USE_ABILITY_TOOL_NAME
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
            .create_tools_with_context(
                &scoped_agent,
                scoped_security.clone(),
                ToolContext {
                    project_slug: Some(caller.prompt.context.current_project.slug.to_string()),
                    current_session_id: caller.runtime.current_session_id,
                },
            )
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
            execution_mode: AgentExecutionMode::Ability,
            hook_runtime: None,
            current_session_id: caller.runtime.current_session_id,
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::agents::instance::AgentModel;
    use crate::agents::prompts::PromptContext;
    use crate::agents::respond::RESPOND_TO_USER_TOOL_NAME;
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
    use nenjo_models::traits::{
        ChatMessage, ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall,
    };

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
        seen_messages: Mutex<Vec<Vec<ChatMessage>>>,
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
                current_session_id: None,
            },
        }
    }

    #[tokio::test]
    async fn ability_sub_instance_uses_ability_prompt_without_domain_addon() {
        let mut caller = test_instance_with_active_domain();
        caller.manifest.abilities = vec![crate::Slug::derive("caller_ability")];
        caller.manifest.domains = vec![crate::Slug::derive("creator")];
        let ability = AbilityManifest {
            slug: crate::Slug::derive("agent-builder"),
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
        caller.manifest.abilities = vec![crate::Slug::derive("caller_ability")];
        caller.manifest.domains = vec![crate::Slug::derive("creator")];
        let ability = AbilityManifest {
            slug: crate::Slug::derive("isolated"),
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
        assert!(!tool_names.contains(&RESPOND_TO_USER_TOOL_NAME));
        assert_eq!(
            sub_instance.runtime.execution_mode,
            AgentExecutionMode::Ability
        );
        assert!(sub_instance.manifest.platform_scopes.is_empty());
        assert!(sub_instance.manifest.abilities.is_empty());
        assert!(sub_instance.manifest.domains.is_empty());
        assert!(sub_instance.prompt.context.active_domain.is_none());
    }

    #[tokio::test]
    async fn ability_completion_rejects_plain_prose_until_finish_is_called() {
        let provider = Arc::new(SequentialProvider {
            responses: vec![
                ChatResponse {
                    text: Some("The prerequisites exist. I'll create it now.".into()),
                    tool_calls: Vec::new(),
                    provider_tool_calls: Vec::new(),
                    usage: TokenUsage::default(),
                },
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "finish_1".into(),
                        name: FINISH_ABILITY_TOOL_NAME.into(),
                        arguments: serde_json::json!({
                            "status": "completed",
                            "summary": "Created and verified the routine",
                            "result": {"slug": "code-generation"}
                        })
                        .to_string(),
                    }],
                    provider_tool_calls: Vec::new(),
                    usage: TokenUsage::default(),
                },
            ],
            next: AtomicUsize::new(0),
            seen_messages: Mutex::new(Vec::new()),
        });
        let mut instance = test_instance_with_active_domain();
        instance.model.model_provider = provider.clone();
        instance.runtime.tools = vec![Arc::new(FinishAbilityTool)];

        let output = turn_loop::run(
            &instance,
            vec![ChatMessage::user("Create the routine")],
            None,
            None,
            None,
            turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        )
        .await
        .unwrap();

        let finish: AbilityFinish = serde_json::from_str(&output.text).unwrap();
        assert!(matches!(finish.status, AbilityFinishStatus::Completed));
        assert_eq!(finish.summary, "Created and verified the routine");
        assert_eq!(output.tool_calls, 1);

        let seen_messages = provider.seen_messages.lock().unwrap();
        assert_eq!(seen_messages.len(), 2);
        assert!(seen_messages[1].iter().any(|message| {
            message.role == "developer"
                && message.content.contains("requires the finish tool")
                && message.content.contains("I'll create it now")
        }));
    }

    #[tokio::test]
    async fn required_finish_uses_its_own_result_when_another_terminal_tool_runs_first() {
        let provider = Arc::new(SequentialProvider {
            responses: vec![ChatResponse {
                text: None,
                tool_calls: vec![
                    ToolCall {
                        id: "other_1".into(),
                        name: "other_terminal".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "finish_1".into(),
                        name: FINISH_ABILITY_TOOL_NAME.into(),
                        arguments: serde_json::json!({
                            "status": "completed",
                            "summary": "Created the routine",
                            "result": {"slug": "code-generation"}
                        })
                        .to_string(),
                    },
                ],
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
            }],
            next: AtomicUsize::new(0),
            seen_messages: Mutex::new(Vec::new()),
        });
        let mut instance = test_instance_with_active_domain();
        instance.model.model_provider = provider;
        instance.runtime.tools = vec![Arc::new(OtherTerminalTool), Arc::new(FinishAbilityTool)];

        let output = turn_loop::run(
            &instance,
            vec![ChatMessage::user("Create the routine")],
            None,
            None,
            None,
            turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        )
        .await
        .unwrap();

        let finish: AbilityFinish = serde_json::from_str(&output.text).unwrap();
        assert!(matches!(finish.status, AbilityFinishStatus::Completed));
        assert_eq!(finish.summary, "Created the routine");
        assert_eq!(output.tool_calls, 2);
    }

    #[tokio::test]
    async fn invalid_finish_result_is_recoverable_within_the_same_ability_run() {
        let provider = Arc::new(SequentialProvider {
            responses: vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "finish_invalid".into(),
                        name: FINISH_ABILITY_TOOL_NAME.into(),
                        arguments: serde_json::json!({
                            "status": "completed",
                            "summary": " "
                        })
                        .to_string(),
                    }],
                    provider_tool_calls: Vec::new(),
                    usage: TokenUsage::default(),
                },
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "finish_valid".into(),
                        name: FINISH_ABILITY_TOOL_NAME.into(),
                        arguments: serde_json::json!({
                            "status": "completed",
                            "summary": "Created and verified the routine"
                        })
                        .to_string(),
                    }],
                    provider_tool_calls: Vec::new(),
                    usage: TokenUsage::default(),
                },
            ],
            next: AtomicUsize::new(0),
            seen_messages: Mutex::new(Vec::new()),
        });
        let mut instance = test_instance_with_active_domain();
        instance.model.model_provider = provider.clone();
        instance.runtime.tools = vec![Arc::new(FinishAbilityTool)];

        let output = turn_loop::run(
            &instance,
            vec![ChatMessage::user("Create the routine")],
            None,
            None,
            None,
            turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        )
        .await
        .unwrap();

        let finish: AbilityFinish = serde_json::from_str(&output.text).unwrap();
        assert_eq!(finish.summary, "Created and verified the routine");
        let seen_messages = provider.seen_messages.lock().unwrap();
        assert_eq!(seen_messages.len(), 2);
        assert!(seen_messages[1].iter().any(|message| {
            message.role == "tool" && message.content.contains("summary is required")
        }));
    }

    #[tokio::test]
    async fn required_completion_tool_must_be_registered_and_terminal() {
        let mut instance = test_instance_with_active_domain();
        instance.runtime.tools.clear();

        let error = turn_loop::run(
            &instance,
            vec![ChatMessage::user("Create the routine")],
            None,
            None,
            None,
            turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("required completion tool 'finish'")
        );
    }

    #[tokio::test]
    async fn finish_requires_a_nonempty_summary() {
        let result = FinishAbilityTool
            .execute(serde_json::json!({
                "status": "completed",
                "summary": "  "
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("summary is required"));
    }

    #[tokio::test]
    async fn failed_finish_propagates_to_async_state_and_parent_event() {
        let provider = Arc::new(SequentialProvider {
            responses: vec![ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "finish_failed".into(),
                    name: FINISH_ABILITY_TOOL_NAME.into(),
                    arguments: serde_json::json!({
                        "status": "failed",
                        "summary": "Routine verification failed",
                        "result": {"slug": "code-generation", "verified": false}
                    })
                    .to_string(),
                }],
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
            }],
            next: AtomicUsize::new(0),
            seen_messages: Mutex::new(Vec::new()),
        });
        let mut instance = test_instance_with_active_domain();
        instance.model.model_provider = provider;
        let instance = Arc::new(instance);
        let manager = instance.runtime.async_ops.clone();
        let operation_id = AsyncOpId::new("ability_build_routine_failed");
        let controls = AsyncControls::new(AsyncControl::Inspect).with(AsyncControl::Wait);
        let started = manager
            .start(
                StartAsyncOp {
                    id: operation_id.clone(),
                    kind: AsyncOpKind::Ability,
                    label: "build_routine".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some(USE_ABILITY_TOOL_NAME.into()),
                    started_summary: "Build the routine".into(),
                    model_visible: true,
                    controls,
                },
                None,
            )
            .await;
        manager
            .drain_signals(AsyncOpWaitFilter::model_visible())
            .await;
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let ability = AbilityManifest {
            slug: crate::Slug::derive("build_routine"),
            name: "build_routine".into(),
            path: None,
            description: Some("Build a routine".into()),
            activation_condition: "When a routine is requested".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "Build and verify the requested routine.".into(),
            },
            platform_scopes: Vec::new(),
            mcp_servers: Vec::new(),
            script_tools: Vec::new(),
            media: Vec::new(),
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        run_ability_operation(AbilityOperation {
            instance,
            ability,
            call_id: operation_id.to_string(),
            task_description: "Build the routine".into(),
            caller_history_snapshot: Vec::new(),
            child_handle: started.child,
            op_handle: started.handle,
            parent_events_tx: Some(events_tx),
        })
        .await;

        let signals = manager
            .drain_signals(AsyncOpWaitFilter::model_visible())
            .await;
        assert!(signals.iter().flat_map(|digest| &digest.events).any(
            |signal| matches!(signal, AsyncOpSignal::Failed { error, .. } if error == "Routine verification failed")
        ));
        let inspections = manager
            .inspect(
                vec![operation_id.to_string()],
                Some(AsyncOpKind::Ability),
                true,
                10,
            )
            .await;
        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].status, "failed");
        assert_eq!(
            inspections[0]
                .latest_output
                .as_ref()
                .and_then(|output| output.pointer("/result/verified")),
            Some(&serde_json::Value::Bool(false))
        );
        assert!(
            std::iter::from_fn(|| events_rx.try_recv().ok()).any(|event| {
                matches!(
                    event,
                    TurnEvent::AbilityCompleted {
                        success: false,
                        final_output,
                        ..
                    } if final_output == "Routine verification failed"
                )
            })
        );
    }

    #[tokio::test]
    async fn failed_ability_tool_call_emits_a_recoverable_running_signal() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_build_routine_1"),
                    kind: AsyncOpKind::Ability,
                    label: "build_routine".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some(USE_ABILITY_TOOL_NAME.into()),
                    started_summary: "building routine".into(),
                    model_visible: true,
                    controls: AsyncControls::new(AsyncControl::Inspect).with(AsyncControl::Wait),
                },
                None,
            )
            .await;
        manager
            .drain_signals(AsyncOpWaitFilter::model_visible())
            .await;

        bridge_ability_transcript(
            &started.handle,
            &TurnEvent::ToolCallEnd {
                batch_id: "batch_1".into(),
                parent_tool_name: None,
                tool_call_id: Some("call_1".into()),
                tool_name: "configure_routine".into(),
                tool_args: "{}".into(),
                result: ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("graph validation failed".into()),
                },
                metadata: None,
            },
            None,
        )
        .await;

        let result = manager
            .wait(1, AsyncOpWaitFilter::kind(Some(AsyncOpKind::Ability)))
            .await;
        assert_eq!(result.woken_by, "recoverable_error");
        assert_eq!(result.updates[0].status, "running");
        assert!(matches!(
            &result.updates[0].events[0],
            AsyncOpSignal::RecoverableToolError { tool, error, .. }
                if tool == "configure_routine" && error == "graph validation failed"
        ));
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
            slug: crate::Slug::derive("agent-builder"),
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
            slug: crate::Slug::derive("agent-builder"),
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
            slug: crate::Slug::derive("agent-builder"),
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
            slug: crate::Slug::derive("review"),
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
            slug: crate::Slug::derive("code-review"),
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
            slug: crate::Slug::derive("search-docs"),
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
        assert!(!is_ability_tool("inspect"));
        assert!(!is_ability_tool("stop"));
        assert!(!is_ability_tool("wait"));
        assert!(!is_ability_tool("list_abilities"));
    }

    #[test]
    fn async_operation_tools_are_generic_harness_tools() {
        let tools = build_async_operation_tools(AsyncOpManager::new());
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();

        assert_eq!(names, ["inspect", "send_input", "stop", "wait"]);
    }

    #[tokio::test]
    async fn generic_async_controls_are_hidden_until_matching_operation_starts() {
        assert!(visible_generic_control_names(None).await.is_empty());
        let all_controls = AsyncControls::new(AsyncControl::Inspect)
            .with(AsyncControl::SendInput)
            .with(AsyncControl::Stop)
            .with(AsyncControl::Wait);
        for kind in [
            AsyncOpKind::Ability,
            AsyncOpKind::SubAgent,
            AsyncOpKind::Delegation,
        ] {
            assert_eq!(
                visible_generic_control_names(Some((kind, all_controls))).await,
                ["inspect", "send_input", "stop", "wait"]
            );
        }
        assert_eq!(
            visible_generic_control_names(Some((
                AsyncOpKind::Media,
                AsyncControls::new(AsyncControl::Inspect)
                    .with(AsyncControl::Stop)
                    .with(AsyncControl::Wait),
            )))
            .await,
            ["inspect", "stop", "wait"]
        );
    }

    async fn visible_generic_control_names(
        operation: Option<(AsyncOpKind, AsyncControls)>,
    ) -> Vec<String> {
        let mut instance = test_instance_with_active_domain();
        let async_ops = instance.runtime.async_ops.clone();
        instance.runtime.tools = build_async_operation_tools(async_ops.clone());
        if let Some((kind, controls)) = operation {
            start_model_visible_operation(&async_ops, kind, controls).await;
        }
        instance
            .visible_local_tool_specs()
            .await
            .into_iter()
            .map(|spec| spec.name)
            .collect()
    }

    async fn start_model_visible_operation(
        async_ops: &AsyncOpManager,
        kind: AsyncOpKind,
        controls: AsyncControls,
    ) {
        let operation_id = format!("{}_1", kind.as_str());
        let _started = async_ops
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new(operation_id),
                    kind,
                    label: kind.as_str().into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("starter".into()),
                    started_summary: "started".into(),
                    model_visible: true,
                    controls,
                },
                None,
            )
            .await;
    }

    #[test]
    fn duplicate_ability_ids_are_rejected() {
        let first = AbilityManifest {
            slug: crate::Slug::derive("frontend-code-review"),
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
            slug: crate::Slug::derive("backend-code-review"),
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
