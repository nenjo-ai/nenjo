//! Start, run, and complete a cancellable ability operation.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::debug;

use nenjo_models::{ConversationMessage, ModelProvider};

use crate::agents::async_ops::{
    AsyncOpChildHandle, AsyncOpId, AsyncOpKind, AsyncOpManager, AsyncOpSignal, StartAsyncOp,
    truncate,
};
use crate::agents::instance::{AgentInstance, BuiltPrompts};
use crate::agents::runner::types::{TurnEvent, TurnOutput};
use crate::agents::runner::{build_instruction_messages, turn_loop};
use crate::input::{AgentRun, ChatInput};
use crate::manifest::AbilityManifest;
use crate::provider::ProviderRuntime;
use crate::tools::{AsyncControl, AsyncControls, AsyncOperationStartReceipt, ToolResult};

use super::child_tools::{AbilityFinish, AbilityFinishStatus, ability_child_tools};
use super::events::spawn_ability_event_bridge;
use super::instance::build_ability_instance;
use super::{
    ABILITY_COMPLETION_GUIDANCE, FINISH_ABILITY_TOOL_NAME, USE_ABILITY_TOOL_NAME, json_tool,
};

static ABILITY_OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize)]
struct AbilityOperationStarted<'a> {
    ability: &'a str,
    #[serde(flatten)]
    operation: AsyncOperationStartReceipt,
}

/// Register the operation before spawning its child scope and returning a receipt.
///
/// The unfinished-operation limit is separate from runnable and provider permits.
/// The spawned child acquires those permits only during its active phases.
pub(super) async fn start_ability_operation<P>(
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
    let timezone = turn_loop::current_execution_timezone().unwrap_or(chrono_tz::UTC);
    let parent_events_tx = turn_loop::current_events_tx();
    let operation_id = next_ability_operation_id(ability_id);
    let controls = AsyncControls::new(AsyncControl::Inspect)
        .with(AsyncControl::SendInput)
        .with(AsyncControl::Stop)
        .with(AsyncControl::Wait);
    let started = match instance
        .runtime
        .async_ops
        .start_nested(
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
        .await
    {
        Ok(started) => started,
        Err(error) => return Ok(error.into_tool_result()),
    };

    let operation = AbilityOperation {
        instance: instance.clone(),
        ability: ability.clone(),
        call_id: operation_id.to_string(),
        task_description: task_description.to_string(),
        caller_history_snapshot,
        timezone,
        child_handle: started.child,
        op_handle: started.handle.clone(),
        parent_events_tx: parent_events_tx.clone(),
    };
    let execution_scope =
        crate::concurrency::ExecutionContext::current().map(|context| context.child());
    let join = tokio::spawn(crate::concurrency::in_scope(
        execution_scope,
        run_ability_operation(operation),
    ));
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

/// Owned inputs and handles retained for one cancellable ability invocation.
pub(super) struct AbilityOperation<P: ProviderRuntime> {
    pub(super) instance: Arc<AgentInstance<P>>,
    pub(super) ability: AbilityManifest,
    pub(super) call_id: String,
    pub(super) task_description: String,
    pub(super) caller_history_snapshot: Vec<nenjo_models::ConversationMessage>,
    pub(super) timezone: chrono_tz::Tz,
    pub(super) child_handle: AsyncOpChildHandle,
    pub(super) op_handle: crate::agents::async_ops::AsyncOpHandle,
    pub(super) parent_events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
}

/// Run the child to an explicit `finish` result and publish one completion outcome.
pub(super) async fn run_ability_operation<P: ProviderRuntime>(mut operation: AbilityOperation<P>) {
    let result = execute_ability_operation(&mut operation)
        .await
        .and_then(|output| {
            turn_loop::record_nested_token_usage(output.input_tokens, output.output_tokens);
            serde_json::from_str::<AbilityFinish>(&output.text)
                .map_err(|error| anyhow::anyhow!("ability finish result was invalid: {error}"))
        });
    complete_ability_operation(&operation, result).await;
}

/// Assemble the isolated child, bridge its events, and await its terminal tool.
///
/// Caller history is recorded in the start event, but is not added to the child's
/// prompt. Cancellation drops the turn future, releasing any phase/model permits.
async fn execute_ability_operation<P: ProviderRuntime>(
    operation: &mut AbilityOperation<P>,
) -> Result<TurnOutput> {
    let mut child = build_ability_instance(&operation.instance, &operation.ability).await;
    let cancel = operation.op_handle.cancel_token();
    child.runtime.execution_cancel = cancel.clone();
    child.runtime.async_ops = AsyncOpManager::with_cancel_and_nested_queue(
        cancel.clone(),
        child.runtime.config.max_active_nested_runs,
        child.runtime.config.max_pending_nested_runs,
    );
    child
        .runtime
        .tools
        .extend(ability_child_tools(operation.child_handle.clone()));

    let task = AgentRun::chat(ChatInput {
        message: operation.task_description.clone(),
        history: Vec::new(),
        project: None,
        replayed_turn_contexts: Vec::new(),
        artifacts: Vec::new(),
        timezone: operation.timezone,
    });
    if let Some(parent) = &operation.parent_events_tx {
        debug!(ability = operation.ability.name, "Emitting AbilityStarted");
        let _ = parent.send(TurnEvent::AbilityStarted {
            call_id: operation.call_id.clone(),
            ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
            ability_name: operation.ability.name.clone(),
            task_input: operation.task_description.clone(),
            caller_history: std::mem::take(&mut operation.caller_history_snapshot),
        });
    }
    let prompts = child
        .build_prompts(&task)
        .map_err(|error| anyhow::anyhow!("ability prompt build failed: {error}"))?;
    let tool_names: Vec<_> = child.runtime.tools.iter().map(|tool| tool.name()).collect();
    debug!(
        ability = operation.ability.name,
        agent = operation.instance.name(),
        tool_count = tool_names.len(),
        tools = ?tool_names,
        "Ability sub-agent prompt"
    );
    debug!("{prompts}");
    debug!(
        ability = operation.ability.name,
        user_message = operation.task_description,
        "Ability sub-agent user message"
    );
    let messages = ability_messages(&child, &prompts, &operation.task_description);
    let (nested_tx, nested_rx) = mpsc::unbounded_channel();
    let bridge = spawn_ability_event_bridge(
        operation.op_handle.clone(),
        operation.call_id.clone(),
        operation.parent_events_tx.clone(),
        nested_rx,
    );
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow::anyhow!("ability operation stopped")),
        result = turn_loop::run(
            &child,
            messages,
            Some(nested_tx),
            None,
            None,
            turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
            crate::agents::runner::chat::ProviderResponseDelivery::Buffered,
        ) => result,
    };
    if let Some(bridge) = bridge {
        let _ = bridge.await;
    }
    result.map_err(|error| anyhow::anyhow!("ability execution failed: {error}"))
}

