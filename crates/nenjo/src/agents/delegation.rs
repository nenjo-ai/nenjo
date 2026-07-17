//! Installed-agent delegation tools.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::debug;

use super::async_ops::{
    AsyncOpChildHandle, AsyncOpHandle, AsyncOpId, AsyncOpKind, AsyncOpSignal, StartAsyncOp,
    truncate,
};
use super::instance::{AgentExecutionMode, AgentInstance};
use super::runner::types::{AsyncOperationTranscriptEvent, TurnEvent};
use super::runner::{turn_loop, types::TurnOutput};
use crate::Slug;
use crate::input::TaskInput;
use crate::manifest::{AgentManifest, ProjectManifest};
use crate::provider::{ErasedProvider, ProviderRuntime};
use crate::tools::{Tool, ToolCategory, ToolOrigin, ToolResult};

pub(crate) const DELEGATE_TO_TOOL_NAME: &str = "delegate_to";
pub(crate) const LIST_DELEGATABLE_AGENTS_TOOL_NAME: &str = "list_delegatable_agents";

static DELEGATION_OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) fn build_delegation_tools<P>(instance: Arc<AgentInstance<P>>) -> Vec<Arc<dyn Tool>>
where
    P: ProviderRuntime,
{
    vec![
        Arc::new(ListDelegatableAgentsTool {
            instance: instance.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(DelegateToTool { instance }) as Arc<dyn Tool>,
    ]
}

pub(crate) fn delegation_child_tools(handle: AsyncOpChildHandle) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(UpdateDelegationParentTool {
            handle: handle.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(AskDelegationParentTool { handle }) as Arc<dyn Tool>,
    ]
}

struct DelegateToTool<P: ProviderRuntime = ErasedProvider> {
    instance: Arc<AgentInstance<P>>,
}

struct ListDelegatableAgentsTool<P: ProviderRuntime = ErasedProvider> {
    instance: Arc<AgentInstance<P>>,
}

#[derive(Debug, serde::Serialize)]
struct DelegatableAgent<'a> {
    slug: &'a str,
    description: &'a str,
}

#[async_trait::async_trait]
impl<P> Tool for ListDelegatableAgentsTool<P>
where
    P: ProviderRuntime,
{
    fn name(&self) -> &str {
        LIST_DELEGATABLE_AGENTS_TOOL_NAME
    }

    fn description(&self) -> &str {
        "List installed agents that can receive delegated work. Returns only target slugs and descriptions."
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
        let Some(provider) = self.instance.runtime.provider_runtime.as_ref() else {
            return Ok(error("agent discovery requires provider runtime support"));
        };
        let current = self.instance.agent_slug();
        let manifest = provider.manifest_snapshot();
        let agents = manifest
            .agents
            .iter()
            .filter(|agent| agent.slug() != *current)
            .map(agent_summary)
            .collect::<Vec<_>>();

        Ok(ok(serde_json::json!({ "agents": agents })))
    }
}

#[derive(Debug, Deserialize)]
struct DelegateToArgs {
    agent: String,
    task: String,
}

#[async_trait::async_trait]
impl<P> Tool for DelegateToTool<P>
where
    P: ProviderRuntime,
{
    fn name(&self) -> &str {
        DELEGATE_TO_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delegate a self-contained task to another installed agent by slug. The target agent keeps its own identity, model, memory, tools, scopes, and abilities."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Target installed agent slug or name."
                },
                "task": {
                    "type": "string",
                    "description": "A self-contained task for the target agent. Include all relevant context, constraints, and expected output."
                }
            },
            "required": ["agent", "task"],
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
        let parsed: DelegateToArgs = serde_json::from_value(args)?;
        if parsed.agent.trim().is_empty() {
            return Ok(error("agent is required"));
        }
        if parsed.task.trim().is_empty() {
            return Ok(error("task is required"));
        }

        let target_slug = Slug::derive_with_fallback(parsed.agent.trim(), "agent");
        let parent_slug = self.instance.agent_slug().clone();
        if target_slug == parent_slug {
            return Ok(error("cannot delegate to the current agent"));
        }

        let Some(provider) = self.instance.runtime.provider_runtime.clone() else {
            return Ok(error("delegation requires provider runtime support"));
        };
        let Some(target_agent) = provider.find_agent_manifest(&target_slug).cloned() else {
            return Ok(error(format!("unknown agent '{target_slug}'")));
        };

        let delegation_ctx = self
            .instance
            .runtime
            .sub_agent_ctx
            .clone()
            .unwrap_or_else(|| {
                crate::types::DelegationContext::new(
                    self.instance.runtime.config.max_delegation_depth,
                )
            });
        if delegation_ctx.would_cycle(&target_slug) {
            return Ok(error(format!(
                "delegating to '{target_slug}' would create a cycle"
            )));
        }
        let Some(child_ctx) = delegation_ctx.child(&parent_slug) else {
            return Ok(error("delegation depth limit reached"));
        };

        let project = active_project(&self.instance.prompt.context.current_project);
        start_delegation_operation(StartDelegationOperation {
            instance: &self.instance,
            provider,
            target_agent_name: target_agent.name.clone(),
            target_slug,
            task: parsed.task,
            child_ctx,
            project,
            workspace_dir: self.instance.runtime.security.workspace_dir.clone(),
        })
        .await
    }
}

