//! Shared runtime for long-running agent operations.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Weak};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::tools::{AsyncOperationKind, AsyncOperationSignalKind};
pub(crate) use crate::tools::{
    AsyncOperationKind as AsyncOpKind, AsyncOperationStatus as AsyncOpStatus,
};

use super::runner::turn_loop;
use super::runner::types::{AsyncOperationTranscriptEvent, TurnEvent};

const SIGNAL_QUEUE_CAP: usize = 128;
const TRANSCRIPT_CAP: usize = 256;
const WAIT_EVENTS_PER_OPERATION: usize = 12;
const INSPECT_LIMIT_CAP: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct AsyncOpId(String);

impl AsyncOpId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for AsyncOpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum AsyncOpSignal {
    Started {
        summary: String,
    },
    Progress {
        summary: String,
        details: Option<String>,
    },
    NeedsInput {
        question: String,
        context: Option<String>,
    },
    Completed {
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
    },
    Failed {
        error: String,
    },
    Stopped {
        reason: Option<String>,
    },
}

impl AsyncOpSignal {
    pub(crate) fn kind(&self) -> &'static str {
        self.signal_kind().as_str()
    }

    pub(crate) fn signal_kind(&self) -> AsyncOperationSignalKind {
        match self {
            Self::Started { .. } => AsyncOperationSignalKind::Started,
            Self::Progress { .. } => AsyncOperationSignalKind::Progress,
            Self::NeedsInput { .. } => AsyncOperationSignalKind::NeedsInput,
            Self::Completed { .. } => AsyncOperationSignalKind::Completed,
            Self::Failed { .. } => AsyncOperationSignalKind::Failed,
            Self::Stopped { .. } => AsyncOperationSignalKind::Stopped,
        }
    }

    pub(crate) fn status(&self) -> AsyncOpStatus {
        match self {
            Self::NeedsInput { .. } => AsyncOpStatus::WaitingForInput,
            Self::Completed { .. } => AsyncOpStatus::Completed,
            Self::Failed { .. } => AsyncOpStatus::Failed,
            Self::Stopped { .. } => AsyncOpStatus::Stopped,
            Self::Started { .. } | Self::Progress { .. } => AsyncOpStatus::Running,
        }
    }

    pub(crate) fn wakes_parent(&self) -> bool {
        matches!(
            self,
            Self::NeedsInput { .. }
                | Self::Completed { .. }
                | Self::Failed { .. }
                | Self::Stopped { .. }
        )
    }

    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Started { summary }
            | Self::Progress { summary, .. }
            | Self::Completed { summary, .. } => summary.clone(),
            Self::NeedsInput { question, .. } => question.clone(),
            Self::Failed { error } => error.clone(),
            Self::Stopped { reason } => reason.clone().unwrap_or_else(|| "stopped".into()),
        }
    }

    pub(crate) fn payload(&self) -> Option<Value> {
        match self {
            Self::Progress { details, .. } => details
                .as_ref()
                .map(|details| serde_json::json!({ "details": details })),
            Self::NeedsInput { context, .. } => context
                .as_ref()
                .map(|context| serde_json::json!({ "context": context })),
            Self::Completed { output, .. } => output.clone(),
            Self::Started { .. } | Self::Failed { .. } | Self::Stopped { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AsyncOpSignalDigest {
    pub(crate) operation_id: String,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) status: &'static str,
    pub(crate) events: Vec<AsyncOpSignal>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AsyncOpInspection {
    pub(crate) operation_id: String,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest_signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latest_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transcript_delta: Option<Vec<AsyncOperationTranscriptEvent>>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AsyncOpWaitResult {
    pub(crate) elapsed_seconds: u64,
    pub(crate) woken_by: &'static str,
    pub(crate) updates: Vec<AsyncOpSignalDigest>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AsyncOpWaitFilter {
    kind: Option<AsyncOpKind>,
    model_visible_only: bool,
}

impl AsyncOpWaitFilter {
    pub(crate) fn all() -> Self {
        Self::default()
    }

    pub(crate) fn kind(kind: Option<AsyncOpKind>) -> Self {
        Self {
            kind,
            model_visible_only: false,
        }
    }

    pub(crate) fn model_visible() -> Self {
        Self {
            kind: None,
            model_visible_only: true,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StartAsyncOp {
    pub(crate) id: AsyncOpId,
    pub(crate) kind: AsyncOpKind,
    pub(crate) label: String,
    pub(crate) parent_operation_id: Option<String>,
    pub(crate) parent_tool_name: Option<String>,
    pub(crate) started_summary: String,
    pub(crate) model_visible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AsyncOpDeliveryResult {
    pub(crate) operation_id: String,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AsyncOpStopped {
    pub(crate) operation_id: String,
    pub(crate) status: &'static str,
}

#[derive(Clone)]
pub(crate) struct AsyncOpManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    operations: Mutex<HashMap<AsyncOpId, Arc<AsyncOperation>>>,
    notify: Notify,
}

struct AsyncOperation {
    id: AsyncOpId,
    kind: AsyncOpKind,
    label: String,
    parent_operation_id: Option<String>,
    parent_tool_name: Option<String>,
    model_visible: bool,
    status: Mutex<AsyncOpStatus>,
    signals: Mutex<VecDeque<AsyncOpSignal>>,
    latest_output: Mutex<Option<Value>>,
    transcript: Mutex<TranscriptState>,
    inbox_tx: mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
    join: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Default)]
struct TranscriptState {
    events: VecDeque<AsyncOperationTranscriptEvent>,
    inspect_cursor: usize,
    total_seen: usize,
}

#[derive(Clone)]
pub(crate) struct AsyncOpHandle {
    manager: AsyncOpManager,
    operation: Arc<AsyncOperation>,
}

#[derive(Clone)]
pub(crate) struct AsyncOpChildHandle {
    manager: Weak<ManagerInner>,
    operation: Weak<AsyncOperation>,
    inbox_rx: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
}

pub(crate) struct StartedAsyncOp {
    pub(crate) handle: AsyncOpHandle,
    pub(crate) child: AsyncOpChildHandle,
}

#[derive(Debug, Clone)]
pub struct StartAsyncOperation {
    pub id: String,
    pub kind: AsyncOperationKind,
    pub label: String,
    pub parent_operation_id: Option<String>,
    pub parent_tool_name: Option<String>,
    pub started_summary: String,
    pub model_visible: bool,
}

#[derive(Clone)]
pub struct AsyncOperationRuntime {
    manager: AsyncOpManager,
}

#[derive(Clone)]
pub struct AsyncOperationHandle {
    operation_id: String,
    manager: AsyncOpManager,
    handle: AsyncOpHandle,
    events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
}

tokio::task_local! {
    static CURRENT_ASYNC_OPERATION_RUNTIME: Option<AsyncOperationRuntime>;
}

impl Default for AsyncOpManager {
    fn default() -> Self {
        Self::new()
    }
}

pub fn current_async_operation_runtime() -> Option<AsyncOperationRuntime> {
    CURRENT_ASYNC_OPERATION_RUNTIME
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

pub(crate) async fn scope_current_async_operation_runtime<F, T>(
    runtime: AsyncOperationRuntime,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    CURRENT_ASYNC_OPERATION_RUNTIME
        .scope(Some(runtime), future)
        .await
}

impl AsyncOperationRuntime {
    pub(crate) fn new(manager: AsyncOpManager) -> Self {
        Self { manager }
    }

    pub async fn start(&self, request: StartAsyncOperation) -> AsyncOperationHandle {
        let operation_id = request.id.clone();
        let events_tx = turn_loop::current_events_tx();
        let started = self
            .manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new(request.id),
                    kind: request.kind,
                    label: request.label,
                    parent_operation_id: request.parent_operation_id,
                    parent_tool_name: request.parent_tool_name,
                    started_summary: request.started_summary,
                    model_visible: request.model_visible,
                },
                events_tx.clone(),
            )
            .await;

        AsyncOperationHandle {
            operation_id,
            manager: self.manager.clone(),
            handle: started.handle,
            events_tx,
        }
    }
}

impl AsyncOperationHandle {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.handle.cancel_token()
    }

    pub async fn attach_join(&self, join: JoinHandle<()>) {
        self.manager
            .attach_join(&AsyncOpId::new(self.operation_id.clone()), join)
            .await;
    }

    pub async fn progress(&self, summary: impl Into<String>, details: Option<String>) {
        self.handle
            .progress(summary.into(), details, self.events_tx.clone())
            .await;
    }

    pub async fn complete(&self, summary: impl Into<String>, output: Option<Value>) {
        self.handle
            .complete(
                AsyncOpSignal::Completed {
                    summary: summary.into(),
                    output,
                },
                self.events_tx.clone(),
            )
            .await;
    }

    pub async fn fail(&self, error: impl Into<String>) {
        self.handle
            .complete(
                AsyncOpSignal::Failed {
                    error: error.into(),
                },
                self.events_tx.clone(),
            )
            .await;
    }
}

impl AsyncOpManager {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                operations: Mutex::new(HashMap::new()),
                notify: Notify::new(),
            }),
        }
    }

    pub(crate) async fn start(
        &self,
        request: StartAsyncOp,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) -> StartedAsyncOp {
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();
        let operation = Arc::new(AsyncOperation {
            id: request.id.clone(),
            kind: request.kind,
            label: request.label,
            parent_operation_id: request.parent_operation_id,
            parent_tool_name: request.parent_tool_name,
            model_visible: request.model_visible,
            status: Mutex::new(AsyncOpStatus::Running),
            signals: Mutex::new(VecDeque::new()),
            latest_output: Mutex::new(None),
            transcript: Mutex::new(TranscriptState::default()),
            inbox_tx,
            cancel: CancellationToken::new(),
            join: Mutex::new(None),
        });
        self.inner
            .operations
            .lock()
            .await
            .insert(request.id, operation.clone());

        let handle = AsyncOpHandle {
            manager: self.clone(),
            operation: operation.clone(),
        };
        handle
            .push_signal(
                AsyncOpSignal::Started {
                    summary: truncate(&request.started_summary, 500),
                },
                false,
                events_tx,
            )
            .await;

        StartedAsyncOp {
            handle,
            child: AsyncOpChildHandle {
                manager: Arc::downgrade(&self.inner),
                operation: Arc::downgrade(&operation),
                inbox_rx: Arc::new(Mutex::new(inbox_rx)),
            },
        }
    }

    pub(crate) async fn send_input(
        &self,
        operation_ids: Vec<String>,
        message: String,
    ) -> Vec<AsyncOpDeliveryResult> {
        let operations = self.select_operations(operation_ids, None).await;
        let mut results = Vec::with_capacity(operations.len());
        for operation in operations {
            if !matches!(
                operation.kind,
                AsyncOpKind::Ability | AsyncOpKind::Delegation
            ) {
                continue;
            }
            let status = *operation.status.lock().await;
            if !status.can_receive_input() {
                results.push(AsyncOpDeliveryResult {
                    operation_id: operation.id.to_string(),
                    status: "not_delivered",
                    reason: Some(format!("operation is {}", status.as_str())),
                });
                continue;
            }
            if operation.inbox_tx.send(message.clone()).is_ok() {
                if status == AsyncOpStatus::WaitingForInput {
                    *operation.status.lock().await = AsyncOpStatus::Running;
                }
                results.push(AsyncOpDeliveryResult {
                    operation_id: operation.id.to_string(),
                    status: "delivered",
                    reason: None,
                });
            } else {
                results.push(AsyncOpDeliveryResult {
                    operation_id: operation.id.to_string(),
                    status: "not_delivered",
                    reason: Some("operation inbox is closed".into()),
                });
            }
        }
        results
    }

    pub(crate) async fn stop(
        &self,
        operation_ids: Vec<String>,
        kind: Option<AsyncOpKind>,
        reason: Option<String>,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) -> Vec<AsyncOpStopped> {
        let operations = self.select_operations(operation_ids, kind).await;
        let mut stopped = Vec::with_capacity(operations.len());
        for operation in operations {
            operation.cancel.cancel();
            if let Some(join) = operation.join.lock().await.take() {
                join.abort();
            }
            let handle = AsyncOpHandle {
                manager: self.clone(),
                operation: operation.clone(),
            };
            handle
                .complete(
                    AsyncOpSignal::Stopped {
                        reason: reason.clone(),
                    },
                    events_tx.clone(),
                )
                .await;
            stopped.push(AsyncOpStopped {
                operation_id: operation.id.to_string(),
                status: AsyncOpStatus::Stopped.as_str(),
            });
        }
        stopped
    }

    pub(crate) async fn inspect(
        &self,
        operation_ids: Vec<String>,
        kind: Option<AsyncOpKind>,
        include_transcript: bool,
        limit: usize,
    ) -> Vec<AsyncOpInspection> {
        let limit = limit.clamp(1, INSPECT_LIMIT_CAP);
        let operations = self.select_operations(operation_ids, kind).await;
        let mut inspected = Vec::with_capacity(operations.len());
        for operation in operations {
            let status = *operation.status.lock().await;
            let latest_signal = operation
                .signals
                .lock()
                .await
                .back()
                .map(AsyncOpSignal::summary);
            let latest_output = operation.latest_output.lock().await.clone();
            let transcript_delta = if include_transcript {
                let mut transcript = operation.transcript.lock().await;
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
            inspected.push(AsyncOpInspection {
                operation_id: operation.id.to_string(),
                kind: operation.kind.as_str(),
                label: operation.label.clone(),
                status: status.as_str(),
                latest_signal,
                latest_output,
                transcript_delta,
            });
        }
        inspected
    }

    pub(crate) async fn wait(&self, seconds: u64, filter: AsyncOpWaitFilter) -> AsyncOpWaitResult {
        let seconds = seconds.clamp(1, 30);
        let started = Instant::now();
        let mut updates = self.drain_signals(filter.clone()).await;
        if updates.is_empty() && (!filter.model_visible_only || self.has_open_model_visible().await)
        {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(seconds)) => {}
                _ = self.inner.notify.notified() => {}
            }
            updates = self.drain_signals(filter).await;
        }
        let woken_by = classify_wake(&updates);
        AsyncOpWaitResult {
            elapsed_seconds: started.elapsed().as_secs(),
            woken_by,
            updates,
        }
    }

    pub(crate) async fn has_open_model_visible(&self) -> bool {
        let operations = self.inner.operations.lock().await;
        for operation in operations.values() {
            if !operation.model_visible {
                continue;
            }
            let status = *operation.status.lock().await;
            if !matches!(
                status,
                AsyncOpStatus::Completed | AsyncOpStatus::Failed | AsyncOpStatus::Stopped
            ) {
                return true;
            }
        }
        false
    }

    pub(crate) async fn notified(&self) {
        self.inner.notify.notified().await;
    }

    pub(crate) async fn drain_signals(
        &self,
        filter: AsyncOpWaitFilter,
    ) -> Vec<AsyncOpSignalDigest> {
        let operations = self.select_operations(Vec::new(), filter.kind).await;
        let mut updates = Vec::new();
        for operation in operations {
            if filter.model_visible_only && !operation.model_visible {
                continue;
            }
            let status = *operation.status.lock().await;
            let mut signals = operation.signals.lock().await;
            if signals.is_empty() {
                continue;
            }
            let mut events = Vec::new();
            while let Some(signal) = signals.pop_front() {
                if matches!(signal, AsyncOpSignal::Progress { .. })
                    && events.len() >= WAIT_EVENTS_PER_OPERATION
                {
                    continue;
                }
                events.push(signal);
            }
            if !events.is_empty() {
                updates.push(AsyncOpSignalDigest {
                    operation_id: operation.id.to_string(),
                    kind: operation.kind.as_str(),
                    label: operation.label.clone(),
                    status: status.as_str(),
                    events,
                });
            }
        }
        updates
    }

    pub(crate) async fn attach_join(&self, id: &AsyncOpId, join: JoinHandle<()>) {
        if let Some(operation) = self.inner.operations.lock().await.get(id).cloned() {
            *operation.join.lock().await = Some(join);
        }
    }

    pub(crate) async fn handle(&self, id: &AsyncOpId) -> Option<AsyncOpHandle> {
        self.inner
            .operations
            .lock()
            .await
            .get(id)
            .cloned()
            .map(|operation| AsyncOpHandle {
                manager: self.clone(),
                operation,
            })
    }

    async fn select_operations(
        &self,
        operation_ids: Vec<String>,
        kind: Option<AsyncOpKind>,
    ) -> Vec<Arc<AsyncOperation>> {
        let operations = self.inner.operations.lock().await;
        if operation_ids.is_empty() {
            return operations
                .values()
                .filter(|operation| kind.is_none_or(|k| operation.kind == k))
                .cloned()
                .collect();
        }
        operation_ids
            .into_iter()
            .filter_map(|id| operations.get(&AsyncOpId::new(id)).cloned())
            .filter(|operation| kind.is_none_or(|k| operation.kind == k))
            .collect()
    }
}

impl Drop for ManagerInner {
    fn drop(&mut self) {
        if let Ok(operations) = self.operations.try_lock() {
            for operation in operations.values() {
                operation.cancel.cancel();
                if let Ok(mut join) = operation.join.try_lock()
                    && let Some(handle) = join.take()
                {
                    handle.abort();
                }
            }
        }
    }
}

impl AsyncOpHandle {
    pub(crate) fn cancel_token(&self) -> CancellationToken {
        self.operation.cancel.clone()
    }

    pub(crate) async fn progress(
        &self,
        summary: String,
        details: Option<String>,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        self.push_signal(
            AsyncOpSignal::Progress {
                summary: truncate(&summary, 500),
                details: details.map(|details| truncate(&details, 1000)),
            },
            false,
            events_tx,
        )
        .await;
    }

    pub(crate) async fn complete(
        &self,
        signal: AsyncOpSignal,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        let mut status = self.operation.status.lock().await;
        if matches!(*status, AsyncOpStatus::Stopped) {
            return;
        }
        *status = signal.status();
        drop(status);
        if let AsyncOpSignal::Completed { output, .. } = &signal {
            *self.operation.latest_output.lock().await = output.clone();
        }
        self.push_signal(signal, true, events_tx).await;
    }

    async fn push_signal(
        &self,
        signal: AsyncOpSignal,
        wake: bool,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        let should_wake = wake || signal.wakes_parent();
        let status = signal.status();
        let event = TurnEvent::AsyncOperationEvent {
            operation_id: self.operation.id.to_string(),
            kind: self.operation.kind.as_str().to_string(),
            label: self.operation.label.clone(),
            parent_operation_id: self.operation.parent_operation_id.clone(),
            parent_tool_name: self.operation.parent_tool_name.clone(),
            status: status.as_str().to_string(),
            signal: signal.kind().to_string(),
            summary: Some(signal.summary()),
            payload: signal.payload(),
            model_visible: self.operation.model_visible,
        };
        let mut signals = self.operation.signals.lock().await;
        push_bounded(&mut signals, signal, SIGNAL_QUEUE_CAP);
        drop(signals);
        if let Some(tx) = events_tx {
            let _ = tx.send(event);
        }
        if should_wake {
            self.manager.inner.notify.notify_waiters();
        }
    }

    pub(crate) async fn transcript(
        &self,
        event: AsyncOperationTranscriptEvent,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        let mut transcript = self.operation.transcript.lock().await;
        push_bounded(&mut transcript.events, event.clone(), TRANSCRIPT_CAP);
        transcript.total_seen += 1;
        drop(transcript);
        if let Some(tx) = events_tx {
            let _ = tx.send(TurnEvent::AsyncOperationTranscript {
                operation_id: self.operation.id.to_string(),
                kind: self.operation.kind.as_str().to_string(),
                label: self.operation.label.clone(),
                event,
            });
        }
    }
}

impl AsyncOpChildHandle {
    pub(crate) fn cancel_token(&self) -> Option<CancellationToken> {
        Some(self.operation.upgrade()?.cancel.clone())
    }

    pub(crate) async fn progress(
        &self,
        summary: String,
        details: Option<String>,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        let Some(operation) = self.operation.upgrade() else {
            return;
        };
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let handle = AsyncOpHandle {
            manager: AsyncOpManager { inner: manager },
            operation,
        };
        handle.progress(summary, details, events_tx).await;
    }

    pub(crate) async fn ask(
        &self,
        question: String,
        context: Option<String>,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) -> Option<String> {
        let operation = self.operation.upgrade()?;
        *operation.status.lock().await = AsyncOpStatus::WaitingForInput;
        let manager = AsyncOpManager {
            inner: self.manager.upgrade()?,
        };
        let handle = AsyncOpHandle { manager, operation };
        handle
            .push_signal(
                AsyncOpSignal::NeedsInput {
                    question: truncate(&question, 500),
                    context: context.map(|value| truncate(&value, 1000)),
                },
                true,
                events_tx,
            )
            .await;
        let next = self.inbox_rx.lock().await.recv().await;
        if let Some(operation) = self.operation.upgrade() {
            *operation.status.lock().await = AsyncOpStatus::Running;
        }
        next
    }
}

fn classify_wake(updates: &[AsyncOpSignalDigest]) -> &'static str {
    let has = |kind| {
        updates
            .iter()
            .flat_map(|digest| digest.events.iter())
            .any(|signal| signal.kind() == kind)
    };
    if has("needs_input") {
        "needs_input"
    } else if has("completed") || has("failed") {
        "operation_result"
    } else if has("stopped") {
        "stopped"
    } else {
        "timeout"
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, item: T, cap: usize) {
    queue.push_back(item);
    while queue.len() > cap {
        queue.pop_front();
    }
}

pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn operation_lifecycle_and_wait() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_research_1"),
                    kind: AsyncOpKind::Ability,
                    label: "research".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "starting research".into(),
                    model_visible: true,
                },
                None,
            )
            .await;
        started
            .handle
            .progress("halfway".into(), Some("details".into()), None)
            .await;
        started
            .handle
            .complete(
                AsyncOpSignal::Completed {
                    summary: "done".into(),
                    output: None,
                },
                None,
            )
            .await;

        let result = manager
            .wait(1, AsyncOpWaitFilter::kind(Some(AsyncOpKind::Ability)))
            .await;
        assert_eq!(result.woken_by, "operation_result");
        assert_eq!(result.updates.len(), 1);
        assert_eq!(result.updates[0].operation_id, "ability_research_1");
        assert_eq!(result.updates[0].events.len(), 3);
    }

    #[tokio::test]
    async fn inspect_retains_completed_output_after_wait_drains_signals() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("media_generate_video_1"),
                    kind: AsyncOpKind::Media,
                    label: "xai generate_video".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("generate_video".into()),
                    started_summary: "starting video".into(),
                    model_visible: true,
                },
                None,
            )
            .await;
        let output = serde_json::json!({
            "type": "assets",
            "assets": [
                {
                    "type": "url",
                    "url": "https://vidgen.x.ai/example/video.mp4",
                    "mime_type": "video/mp4"
                }
            ]
        });
        started
            .handle
            .complete(
                AsyncOpSignal::Completed {
                    summary: "done".into(),
                    output: Some(output.clone()),
                },
                None,
            )
            .await;

        let result = manager
            .wait(1, AsyncOpWaitFilter::kind(Some(AsyncOpKind::Media)))
            .await;
        assert_eq!(result.woken_by, "operation_result");

        let inspected = manager
            .inspect(
                vec!["media_generate_video_1".into()],
                Some(AsyncOpKind::Media),
                false,
                10,
            )
            .await;
        assert_eq!(inspected.len(), 1);
        assert_eq!(inspected[0].status, "completed");
        assert_eq!(inspected[0].latest_output, Some(output));
    }

    #[tokio::test]
    async fn model_visible_wait_drains_only_model_visible_operations() {
        let manager = AsyncOpManager::new();
        let _visible = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_build_1"),
                    kind: AsyncOpKind::Ability,
                    label: "build".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "build library".into(),
                    model_visible: true,
                },
                None,
            )
            .await;
        let _hidden = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("shell_1"),
                    kind: AsyncOpKind::Shell,
                    label: "shell".into(),
                    parent_operation_id: None,
                    parent_tool_name: None,
                    started_summary: "hidden shell".into(),
                    model_visible: false,
                },
                None,
            )
            .await;

        let result = manager.wait(1, AsyncOpWaitFilter::model_visible()).await;

        assert_eq!(result.updates.len(), 1);
        assert_eq!(result.updates[0].operation_id, "ability_build_1");
        assert_eq!(result.updates[0].status, "running");
        assert!(manager.has_open_model_visible().await);
    }

    #[tokio::test]
    async fn model_visible_open_false_after_terminal_signal() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_build_1"),
                    kind: AsyncOpKind::Ability,
                    label: "build".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "build library".into(),
                    model_visible: true,
                },
                None,
            )
            .await;
        started
            .handle
            .complete(
                AsyncOpSignal::Completed {
                    summary: "done".into(),
                    output: None,
                },
                None,
            )
            .await;

        let result = manager.wait(1, AsyncOpWaitFilter::model_visible()).await;

        assert!(!manager.has_open_model_visible().await);
        assert_eq!(result.woken_by, "operation_result");
        assert_eq!(result.updates[0].status, "completed");
    }

    #[tokio::test]
    async fn bounded_signal_queue_drops_oldest() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("op"),
                    kind: AsyncOpKind::Shell,
                    label: "shell".into(),
                    parent_operation_id: None,
                    parent_tool_name: None,
                    started_summary: "started".into(),
                    model_visible: false,
                },
                None,
            )
            .await;
        for index in 0..(SIGNAL_QUEUE_CAP + 10) {
            started
                .handle
                .progress(format!("p{index}"), None, None)
                .await;
        }

        let updates = manager.drain_signals(AsyncOpWaitFilter::all()).await;
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].events.len(), WAIT_EVENTS_PER_OPERATION);
    }

    #[tokio::test]
    async fn needs_input_resumes_after_send() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_research_1"),
                    kind: AsyncOpKind::Ability,
                    label: "research".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "starting".into(),
                    model_visible: true,
                },
                None,
            )
            .await;
        let child = started.child.clone();
        let ask = tokio::spawn(async move { child.ask("continue?".into(), None, None).await });
        let _ = manager
            .wait(1, AsyncOpWaitFilter::kind(Some(AsyncOpKind::Ability)))
            .await;
        let sent = manager
            .send_input(vec!["ability_research_1".into()], "yes".into())
            .await;
        assert_eq!(sent[0].status, "delivered");
        assert_eq!(ask.await.unwrap().as_deref(), Some("yes"));
    }
}
