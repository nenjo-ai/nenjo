use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::AgentExecutionMode;
use crate::agents::runner::types::{TurnEvent, TurnOutput};
use crate::input::{AgentRun, ChatInput};
use crate::provider::ProviderRuntime;
use crate::types::DelegationContext;

use super::error::SubAgentError;
use super::events::{
    SignalDigest, SubAgentSignal, SubAgentStatus, SubAgentTranscriptEvent, push_bounded,
};
use super::format::ResultFormat;
use super::slug::SubAgentSlug;

const SIGNAL_QUEUE_CAP: usize = 128;
const TRANSCRIPT_CAP: usize = 256;
const WAIT_EVENTS_PER_AGENT: usize = 12;
const INSPECT_LIMIT_CAP: usize = 50;

#[derive(Debug, Clone)]
pub(crate) struct SubAgentLimits {
    pub(crate) max_depth: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct SpawnRequest {
    pub(crate) agent_name: String,
    pub(crate) slug: Option<SubAgentSlug>,
    pub(crate) prompt: Option<String>,
    pub(crate) task: SubAgentTask,
    pub(crate) context: Option<Value>,
    pub(crate) result_format: Option<ResultFormat>,
}

#[derive(Debug, Clone)]
pub(crate) struct SubAgentTask {
    pub(crate) description: String,
    pub(crate) goal: String,
    pub(crate) acceptance_criteria: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SpawnedSubAgent {
    pub(crate) slug: String,
    pub(crate) agent: String,
    pub(crate) status: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeliveryResult {
    pub(crate) slug: String,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct StoppedSubAgent {
    pub(crate) slug: String,
    pub(crate) status: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectedSubAgent {
    pub(crate) slug: String,
    pub(crate) agent: String,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest_signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transcript_delta: Option<Vec<SubAgentTranscriptEvent>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WaitResult {
    pub(crate) elapsed_seconds: u64,
    pub(crate) woken_by: &'static str,
    pub(crate) updates: Vec<SignalDigest>,
}

pub(crate) struct SubAgentRuntime<P: ProviderRuntime> {
    inner: Arc<RuntimeInner<P>>,
}

struct RuntimeInner<P: ProviderRuntime> {
    provider: P,
    parent_agent_id: Uuid,
    delegation_ctx: DelegationContext,
    runs: Mutex<HashMap<SubAgentSlug, Arc<SubAgentRun>>>,
    notify: Notify,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
}

struct SubAgentRun {
    slug: SubAgentSlug,
    agent_name: String,
    status: Mutex<SubAgentStatus>,
    signals: Mutex<VecDeque<SubAgentSignal>>,
    transcript: Mutex<TranscriptState>,
    inbox_tx: mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
    join: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Default)]
struct TranscriptState {
    events: VecDeque<SubAgentTranscriptEvent>,
    inspect_cursor: usize,
    total_seen: usize,
}

#[derive(Clone)]
pub(crate) struct SubAgentHandle<P: ProviderRuntime> {
    inner: Arc<RuntimeInner<P>>,
}

pub(crate) struct ChildRuntimeHandle<P: ProviderRuntime> {
    inner: Weak<RuntimeInner<P>>,
    run: Weak<SubAgentRun>,
    inbox_rx: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
}

impl<P: ProviderRuntime> Clone for ChildRuntimeHandle<P> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            run: self.run.clone(),
            inbox_rx: self.inbox_rx.clone(),
        }
    }
}

impl<P: ProviderRuntime> SubAgentRuntime<P> {
    pub(crate) fn new(
        provider: P,
        parent_agent_id: Uuid,
        limits: SubAgentLimits,
        delegation_ctx: Option<DelegationContext>,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) -> Self {
        let delegation_ctx =
            delegation_ctx.unwrap_or_else(|| DelegationContext::new(limits.max_depth));
        Self {
            inner: Arc::new(RuntimeInner {
                provider,
                parent_agent_id,
                delegation_ctx,
                runs: Mutex::new(HashMap::new()),
                notify: Notify::new(),
                events_tx,
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
                if let Ok(mut join) = run.join.try_lock()
                    && let Some(handle) = join.take()
                {
                    handle.abort();
                }
            }
        }
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
        let target = self
            .inner
            .provider
            .find_agent_manifest(&request.agent_name)
            .cloned()
            .ok_or_else(|| SubAgentError::AgentNotFound(request.agent_name.clone()))?;
        if self.inner.delegation_ctx.would_cycle(target.id)
            || target.id == self.inner.parent_agent_id
        {
            return Err(SubAgentError::Cycle(request.agent_name));
        }
        let child_ctx = self
            .inner
            .delegation_ctx
            .child(self.inner.parent_agent_id)
            .ok_or_else(|| SubAgentError::DepthLimit(request.agent_name.clone()))?;

        let slug = self.reserve_slug(&request).await;
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();
        let run = Arc::new(SubAgentRun {
            slug: slug.clone(),
            agent_name: request.agent_name.clone(),
            status: Mutex::new(SubAgentStatus::Running),
            signals: Mutex::new(VecDeque::new()),
            transcript: Mutex::new(TranscriptState::default()),
            inbox_tx,
            cancel: CancellationToken::new(),
            join: Mutex::new(None),
        });

        {
            let mut runs = self.inner.runs.lock().await;
            runs.insert(slug.clone(), run.clone());
        }

        self.push_signal(
            &run,
            SubAgentSignal::Started {
                task_summary: truncate(&request.task.description, 180),
            },
            false,
        )
        .await;

        let inner = self.inner.clone();
        let child_handle = ChildRuntimeHandle {
            inner: Arc::downgrade(&inner),
            run: Arc::downgrade(&run),
            inbox_rx: Arc::new(Mutex::new(inbox_rx)),
        };
        let provider = inner.provider.clone();
        let agent_name = request.agent_name.clone();
        let task = build_child_task(&request);
        let result_format = request.result_format.clone();
        let completion_format = result_format.clone();
        let cancel = run.cancel.clone();

        let join = tokio::spawn(async move {
            let result = run_child_agent(
                provider,
                agent_name.clone(),
                task,
                child_ctx,
                child_handle.clone(),
                result_format,
                cancel,
            )
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
        *run.join.lock().await = Some(join);

        Ok(SpawnedSubAgent {
            slug: slug.to_string(),
            agent: request.agent_name,
            status: SubAgentStatus::Running.as_str(),
        })
    }

    async fn reserve_slug(&self, request: &SpawnRequest) -> SubAgentSlug {
        let base = request
            .slug
            .clone()
            .unwrap_or_else(|| SubAgentSlug::derive(&request.agent_name));
        let runs = self.inner.runs.lock().await;
        if !runs.contains_key(&base) {
            return base;
        }
        for suffix in 2.. {
            let candidate = base.with_suffix(suffix);
            if !runs.contains_key(&candidate) {
                return candidate;
            }
        }
        unreachable!("unbounded suffix search must find an available slug")
    }

    pub(crate) async fn send(&self, messages: Vec<(SubAgentSlug, String)>) -> Vec<DeliveryResult> {
        let mut results = Vec::with_capacity(messages.len());
        for (slug, message) in messages {
            let Some(run) = self.find(&slug).await else {
                results.push(DeliveryResult {
                    slug: slug.to_string(),
                    status: "not_delivered",
                    reason: Some("sub-agent not found".into()),
                });
                continue;
            };
            let status = *run.status.lock().await;
            if !status.can_receive_input() {
                results.push(DeliveryResult {
                    slug: slug.to_string(),
                    status: "not_delivered",
                    reason: Some(format!("sub-agent is {}", status.as_str())),
                });
                continue;
            }
            if run.inbox_tx.send(message).is_ok() {
                if status == SubAgentStatus::WaitingForInput {
                    *run.status.lock().await = SubAgentStatus::Running;
                }
                results.push(DeliveryResult {
                    slug: slug.to_string(),
                    status: "delivered",
                    reason: None,
                });
            } else {
                results.push(DeliveryResult {
                    slug: slug.to_string(),
                    status: "not_delivered",
                    reason: Some("sub-agent inbox is closed".into()),
                });
            }
        }
        results
    }

    pub(crate) async fn stop(
        &self,
        slugs: Vec<SubAgentSlug>,
        reason: Option<String>,
    ) -> Vec<StoppedSubAgent> {
        let mut stopped = Vec::with_capacity(slugs.len());
        for slug in slugs {
            let Some(run) = self.find(&slug).await else {
                continue;
            };
            run.cancel.cancel();
            if let Some(handle) = run.join.lock().await.take() {
                handle.abort();
            }
            *run.status.lock().await = SubAgentStatus::Stopped;
            self.push_signal(
                &run,
                SubAgentSignal::Stopped {
                    reason: reason.clone(),
                },
                true,
            )
            .await;
            stopped.push(StoppedSubAgent {
                slug: slug.to_string(),
                status: SubAgentStatus::Stopped.as_str(),
            });
        }
        stopped
    }

    pub(crate) async fn inspect(
        &self,
        slugs: Vec<SubAgentSlug>,
        include_transcript: bool,
        limit: usize,
    ) -> Vec<InspectedSubAgent> {
        let limit = limit.clamp(1, INSPECT_LIMIT_CAP);
        let selected = if slugs.is_empty() {
            self.inner
                .runs
                .lock()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>()
        } else {
            let mut runs = Vec::new();
            for slug in slugs {
                if let Some(run) = self.find(&slug).await {
                    runs.push(run);
                }
            }
            runs
        };

        let mut inspected = Vec::with_capacity(selected.len());
        for run in selected {
            let status = *run.status.lock().await;
            let latest_signal = run.signals.lock().await.back().map(signal_summary);
            let transcript_delta = if include_transcript {
                let mut transcript = run.transcript.lock().await;
                let start = transcript.inspect_cursor.saturating_sub(
                    transcript
                        .total_seen
                        .saturating_sub(transcript.events.len()),
                );
                let delta = transcript
                    .events
                    .iter()
                    .skip(start)
                    .take(limit)
                    .cloned()
                    .collect::<Vec<_>>();
                transcript.inspect_cursor = transcript.total_seen;
                Some(delta)
            } else {
                None
            };
            inspected.push(InspectedSubAgent {
                slug: run.slug.to_string(),
                agent: run.agent_name.clone(),
                status: status.as_str(),
                latest_signal,
                transcript_delta,
            });
        }
        inspected
    }

    pub(crate) async fn wait(&self, seconds: u64) -> WaitResult {
        let seconds = seconds.clamp(1, 30);
        let started = Instant::now();
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(seconds)) => {}
            _ = self.inner.notify.notified() => {}
        }
        let updates = self.drain_signals().await;
        let woken_by = classify_wake(&updates);
        WaitResult {
            elapsed_seconds: started.elapsed().as_secs(),
            woken_by,
            updates,
        }
    }

    async fn drain_signals(&self) -> Vec<SignalDigest> {
        let runs = self
            .inner
            .runs
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut updates = Vec::new();
        for run in runs {
            let mut signals = run.signals.lock().await;
            if signals.is_empty() {
                continue;
            }
            let mut events = Vec::new();
            while let Some(signal) = signals.pop_front() {
                if matches!(signal, SubAgentSignal::Progress { .. })
                    && events.len() >= WAIT_EVENTS_PER_AGENT
                {
                    continue;
                }
                events.push(signal);
            }
            if !events.is_empty() {
                updates.push(SignalDigest {
                    slug: run.slug.to_string(),
                    events,
                });
            }
        }
        updates
    }

    async fn find(&self, slug: &SubAgentSlug) -> Option<Arc<SubAgentRun>> {
        self.inner.runs.lock().await.get(slug).cloned()
    }

    async fn push_signal(&self, run: &SubAgentRun, signal: SubAgentSignal, wake: bool) {
        let should_wake = wake || signal.wakes_parent();
        let event = TurnEvent::SubAgentEvent {
            slug: run.slug.to_string(),
            agent_name: run.agent_name.clone(),
            kind: signal.kind().to_string(),
            summary: signal_summary(&signal),
            model_visible: false,
        };
        let mut signals = run.signals.lock().await;
        push_bounded(&mut signals, signal, SIGNAL_QUEUE_CAP);
        drop(signals);
        if let Some(tx) = &self.inner.events_tx {
            let _ = tx.send(event);
        }
        if should_wake {
            self.inner.notify.notify_waiters();
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
                false,
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
                true,
            )
            .await;
        let next = self.inbox_rx.lock().await.recv().await;
        *run.status.lock().await = SubAgentStatus::Running;
        next
    }

    pub(crate) async fn transcript(&self, event: SubAgentTranscriptEvent) {
        let Some(run) = self.run() else {
            return;
        };
        let mut transcript = run.transcript.lock().await;
        push_bounded(&mut transcript.events, event.clone(), TRANSCRIPT_CAP);
        transcript.total_seen += 1;
        drop(transcript);
        if let Some(handle) = self.parent()
            && let Some(tx) = &handle.inner.events_tx
        {
            let _ = tx.send(TurnEvent::SubAgentTranscript {
                slug: run.slug.to_string(),
                agent_name: run.agent_name.clone(),
                event,
            });
        }
    }

    async fn complete(&self, status: SubAgentStatus, signal: SubAgentSignal) {
        let Some(run) = self.run() else {
            return;
        };
        let mut current = run.status.lock().await;
        if matches!(*current, SubAgentStatus::Stopped) {
            return;
        }
        *current = status;
        drop(current);
        if let Some(handle) = self.parent() {
            handle.push_signal(&run, signal, true).await;
        }
    }

    fn parent(&self) -> Option<SubAgentHandle<P>> {
        self.inner.upgrade().map(|inner| SubAgentHandle { inner })
    }

    fn run(&self) -> Option<Arc<SubAgentRun>> {
        self.run.upgrade()
    }
}

async fn run_child_agent<P: ProviderRuntime>(
    provider: P,
    agent_name: String,
    task: String,
    child_ctx: DelegationContext,
    child_handle: ChildRuntimeHandle<P>,
    _result_format: Option<ResultFormat>,
    cancel: CancellationToken,
) -> Result<TurnOutput> {
    let builder = provider
        .build_agent_by_name(&agent_name)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?
        .with_child_delegation_ctx(child_ctx)
        .with_execution_mode(AgentExecutionMode::Child);
    let runner = builder.build().await?;
    let mut handle = runner
        .chat_stream_as_sub_agent(&task, child_handle.clone())
        .await?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                handle.abort();
                return Err(anyhow::anyhow!("sub-agent stopped"));
            }
            event = handle.recv() => {
                let Some(event) = event else {
                    break;
                };
                bridge_transcript(&child_handle, event).await;
            }
        }
    }
    handle.output().await
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
        | TurnEvent::SubAgentEvent { .. }
        | TurnEvent::SubAgentTranscript { .. }
        | TurnEvent::MessageCompacted { .. }
        | TurnEvent::Paused
        | TurnEvent::Resumed
        | TurnEvent::Done { .. } => {}
    }
}