struct UpdateDelegationParentTool {
    handle: AsyncOpChildHandle,
}

struct AskDelegationParentTool {
    handle: AsyncOpChildHandle,
}

#[async_trait::async_trait]
impl Tool for UpdateDelegationParentTool {
    fn name(&self) -> &str {
        "update_parent_agent"
    }

    fn description(&self) -> &str {
        "Send a compact delegation progress update to the parent agent without waking it immediately."
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
                error: Some("delegation operation was stopped".into()),
            });
        }
        self.handle
            .progress(
                parsed.summary,
                parsed.details,
                turn_loop::current_events_tx(),
            )
            .await;
        Ok(ok(serde_json::json!({ "status": "delivered" })))
    }
}

#[async_trait::async_trait]
impl Tool for AskDelegationParentTool {
    fn name(&self) -> &str {
        "ask_parent_agent"
    }

    fn description(&self) -> &str {
        "Ask the parent agent for input and wait until it responds with send_operation_input."
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
                turn_loop::current_events_tx(),
            )
            .await
        {
            Some(message) => Ok(ok(serde_json::json!({ "message": message }))),
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("parent did not provide input before the operation ended".into()),
            }),
        }
    }
}

struct StartDelegationOperation<'a, P: ProviderRuntime> {
    instance: &'a Arc<AgentInstance<P>>,
    provider: P,
    target_agent_name: String,
    target_slug: Slug,
    task: String,
    child_ctx: crate::types::DelegationContext,
    project: Option<ProjectManifest>,
    workspace_dir: PathBuf,
}

async fn start_delegation_operation<P>(
    params: StartDelegationOperation<'_, P>,
) -> Result<ToolResult>
where
    P: ProviderRuntime,
{
    let StartDelegationOperation {
        instance,
        provider,
        target_agent_name,
        target_slug,
        task,
        child_ctx,
        project,
        workspace_dir,
    } = params;

    debug!(
        caller = instance.name(),
        target = target_slug.as_str(),
        "Delegating to installed agent"
    );

    let operation_id = next_delegation_operation_id(&target_slug);
    let parent_events_tx = turn_loop::current_events_tx();
    let started = instance
        .runtime
        .async_ops
        .start(
            StartAsyncOp {
                id: operation_id.clone(),
                kind: AsyncOpKind::Delegation,
                label: target_agent_name.clone(),
                parent_operation_id: None,
                parent_tool_name: Some(DELEGATE_TO_TOOL_NAME.into()),
                started_summary: task.clone(),
                model_visible: true,
            },
            parent_events_tx.clone(),
        )
        .await;

    let child_handle = started.child.clone();
    let op_handle = started.handle.clone();
    let join_operation_id = operation_id.to_string();
    let target_agent_slug = target_slug.to_string();
    let join = tokio::spawn(async move {
        run_delegation_operation(DelegationOperation {
            provider,
            target_slug,
            task,
            child_ctx,
            project,
            workspace_dir,
            child_handle,
            op_handle,
            operation_id: join_operation_id,
            parent_events_tx,
        })
        .await;
    });
    instance
        .runtime
        .async_ops
        .attach_join(&operation_id, join)
        .await;

    Ok(ok(serde_json::json!({
        "operation_id": operation_id.to_string(),
        "agent": target_agent_slug,
        "status": "running",
        "control_tools": {
            "inspect": "inspect_operations",
            "send_input": "send_operation_input",
            "stop": "stop_operations",
            "wait": "wait_operations"
        }
    })))
}

struct DelegationOperation<P: ProviderRuntime> {
    provider: P,
    target_slug: Slug,
    task: String,
    child_ctx: crate::types::DelegationContext,
    project: Option<ProjectManifest>,
    workspace_dir: PathBuf,
    child_handle: AsyncOpChildHandle,
    op_handle: AsyncOpHandle,
    operation_id: String,
    parent_events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
}

async fn run_delegation_operation<P>(operation: DelegationOperation<P>)
where
    P: ProviderRuntime,
{
    let result = run_delegation_turn(&operation).await;
    match result {
        Ok(output) => {
            turn_loop::record_nested_token_usage(output.input_tokens, output.output_tokens);
            operation
                .op_handle
                .complete(
                    AsyncOpSignal::Completed {
                        summary: truncate(&output.text, 500),
                        output: Some(serde_json::json!({
                            "result_preview": output.text,
                        })),
                    },
                    operation.parent_events_tx.clone(),
                )
                .await;
        }
        Err(err) => {
            operation
                .op_handle
                .complete(
                    AsyncOpSignal::Failed {
                        error: truncate(&format!("delegation failed: {err}"), 500),
                    },
                    operation.parent_events_tx.clone(),
                )
                .await;
        }
    }
}

