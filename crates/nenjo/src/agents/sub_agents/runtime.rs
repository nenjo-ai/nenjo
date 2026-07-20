use std::collections::HashMap;
use std::sync::{Arc, Weak};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use crate::Slug;
use crate::agents::AgentExecutionMode;
use crate::agents::async_ops::{
    AsyncOpId, AsyncOpKind, AsyncOpManager, AsyncOpSignal, StartAsyncOp,
};
use crate::agents::runner::types::{TurnEvent, TurnOutput};
use crate::input::TaskInput;
use crate::manifest::{AgentManifest, ModelManifest, model_manifest_slug};
use crate::provider::ProviderRuntime;
use crate::tools::{AsyncControl, AsyncControls, AsyncOperationStartReceipt, Tool};
use crate::types::DelegationContext;

use super::error::SubAgentError;
use super::events::{SubAgentSignal, SubAgentStatus, SubAgentTranscriptEvent};
use super::format::ResultFormat;

const SUB_AGENT_TASK_TEMPLATE: &str = r#"Task:
{{ task.title }}

Instructions:
{{ task.description }}
"#;

#[derive(Debug, Clone)]
pub(crate) struct SubAgentLimits {
    pub(crate) max_depth: u32,
}

pub(crate) struct SubAgentRuntimeOptions {
    pub(crate) limits: SubAgentLimits,
    pub(crate) delegation_ctx: Option<DelegationContext>,
    pub(crate) async_ops: AsyncOpManager,
    pub(crate) events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
}

#[derive(Debug, Clone)]
pub(crate) struct SpawnRequest {
    pub(crate) agent_name: String,
    pub(crate) slug: Option<Slug>,
    pub(crate) prompt: Option<String>,
    pub(crate) task: TaskInput,
    pub(crate) context: Option<Value>,
    pub(crate) result_format: Option<ResultFormat>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SpawnedSubAgent {
    pub(crate) slug: String,
    pub(crate) agent: String,
    pub(crate) capabilities: SubAgentCapabilities,
    #[serde(flatten)]
    pub(crate) operation: AsyncOperationStartReceipt,
}

#[derive(Debug, Serialize)]
pub(crate) struct SubAgentCapabilities {
    pub(crate) mode: &'static str,
    pub(crate) inherited_host_tools: Vec<String>,
    pub(crate) child_tools: &'static [&'static str],
    pub(crate) note: &'static str,
}

pub(crate) struct SubAgentRuntime<P: ProviderRuntime> {
    inner: Arc<RuntimeInner<P>>,
}

struct RuntimeInner<P: ProviderRuntime> {
    provider: P,
    parent_agent_slug: Slug,
    parent_model_manifest: ModelManifest,
    inherited_host_tools: Vec<Arc<dyn Tool>>,
    delegation_ctx: DelegationContext,
    async_ops: AsyncOpManager,
    runs: Mutex<HashMap<Slug, Arc<SubAgentRun>>>,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
}

struct SubAgentRun {
    slug: Slug,
    agent_name: String,
    status: Mutex<SubAgentStatus>,
    cancel: CancellationToken,
    abort: Mutex<Option<AbortHandle>>,
}

#[derive(Clone)]
pub(crate) struct SubAgentHandle<P: ProviderRuntime> {
    inner: Arc<RuntimeInner<P>>,
}

pub(crate) struct ChildRuntimeHandle<P: ProviderRuntime> {
    inner: Weak<RuntimeInner<P>>,
    run: Weak<SubAgentRun>,
    async_op: super::super::async_ops::AsyncOpChildHandle,
}

impl<P: ProviderRuntime> Clone for ChildRuntimeHandle<P> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            run: self.run.clone(),
            async_op: self.async_op.clone(),
        }
    }
}