fn build_child_task(request: &SpawnRequest) -> String {
    let mut task = String::new();
    if let Some(prompt) = request
        .prompt
        .as_ref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        task.push_str("Prompt:\n");
        task.push_str(prompt.trim());
        task.push_str("\n\n");
    }

    task.push_str("Task:\n");
    task.push_str("Description:\n");
    task.push_str(request.task.description.trim());
    task.push_str("\n\nGoal:\n");
    task.push_str(request.task.goal.trim());

    if !request.task.acceptance_criteria.is_empty() {
        task.push_str("\n\nAcceptance criteria:\n");
        for criterion in &request.task.acceptance_criteria {
            task.push_str("- ");
            task.push_str(criterion.trim());
            task.push('\n');
        }
    }

    if let Some(context) = &request.context {
        task.push_str("\n\nContext metadata:\n");
        task.push_str(
            &serde_json::to_string_pretty(context).unwrap_or_else(|_| context.to_string()),
        );
    }
    if let Some(format) = &request.result_format {
        task.push_str(&format.instructions());
    }
    task
}

fn classify_wake(updates: &[SignalDigest]) -> &'static str {
    let has = |kind| {
        updates
            .iter()
            .flat_map(|digest| digest.events.iter())
            .any(|signal| signal.kind() == kind)
    };
    if has("needs_input") {
        "needs_input"
    } else if has("completed") || has("failed") {
        "sub_agent_result"
    } else if has("stopped") {
        "stopped"
    } else {
        "timeout"
    }
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

impl From<SpawnRequest> for AgentRun {
    fn from(value: SpawnRequest) -> Self {
        AgentRun::chat(ChatInput {
            message: build_child_task(&value),
            history: Vec::new(),
            project_id: None,
        })
    }
}