/// Preserve prompt ordering while supplying only this ability's task and context.
fn ability_messages<P: ProviderRuntime>(
    child: &AgentInstance<P>,
    prompts: &BuiltPrompts,
    task_description: &str,
) -> Vec<ConversationMessage> {
    let supports_developer_role = child
        .model
        .model_provider
        .supports_developer_role(&child.model.model_name);
    let mut messages =
        build_instruction_messages(&prompts.system, &prompts.developer, supports_developer_role);
    messages.push(ConversationMessage::developer(ABILITY_COMPLETION_GUIDANCE));
    messages.extend(
        prompts
            .session_context
            .messages()
            .iter()
            .cloned()
            .map(ConversationMessage::runtime_context),
    );
    messages.extend(
        prompts
            .turn_context
            .messages()
            .iter()
            .cloned()
            .map(ConversationMessage::runtime_context),
    );
    messages.push(ConversationMessage::user(task_description));
    messages
}

/// Keep the operation registry and parent completion event consistent on every exit.
async fn complete_ability_operation<P: ProviderRuntime>(
    operation: &AbilityOperation<P>,
    result: Result<AbilityFinish>,
) {
    let (signal, success, final_output) = match result {
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
            (signal, success, summary)
        }
        Err(error) => {
            let error = error.to_string();
            (
                AsyncOpSignal::Failed {
                    error: truncate(&error, 500),
                    output: None,
                },
                false,
                error,
            )
        }
    };
    operation
        .op_handle
        .complete(signal, operation.parent_events_tx.clone())
        .await;
    if let Some(parent) = &operation.parent_events_tx {
        debug!(
            ability = operation.ability.name,
            success, "Emitting AbilityCompleted"
        );
        let _ = parent.send(TurnEvent::AbilityCompleted {
            call_id: operation.call_id.clone(),
            ability_tool_name: USE_ABILITY_TOOL_NAME.to_string(),
            ability_name: operation.ability.name.clone(),
            success,
            final_output,
        });
    }
}

/// Generate process-local operation IDs without changing the stable ability name.
fn next_ability_operation_id(ability_id: &str) -> AsyncOpId {
    let slug = crate::Slug::derive_with_fallback(ability_id, "ability");
    let sequence = ABILITY_OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    AsyncOpId::new(format!("ability_{slug}_{sequence}"))
}