impl<P: ProviderRuntime> SubAgentRuntime<P> {
    pub(crate) fn new(
        provider: P,
        parent_agent_slug: Slug,
        parent_model_manifest: ModelManifest,
        inherited_host_tools: Vec<Arc<dyn Tool>>,
        options: SubAgentRuntimeOptions,
    ) -> Self {
        let delegation_ctx = options
            .delegation_ctx
            .unwrap_or_else(|| DelegationContext::new(options.limits.max_depth));
        Self {
            inner: Arc::new(RuntimeInner {
                provider,
                parent_agent_slug,
                parent_model_manifest,
                inherited_host_tools,
                delegation_ctx,
                async_ops: options.async_ops,
                runs: Mutex::new(HashMap::new()),
                events_tx: options.events_tx,
            }),
        }
    }

    pub(crate) fn handle(&self) -> SubAgentHandle<P> {
        SubAgentHandle {
            inner: self.inner.clone(),
        }
    }
}

impl<P: ProviderRuntime> Drop for RuntimeInner<P> {
    fn drop(&mut self) {
        if let Ok(runs) = self.runs.try_lock() {
            for run in runs.values() {
                run.cancel.cancel();
                if let Ok(mut abort) = run.abort.try_lock()
                    && let Some(handle) = abort.take()
                {
                    handle.abort();
                }
            }
        }
    }
}

impl<P: ProviderRuntime> RuntimeInner<P> {
    fn inherited_tool_names(&self) -> Vec<String> {
        self.inherited_host_tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }
}

impl<P: ProviderRuntime> SubAgentHandle<P> {
    pub(crate) async fn spawn_many(
        &self,
        requests: Vec<SpawnRequest>,
    ) -> Vec<Result<SpawnedSubAgent, SubAgentError>> {
        let mut results = Vec::with_capacity(requests.len());
        for request in requests {
            results.push(self.spawn_one(request).await);
        }
        results
    }