async fn run_delegation_turn<P>(operation: &DelegationOperation<P>) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let mut builder = operation
        .provider
        .agent(&operation.target_slug)
        .await?
        .with_child_delegation_ctx(operation.child_ctx.clone())
        .with_work_dir(operation.workspace_dir.clone())
        .with_execution_mode(AgentExecutionMode::DelegatedChild);
    if let Some(project) = &operation.project {
        builder = builder.with_project_context(project);
    }
    let runner = builder.build().await?;
    let mut task = TaskInput::new("Delegated task", operation.task.clone());
    if let Some(project) = &operation.project {
        task = task.with_project(project.slug.clone());
    }
    let mut handle = runner
        .task_stream_as_delegated_agent(task, operation.child_handle.clone())
        .await?;
    let cancel_token = operation.op_handle.cancel_token();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                handle.abort();
                anyhow::bail!("delegation operation stopped");
            }
            event = handle.recv() => {
                let Some(event) = event else {
                    break;
                };
                bridge_delegation_event(operation, event).await;
            }
        }
    }

    handle.output().await
}

async fn bridge_delegation_event<P>(operation: &DelegationOperation<P>, event: TurnEvent)
where
    P: ProviderRuntime,
{
    bridge_delegation_transcript(
        &operation.op_handle,
        &event,
        operation.parent_events_tx.clone(),
    )
    .await;
    let Some(parent_tx) = operation.parent_events_tx.clone() else {
        return;
    };
    match event {
        TurnEvent::ToolCallStart {
            batch_id,
            parent_tool_name,
            calls,
        } => {
            let _ = parent_tx.send(TurnEvent::ToolCallStart {
                batch_id,
                parent_tool_name: parent_tool_name.or_else(|| Some(operation.operation_id.clone())),
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
                parent_tool_name: parent_tool_name.or_else(|| Some(operation.operation_id.clone())),
                tool_call_id,
                tool_name,
                tool_args,
                result,
                metadata,
            });
        }
        TurnEvent::ModelRequestStarted {
            request_id,
            parent_call_id,
            provider,
            model,
        } => {
            let _ = parent_tx.send(TurnEvent::ModelRequestStarted {
                request_id,
                parent_call_id: parent_call_id.or_else(|| Some(operation.operation_id.clone())),
                provider,
                model,
            });
        }
        TurnEvent::ModelRequestCompleted {
            request_id,
            parent_call_id,
        } => {
            let _ = parent_tx.send(TurnEvent::ModelRequestCompleted {
                request_id,
                parent_call_id: parent_call_id.or_else(|| Some(operation.operation_id.clone())),
            });
        }
        TurnEvent::AsyncOperationEvent {
            operation_id,
            kind,
            label,
            parent_operation_id,
            parent_tool_name,
            status,
            signal,
            summary,
            payload,
            model_visible,
        } => {
            let _ = parent_tx.send(TurnEvent::AsyncOperationEvent {
                operation_id,
                kind,
                label,
                parent_operation_id: parent_operation_id
                    .or_else(|| Some(operation.operation_id.clone())),
                parent_tool_name,
                status,
                signal,
                summary,
                payload,
                model_visible,
            });
        }
        TurnEvent::AsyncOperationTranscript {
            operation_id,
            kind,
            label,
            event,
        } => {
            let _ = parent_tx.send(TurnEvent::AsyncOperationTranscript {
                operation_id,
                kind,
                label,
                event,
            });
        }
        TurnEvent::AbilityStarted { .. }
        | TurnEvent::AbilityCompleted { .. }
        | TurnEvent::AssistantTextDelta { .. }
        | TurnEvent::AssistantResponse { .. }
        | TurnEvent::HookStarted { .. }
        | TurnEvent::HookActivated { .. }
        | TurnEvent::HookCompleted { .. }
        | TurnEvent::SubAgentEvent { .. }
        | TurnEvent::SubAgentTranscript { .. }
        | TurnEvent::MessageCompacted { .. }
        | TurnEvent::Paused
        | TurnEvent::Resumed
        | TurnEvent::Done { .. } => {
            let _ = parent_tx.send(event);
        }
        TurnEvent::TranscriptMessage { .. } => {}
    }
}

async fn bridge_delegation_transcript(
    handle: &AsyncOpHandle,
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
        | TurnEvent::ModelRequestCompleted { .. }
        | TurnEvent::AssistantResponse { .. }
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

fn active_project(project: &ProjectManifest) -> Option<ProjectManifest> {
    if project.name.trim().is_empty() {
        None
    } else {
        Some(project.clone())
    }
}

fn next_delegation_operation_id(target_slug: &Slug) -> AsyncOpId {
    let sequence = DELEGATION_OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    AsyncOpId::new(format!("delegation_{target_slug}_{sequence}"))
}

fn agent_summary(agent: &AgentManifest) -> DelegatableAgent<'_> {
    DelegatableAgent {
        slug: agent.slug.as_str(),
        description: agent.description.as_deref().unwrap_or_default(),
    }
}

fn ok(value: serde_json::Value) -> ToolResult {
    ToolResult {
        success: true,
        output: value.to_string(),
        error: None,
    }
}

fn error(message: impl Into<String>) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(message.into()),
    }
}