    async fn spawn_one(&self, request: SpawnRequest) -> Result<SpawnedSubAgent, SubAgentError> {
        let child_agent = ephemeral_agent_manifest(
            &request,
            model_manifest_slug(
                &self.inner.parent_model_manifest.model_provider,
                &self.inner.parent_model_manifest.model,
            ),
        )?;
        let child_ctx = self
            .inner
            .delegation_ctx
            .child(&self.inner.parent_agent_slug)
            .ok_or_else(|| SubAgentError::DepthLimit(request.agent_name.clone()))?;

        let slug = self.reserve_slug(&request).await?;
        let controls = AsyncControls::new(AsyncControl::Inspect)
            .with(AsyncControl::SendInput)
            .with(AsyncControl::Stop)
            .with(AsyncControl::Wait);
        let started = self
            .inner
            .async_ops
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new(slug.to_string()),
                    kind: AsyncOpKind::SubAgent,
                    label: request.agent_name.clone(),
                    parent_operation_id: None,
                    parent_tool_name: Some("spawn_sub_agents".into()),
                    started_summary: truncate(&request.task.title, 180),
                    model_visible: true,
                    controls,
                },
                self.inner.events_tx.clone(),
            )
            .await;
        let run = Arc::new(SubAgentRun {
            slug: slug.clone(),
            agent_name: request.agent_name.clone(),
            status: Mutex::new(SubAgentStatus::Running),
            cancel: started.handle.cancel_token(),
            abort: Mutex::new(None),
        });

        {
            let mut runs = self.inner.runs.lock().await;
            runs.insert(slug.clone(), run.clone());
        }

        self.push_signal(
            &run,
            SubAgentSignal::Started {
                task_summary: truncate(&request.task.title, 180),
            },
        )
        .await;

        let inner = self.inner.clone();
        let child_handle = ChildRuntimeHandle {
            inner: Arc::downgrade(&inner),
            run: Arc::downgrade(&run),
            async_op: started.child,
        };
        let provider = inner.provider.clone();
        let child_model_manifest = inner.parent_model_manifest.clone();
        let inherited_host_tools = inner.inherited_host_tools.clone();
        let task = build_child_task_input(&request);
        let result_format = request.result_format.clone();
        let completion_format = result_format.clone();
        let cancel = run.cancel.clone();

        let join = tokio::spawn(async move {
            let result = run_child_agent(ChildAgentRun {
                provider,
                agent: child_agent,
                model_manifest: child_model_manifest,
                inherited_host_tools,
                task,
                child_ctx,
                child_handle: child_handle.clone(),
                cancel,
            })
            .await;
            match result {
                Ok(output) => {
                    let (structured_result, result_format_valid) = completion_format
                        .as_ref()
                        .map(|format| format.validate_output(&output.text))
                        .unwrap_or((None, None));
                    child_handle
                        .complete(
                            SubAgentStatus::Completed,
                            SubAgentSignal::Completed {
                                summary: truncate(&output.text, 500),
                                structured_result,
                                result_format_valid,
                            },
                        )
                        .await;
                }
                Err(err) => {
                    child_handle
                        .complete(
                            SubAgentStatus::Failed,
                            SubAgentSignal::Failed {
                                error: truncate(&err.to_string(), 500),
                            },
                        )
                        .await;
                }
            }
        });
        *run.abort.lock().await = Some(join.abort_handle());
        started
            .handle
            .attach_join(join, self.inner.events_tx.clone())
            .await;

        Ok(SpawnedSubAgent {
            slug: slug.to_string(),
            agent: request.agent_name,
            capabilities: SubAgentCapabilities {
                mode: "isolated_ephemeral_child",
                inherited_host_tools: self.inner.inherited_tool_names(),
                child_tools: &["update_parent_agent", "ask_parent_agent"],
                note: "Child agents inherit parent host tools and scoped workspace access, but not sub-agent management tools or installed-agent abilities.",
            },
            operation: AsyncOperationStartReceipt::new(
                slug.to_string(),
                AsyncOpKind::SubAgent,
                controls,
            ),
        })
    }

    async fn reserve_slug(&self, request: &SpawnRequest) -> Result<Slug, SubAgentError> {
        let base = request
            .slug
            .clone()
            .unwrap_or_else(|| Slug::derive_with_fallback(&request.agent_name, "sub_agent"));
        let runs = self.inner.runs.lock().await;
        if !runs.contains_key(&base) {
            return Ok(base);
        }

        let mut suffix = 2usize;
        loop {
            let candidate = base.with_suffix(suffix);
            if !runs.contains_key(&candidate) {
                return Ok(candidate);
            }
            suffix = suffix
                .checked_add(1)
                .ok_or_else(|| SubAgentError::SlugExhausted(base.to_string()))?;
        }
    }

    async fn push_signal(&self, run: &SubAgentRun, signal: SubAgentSignal) {
        let event = TurnEvent::SubAgentEvent {
            slug: run.slug.to_string(),
            agent_name: run.agent_name.clone(),
            kind: signal.kind().to_string(),
            summary: signal_summary(&signal),
            model_visible: false,
        };
        if let Some(tx) = &self.inner.events_tx {
            let _ = tx.send(event);
        }
        let op_signal = match &signal {
            SubAgentSignal::Started { .. } => return,
            SubAgentSignal::Progress { summary, details } => AsyncOpSignal::Progress {
                summary: summary.clone(),
                details: details.clone(),
            },
            SubAgentSignal::NeedsInput { question, context } => AsyncOpSignal::NeedsInput {
                question: question.clone(),
                context: context.clone(),
            },
            SubAgentSignal::Completed {
                summary,
                structured_result,
                result_format_valid,
            } => AsyncOpSignal::Completed {
                summary: summary.clone(),
                output: Some(serde_json::json!({
                    "structured_result": structured_result,
                    "result_format_valid": result_format_valid,
                })),
            },
            SubAgentSignal::Failed { error } => AsyncOpSignal::Failed {
                error: error.clone(),
                output: None,
            },
            SubAgentSignal::Stopped { reason } => AsyncOpSignal::Stopped {
                reason: reason.clone(),
            },
        };
        if let Some(operation) = self
            .inner
            .async_ops
            .handle(&AsyncOpId::new(run.slug.to_string()))
            .await
        {
            operation
                .complete(op_signal, self.inner.events_tx.clone())
                .await;
        }
    }
}

impl<P: ProviderRuntime> ChildRuntimeHandle<P> {
    pub(crate) async fn progress(&self, summary: String, details: Option<String>) {
        let Some(handle) = self.parent() else {
            return;
        };
        let Some(run) = self.run() else {
            return;
        };
        handle
            .push_signal(
                &run,
                SubAgentSignal::Progress {
                    summary: truncate(&summary, 500),
                    details: details.map(|d| truncate(&d, 1000)),
                },
            )
            .await;
    }

    pub(crate) async fn ask(&self, question: String, context: Option<String>) -> Option<String> {
        let run = self.run()?;
        *run.status.lock().await = SubAgentStatus::WaitingForInput;
        let handle = self.parent()?;
        handle
            .push_signal(
                &run,
                SubAgentSignal::NeedsInput {
                    question: truncate(&question, 500),
                    context: context.map(|c| truncate(&c, 1000)),
                },
            )
            .await;
        let next = self.async_op.receive_input().await;
        *run.status.lock().await = SubAgentStatus::Running;
        next
    }

    pub(crate) async fn transcript(&self, event: SubAgentTranscriptEvent) {
        let Some(run) = self.run() else {
            return;
        };
        if let Some(handle) = self.parent()
            && let Some(tx) = &handle.inner.events_tx
        {
            let _ = tx.send(TurnEvent::SubAgentTranscript {
                slug: run.slug.to_string(),
                agent_name: run.agent_name.clone(),
                event: event.clone(),
            });
        }
        self.async_op
            .transcript(
                event,
                self.parent()
                    .and_then(|handle| handle.inner.events_tx.clone()),
            )
            .await;
    }

    async fn complete(&self, status: SubAgentStatus, signal: SubAgentSignal) {
        let Some(run) = self.run() else {
            return;
        };
        let mut current = run.status.lock().await;
        if run.cancel.is_cancelled() {
            *current = SubAgentStatus::Stopped;
            return;
        }
        if matches!(*current, SubAgentStatus::Stopped) {
            return;
        }
        *current = status;
        drop(current);
        if let Some(handle) = self.parent() {
            handle.push_signal(&run, signal).await;
        }
    }

    fn parent(&self) -> Option<SubAgentHandle<P>> {
        self.inner.upgrade().map(|inner| SubAgentHandle { inner })
    }

    fn run(&self) -> Option<Arc<SubAgentRun>> {
        self.run.upgrade()
    }
}

struct ChildAgentRun<P: ProviderRuntime> {
    provider: P,
    agent: AgentManifest,
    model_manifest: ModelManifest,
    inherited_host_tools: Vec<Arc<dyn Tool>>,
    task: TaskInput,
    child_ctx: DelegationContext,
    child_handle: ChildRuntimeHandle<P>,
    cancel: CancellationToken,
}

async fn run_child_agent<P: ProviderRuntime>(run: ChildAgentRun<P>) -> Result<TurnOutput> {
    let builder = run
        .provider
        .new_agent()
        .with_agent_manifest(run.agent)
        .with_model_manifest(run.model_manifest)
        .with_tools(run.inherited_host_tools)
        .with_child_delegation_ctx(run.child_ctx)
        .with_execution_mode(AgentExecutionMode::EphemeralChild);
    let runner = builder.build().await?;
    let mut handle = runner
        .task_stream_as_sub_agent(run.task, run.child_handle.clone())
        .await?;

    loop {
        tokio::select! {
            _ = run.cancel.cancelled() => {
                handle.abort();
                return Err(anyhow::anyhow!("sub-agent stopped"));
            }
            event = handle.recv() => {
                let Some(event) = event else {
                    break;
                };
                bridge_transcript(&run.child_handle, event).await;
            }
        }
    }
    handle.output().await
}

fn ephemeral_agent_manifest(
    request: &SpawnRequest,
    model: crate::Slug,
) -> Result<AgentManifest, SubAgentError> {
    let prompt = request
        .prompt
        .as_ref()
        .filter(|prompt| !prompt.trim().is_empty())
        .map(|prompt| prompt.trim().to_string())
        .unwrap_or_else(|| format!("You are {}.", request.agent_name));

    AgentManifest::builder()
        .with_name(request.agent_name.clone())
        .with_slug(
            request
                .slug
                .clone()
                .unwrap_or_else(|| Slug::derive_with_fallback(&request.agent_name, "sub_agent")),
        )
        .with_model(model)
        .with_system_prompt(prompt)
        .with_developer_prompt(
            "You are an isolated sub-agent worker. Work only on the assigned task, report progress to the parent when useful, and return a focused final result. You inherit the parent agent's host tools and scoped workspace access for this run. You also have update_parent_agent and ask_parent_agent. You do not inherit sub-agent management tools, installed-agent abilities, or unrelated domains. Use inherited tools only within the assigned task and report any mutations or evidence clearly to the parent.",
        )
        .with_task_template(SUB_AGENT_TASK_TEMPLATE)
        .build()
        .map_err(|err| SubAgentError::ManifestBuild {
            agent: request.agent_name.clone(),
            reason: err.to_string(),
        })
}

async fn bridge_transcript<P: ProviderRuntime>(child: &ChildRuntimeHandle<P>, event: TurnEvent) {
    match event {
        TurnEvent::ToolCallStart { calls, .. } => {
            for call in calls {
                child
                    .transcript(SubAgentTranscriptEvent::ToolCall {
                        tool: call.tool_name,
                        summary: call
                            .text_preview
                            .unwrap_or_else(|| truncate(&call.tool_args, 240)),
                    })
                    .await;
            }
        }
        TurnEvent::ToolCallEnd {
            tool_name, result, ..
        } => {
            child
                .transcript(SubAgentTranscriptEvent::ToolResult {
                    tool: tool_name,
                    success: result.success,
                    summary: truncate(
                        result.error.as_deref().unwrap_or(result.output.as_str()),
                        240,
                    ),
                })
                .await;
        }
        TurnEvent::TranscriptMessage { message } => {
            let summary = truncate(&message.content, 240);
            let event = match message.role.as_str() {
                "user" => SubAgentTranscriptEvent::Input { summary },
                "assistant" => SubAgentTranscriptEvent::AssistantMessage { summary },
                "tool" => SubAgentTranscriptEvent::ToolResult {
                    tool: "tool".into(),
                    success: true,
                    summary,
                },
                _ => return,
            };
            child.transcript(event).await;
        }
        TurnEvent::AbilityStarted { .. }
        | TurnEvent::AbilityCompleted { .. }
        | TurnEvent::ModelRequestStarted { .. }
        | TurnEvent::AssistantTextDelta { .. }
        | TurnEvent::AssistantResponse { .. }
        | TurnEvent::ModelRequestCompleted { .. }
        | TurnEvent::HookActivated { .. }
        | TurnEvent::HookStarted { .. }
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

fn build_child_task_input(request: &SpawnRequest) -> TaskInput {
    let mut task = request.task.clone();
    task.title = task.title.trim().to_string();
    let mut instructions = task.instructions.trim().to_string();
    if let Some(context) = &request.context {
        instructions.push_str("\n\nContext metadata:\n");
        instructions.push_str(
            &serde_json::to_string_pretty(context).unwrap_or_else(|_| context.to_string()),
        );
    }

    if let Some(format) = &request.result_format {
        instructions.push_str("\n\nOutput format:\n");
        instructions.push_str(format.instructions().trim());
    }

    task.instructions = instructions;
    task.with_project("sub_agent")
}

fn signal_summary(signal: &SubAgentSignal) -> String {
    match signal {
        SubAgentSignal::Started { task_summary } => task_summary.clone(),
        SubAgentSignal::Progress { summary, .. } => summary.clone(),
        SubAgentSignal::NeedsInput { question, .. } => question.clone(),
        SubAgentSignal::Completed { summary, .. } => summary.clone(),
        SubAgentSignal::Failed { error } => error.clone(),
        SubAgentSignal::Stopped { reason } => reason.clone().unwrap_or_else(|| "stopped".into()),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}
