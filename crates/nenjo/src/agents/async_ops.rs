//! Shared runtime for long-running agent operations.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::{AbortHandle, JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::tools::{AsyncControl, AsyncControls, AsyncOperationKind, AsyncOperationSignalKind};
pub(crate) use crate::tools::{
    AsyncOperationKind as AsyncOpKind, AsyncOperationStatus as AsyncOpStatus,
};

use super::runner::turn_loop;
use super::runner::types::{AsyncOperationTranscriptEvent, TurnEvent};

const SIGNAL_QUEUE_CAP: usize = 128;
const TRANSCRIPT_CAP: usize = 256;
const WAIT_EVENTS_PER_OPERATION: usize = 12;
const INSPECT_LIMIT_CAP: usize = 50;
const OPERATION_INBOX_CAP: usize = 8;
const TERMINAL_OPERATION_CAP: usize = 128;

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
    RecoverableToolError {
        tool: String,
        error: String,
        recommended_action: AsyncOpRecommendedAction,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
    },
    Stopped {
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AsyncOpRecommendedAction {
    Wait,
}

impl AsyncOpSignal {
    pub(crate) fn kind(&self) -> &'static str {
        self.signal_kind().as_str()
    }

    pub(crate) fn signal_kind(&self) -> AsyncOperationSignalKind {
        match self {
            Self::Started { .. } => AsyncOperationSignalKind::Started,
            Self::Progress { .. } => AsyncOperationSignalKind::Progress,
            Self::RecoverableToolError { .. } => AsyncOperationSignalKind::RecoverableToolError,
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
            Self::Started { .. } | Self::Progress { .. } | Self::RecoverableToolError { .. } => {
                AsyncOpStatus::Running
            }
        }
    }

    pub(crate) fn wakes_parent(&self) -> bool {
        matches!(
            self,
            Self::RecoverableToolError { .. }
                | Self::NeedsInput { .. }
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
            Self::RecoverableToolError { tool, .. } => format!(
                "Tool '{tool}' failed, but the ability is still running and can self-correct"
            ),
            Self::NeedsInput { question, .. } => question.clone(),
            Self::Failed { error, .. } => error.clone(),
            Self::Stopped { reason } => reason.clone().unwrap_or_else(|| "stopped".into()),
        }
    }

    pub(crate) fn payload(&self) -> Option<Value> {
        match self {
            Self::Progress { details, .. } => details
                .as_ref()
                .map(|details| serde_json::json!({ "details": details })),
            Self::RecoverableToolError {
                tool,
                error,
                recommended_action,
            } => Some(serde_json::json!({
                "tool": tool,
                "error": error,
                "recoverable": true,
                "recommended_action": recommended_action,
            })),
            Self::NeedsInput { context, .. } => context
                .as_ref()
                .map(|context| serde_json::json!({ "context": context })),
            Self::Completed { output, .. } | Self::Failed { output, .. } => output.clone(),
            Self::Started { .. } | Self::Stopped { .. } => None,
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
    control: Option<AsyncControl>,
    model_visible_only: bool,
}

impl AsyncOpWaitFilter {
    #[cfg(test)]
    pub(crate) fn all() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn kind(kind: Option<AsyncOpKind>) -> Self {
        Self {
            kind,
            control: None,
            model_visible_only: false,
        }
    }

    pub(crate) fn control(control: AsyncControl, kind: Option<AsyncOpKind>) -> Self {
        Self {
            kind,
            control: Some(control),
            model_visible_only: false,
        }
    }

    pub(crate) fn model_visible() -> Self {
        Self {
            kind: None,
            control: None,
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
    pub(crate) controls: AsyncControls,
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
    change_tx: watch::Sender<u64>,
    next_sequence: AtomicU64,
    cancel: CancellationToken,
}

struct AsyncOperation {
    id: AsyncOpId,
    sequence: u64,
    kind: AsyncOpKind,
    label: String,
    parent_operation_id: Option<String>,
    parent_tool_name: Option<String>,
    model_visible: bool,
    controls: AsyncControls,
    lifecycle: Mutex<AsyncOpLifecycle>,
    signals: Mutex<VecDeque<AsyncOpSignal>>,
    latest_output: Mutex<Option<Value>>,
    transcript: Mutex<TranscriptState>,
    inbox_tx: mpsc::Sender<String>,
    cancel: CancellationToken,
}

enum AsyncOpLifecycle {
    Active {
        phase: AsyncOpActivePhase,
        abort: Option<AbortHandle>,
    },
    Terminal(AsyncOpStatus),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncOpActivePhase {
    Running,
    WaitingForInput,
}

enum TerminalTransition {
    Applied(Option<AbortHandle>),
    AlreadyTerminal,
}

impl AsyncOpLifecycle {
    fn status(&self) -> AsyncOpStatus {
        match self {
            Self::Active {
                phase: AsyncOpActivePhase::Running,
                ..
            } => AsyncOpStatus::Running,
            Self::Active {
                phase: AsyncOpActivePhase::WaitingForInput,
                ..
            } => AsyncOpStatus::WaitingForInput,
            Self::Terminal(status) => *status,
        }
    }

    fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }

    fn set_active_phase(&mut self, phase: AsyncOpActivePhase) -> bool {
        let Self::Active { phase: current, .. } = self else {
            return false;
        };
        *current = phase;
        true
    }

    fn attach_abort(&mut self, abort: AbortHandle) -> Result<(), AbortHandle> {
        let Self::Active { abort: current, .. } = self else {
            return Err(abort);
        };
        if current.is_some() {
            return Err(abort);
        }
        *current = Some(abort);
        Ok(())
    }

    fn transition_terminal(&mut self, status: AsyncOpStatus) -> TerminalTransition {
        debug_assert!(matches!(
            status,
            AsyncOpStatus::Completed | AsyncOpStatus::Failed | AsyncOpStatus::Stopped
        ));
        let Self::Active { abort, .. } = self else {
            return TerminalTransition::AlreadyTerminal;
        };
        let abort = abort.take();
        *self = Self::Terminal(status);
        TerminalTransition::Applied(abort)
    }
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
    inbox_rx: Arc<Mutex<mpsc::Receiver<String>>>,
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
    pub controls: AsyncControls,
}

#[derive(Clone)]
pub struct AsyncOperationRuntime {
    manager: AsyncOpManager,
}

#[derive(Clone)]
pub struct AsyncOperationHandle {
    operation_id: String,
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
                    controls: request.controls,
                },
                events_tx.clone(),
            )
            .await;

        AsyncOperationHandle {
            operation_id,
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
        self.handle.attach_join(join, self.events_tx.clone()).await;
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
        self.fail_with_output(error, None).await;
    }

    pub async fn fail_with_output(&self, error: impl Into<String>, output: Option<Value>) {
        self.handle
            .complete(
                AsyncOpSignal::Failed {
                    error: error.into(),
                    output,
                },
                self.events_tx.clone(),
            )
            .await;
    }

    pub async fn transcript(&self, event: AsyncOperationTranscriptEvent) {
        self.handle.transcript(event, self.events_tx.clone()).await;
    }
}

impl AsyncOpManager {
    pub(crate) fn new() -> Self {
        Self::with_cancel(CancellationToken::new())
    }

    pub(crate) fn with_cancel(cancel: CancellationToken) -> Self {
        let (change_tx, _change_rx) = watch::channel(0);
        Self {
            inner: Arc::new(ManagerInner {
                operations: Mutex::new(HashMap::new()),
                change_tx,
                next_sequence: AtomicU64::new(1),
                cancel,
            }),
        }
    }

    pub(crate) async fn start(
        &self,
        request: StartAsyncOp,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) -> StartedAsyncOp {
        let (inbox_tx, inbox_rx) = mpsc::channel(OPERATION_INBOX_CAP);
        let operation = Arc::new(AsyncOperation {
            id: request.id.clone(),
            sequence: self.inner.next_sequence.fetch_add(1, Ordering::Relaxed),
            kind: request.kind,
            label: request.label,
            parent_operation_id: request.parent_operation_id,
            parent_tool_name: request.parent_tool_name,
            model_visible: request.model_visible,
            controls: request.controls,
            lifecycle: Mutex::new(AsyncOpLifecycle::Active {
                phase: AsyncOpActivePhase::Running,
                abort: None,
            }),
            signals: Mutex::new(VecDeque::new()),
            latest_output: Mutex::new(None),
            transcript: Mutex::new(TranscriptState::default()),
            inbox_tx,
            cancel: self.inner.cancel.child_token(),
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
        let operations = self
            .select_operations(operation_ids, None, Some(AsyncControl::SendInput))
            .await;
        let mut results = Vec::with_capacity(operations.len());
        for operation in operations {
            let mut lifecycle = operation.lifecycle.lock().await;
            let status = lifecycle.status();
            if !lifecycle.is_active() {
                results.push(AsyncOpDeliveryResult {
                    operation_id: operation.id.to_string(),
                    status: "not_delivered",
                    reason: Some(format!("operation is {}", status.as_str())),
                });
                continue;
            }
            let result = match operation.inbox_tx.try_send(message.clone()) {
                Ok(()) => {
                    lifecycle.set_active_phase(AsyncOpActivePhase::Running);
                    AsyncOpDeliveryResult {
                        operation_id: operation.id.to_string(),
                        status: "delivered",
                        reason: None,
                    }
                }
                Err(TrySendError::Full(_)) => AsyncOpDeliveryResult {
                    operation_id: operation.id.to_string(),
                    status: "not_delivered",
                    reason: Some("operation inbox is full".into()),
                },
                Err(TrySendError::Closed(_)) => AsyncOpDeliveryResult {
                    operation_id: operation.id.to_string(),
                    status: "not_delivered",
                    reason: Some("operation inbox is closed".into()),
                },
            };
            results.push(result);
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
        let operations = self
            .select_operations(operation_ids, kind, Some(AsyncControl::Stop))
            .await;
        let mut stopped = Vec::with_capacity(operations.len());
        for operation in operations {
            let abort = {
                let mut lifecycle = operation.lifecycle.lock().await;
                match lifecycle.transition_terminal(AsyncOpStatus::Stopped) {
                    TerminalTransition::Applied(abort) => abort,
                    TerminalTransition::AlreadyTerminal => continue,
                }
            };
            operation.cancel.cancel();
            if let Some(abort) = abort {
                abort.abort();
            }
            let handle = AsyncOpHandle {
                manager: self.clone(),
                operation: operation.clone(),
            };
            handle
                .push_signal(
                    AsyncOpSignal::Stopped {
                        reason: reason.clone(),
                    },
                    true,
                    events_tx.clone(),
                )
                .await;
            self.prune_terminal_operations().await;
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
        let operations = self
            .select_operations(operation_ids, kind, Some(AsyncControl::Inspect))
            .await;
        let mut inspected = Vec::with_capacity(operations.len());
        for operation in operations {
            let status = operation.lifecycle.lock().await.status();
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
        let deadline = started + std::time::Duration::from_secs(seconds);
        let mut changes = self.inner.change_tx.subscribe();
        let updates = loop {
            let updates = self.drain_signals(filter.clone()).await;
            if !updates.is_empty()
                || (filter.model_visible_only && !self.has_open_model_visible().await)
            {
                break updates;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break updates;
            }
            let timed_out = tokio::select! {
                _ = tokio::time::sleep(remaining) => true,
                _ = changes.changed() => false,
            };
            if timed_out {
                break self.drain_signals(filter.clone()).await;
            }
        };
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
            let status = operation.lifecycle.lock().await.status();
            if !matches!(
                status,
                AsyncOpStatus::Completed | AsyncOpStatus::Failed | AsyncOpStatus::Stopped
            ) {
                return true;
            }
        }
        false
    }

    pub(crate) async fn has_model_visible_control(&self, control: AsyncControl) -> bool {
        self.inner
            .operations
            .lock()
            .await
            .values()
            .any(|operation| operation.model_visible && operation.controls.contains(control))
    }

    pub(crate) async fn drain_signals(
        &self,
        filter: AsyncOpWaitFilter,
    ) -> Vec<AsyncOpSignalDigest> {
        let operations = self
            .select_operations(Vec::new(), filter.kind, filter.control)
            .await;
        let mut updates = Vec::new();
        for operation in operations {
            if filter.model_visible_only && !operation.model_visible {
                continue;
            }
            let status = operation.lifecycle.lock().await.status();
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
        control: Option<AsyncControl>,
    ) -> Vec<Arc<AsyncOperation>> {
        let operations = self.inner.operations.lock().await;
        if operation_ids.is_empty() {
            return operations
                .values()
                .filter(|operation| kind.is_none_or(|k| operation.kind == k))
                .filter(|operation| control.is_none_or(|c| operation.controls.contains(c)))
                .cloned()
                .collect();
        }
        operation_ids
            .into_iter()
            .filter_map(|id| operations.get(&AsyncOpId::new(id)).cloned())
            .filter(|operation| kind.is_none_or(|k| operation.kind == k))
            .filter(|operation| control.is_none_or(|c| operation.controls.contains(c)))
            .collect()
    }

    async fn prune_terminal_operations(&self) {
        let operations = self
            .inner
            .operations
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut terminal = Vec::new();
        for operation in operations {
            if !operation.lifecycle.lock().await.is_active() {
                terminal.push((operation.sequence, operation.id.clone(), operation));
            }
        }
        if terminal.len() <= TERMINAL_OPERATION_CAP {
            return;
        }
        terminal.sort_unstable_by_key(|(sequence, _, _)| *sequence);
        let remove_count = terminal.len() - TERMINAL_OPERATION_CAP;
        let mut operations = self.inner.operations.lock().await;
        for (_, id, expected) in terminal.into_iter().take(remove_count) {
            if operations
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, &expected))
            {
                operations.remove(&id);
            }
        }
    }

    fn notify_change(&self) {
        self.inner
            .change_tx
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

impl Drop for ManagerInner {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(operations) = self.operations.try_lock() {
            for operation in operations.values() {
                operation.cancel.cancel();
                if let Ok(mut lifecycle) = operation.lifecycle.try_lock()
                    && let AsyncOpLifecycle::Active {
                        abort: Some(abort), ..
                    } = &mut *lifecycle
                {
                    abort.abort();
                }
            }
        }
    }
}

impl AsyncOpHandle {
    pub(crate) fn cancel_token(&self) -> CancellationToken {
        self.operation.cancel.clone()
    }

    pub(crate) async fn attach_join(
        &self,
        join: JoinHandle<()>,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        let abort = join.abort_handle();
        let attached = self
            .operation
            .lifecycle
            .lock()
            .await
            .attach_abort(abort.clone())
            .is_ok();
        if !attached {
            abort.abort();
        }

        let manager = Arc::downgrade(&self.manager.inner);
        let operation = Arc::downgrade(&self.operation);
        tokio::spawn(async move {
            let outcome = join.await;
            let Some(manager) = manager.upgrade() else {
                return;
            };
            let Some(operation) = operation.upgrade() else {
                return;
            };
            let error = operation_task_error(outcome);
            let handle = AsyncOpHandle {
                manager: AsyncOpManager { inner: manager },
                operation,
            };
            handle.fail_if_active(error, events_tx).await;
        });
    }

    pub(crate) async fn progress(
        &self,
        summary: String,
        details: Option<String>,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        self.emit_signal(
            AsyncOpSignal::Progress {
                summary: truncate(&summary, 500),
                details: details.map(|details| truncate(&details, 1000)),
            },
            events_tx,
        )
        .await;
    }

    pub(crate) async fn recoverable_tool_error(
        &self,
        tool: String,
        error: String,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        self.emit_signal(
            AsyncOpSignal::RecoverableToolError {
                tool: truncate(&tool, 200),
                error: truncate(&error, 1000),
                recommended_action: AsyncOpRecommendedAction::Wait,
            },
            events_tx,
        )
        .await;
    }

    pub(crate) async fn complete(
        &self,
        signal: AsyncOpSignal,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        self.emit_signal(signal, events_tx).await;
    }

    async fn emit_signal(
        &self,
        signal: AsyncOpSignal,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        match signal.status() {
            AsyncOpStatus::Running => {
                let lifecycle = self.operation.lifecycle.lock().await;
                if !lifecycle.is_active() {
                    return;
                }
                self.push_signal(signal, false, events_tx).await;
            }
            AsyncOpStatus::WaitingForInput => {
                let mut lifecycle = self.operation.lifecycle.lock().await;
                if !lifecycle.set_active_phase(AsyncOpActivePhase::WaitingForInput) {
                    return;
                }
                self.push_signal(signal, true, events_tx).await;
            }
            status
            @ (AsyncOpStatus::Completed | AsyncOpStatus::Failed | AsyncOpStatus::Stopped) => {
                let transition = self
                    .operation
                    .lifecycle
                    .lock()
                    .await
                    .transition_terminal(status);
                if matches!(transition, TerminalTransition::AlreadyTerminal) {
                    return;
                }
                match &signal {
                    AsyncOpSignal::Completed { output, .. }
                    | AsyncOpSignal::Failed { output, .. } => {
                        *self.operation.latest_output.lock().await = output.clone();
                    }
                    AsyncOpSignal::Stopped { .. } => {}
                    AsyncOpSignal::Started { .. }
                    | AsyncOpSignal::Progress { .. }
                    | AsyncOpSignal::RecoverableToolError { .. }
                    | AsyncOpSignal::NeedsInput { .. } => unreachable!(),
                }
                self.push_signal(signal, true, events_tx).await;
                self.manager.prune_terminal_operations().await;
            }
        }
    }

    async fn fail_if_active(
        &self,
        error: String,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        if self.operation.lifecycle.lock().await.is_active() {
            self.complete(
                AsyncOpSignal::Failed {
                    error: truncate(&error, 500),
                    output: None,
                },
                events_tx,
            )
            .await;
        }
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
            self.manager.notify_change();
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
        let manager = AsyncOpManager {
            inner: self.manager.upgrade()?,
        };
        let handle = AsyncOpHandle { manager, operation };
        {
            let mut lifecycle = handle.operation.lifecycle.lock().await;
            if !lifecycle.set_active_phase(AsyncOpActivePhase::WaitingForInput) {
                return None;
            }
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
        }
        let cancel = handle.cancel_token();
        let mut inbox = self.inbox_rx.lock().await;
        let next = tokio::select! {
            _ = cancel.cancelled() => None,
            next = inbox.recv() => next,
        };
        if cancel.is_cancelled() {
            return None;
        }
        if let Some(operation) = self.operation.upgrade() {
            operation
                .lifecycle
                .lock()
                .await
                .set_active_phase(AsyncOpActivePhase::Running);
        }
        next
    }

    pub(crate) async fn receive_input(&self) -> Option<String> {
        let operation = self.operation.upgrade()?;
        let cancel = operation.cancel.clone();
        let mut inbox = self.inbox_rx.lock().await;
        let next = tokio::select! {
            _ = cancel.cancelled() => None,
            next = inbox.recv() => next,
        };
        if cancel.is_cancelled() {
            return None;
        }
        operation
            .lifecycle
            .lock()
            .await
            .set_active_phase(AsyncOpActivePhase::Running);
        next
    }

    pub(crate) async fn transcript(
        &self,
        event: AsyncOperationTranscriptEvent,
        events_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    ) {
        let Some(operation) = self.operation.upgrade() else {
            return;
        };
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        AsyncOpHandle {
            manager: AsyncOpManager { inner: manager },
            operation,
        }
        .transcript(event, events_tx)
        .await;
    }
}

fn operation_task_error(outcome: Result<(), JoinError>) -> String {
    match outcome {
        Ok(()) => "Async operation task ended without reporting a terminal status".into(),
        Err(error) if error.is_panic() => format!("Async operation task panicked: {error}"),
        Err(error) if error.is_cancelled() => {
            "Async operation task was cancelled before reporting a terminal status".into()
        }
        Err(error) => format!("Async operation task failed: {error}"),
    }
}

fn classify_wake(updates: &[AsyncOpSignalDigest]) -> &'static str {
    let has = |kind| {
        updates
            .iter()
            .flat_map(|digest| digest.events.iter())
            .any(|signal| signal.signal_kind() == kind)
    };
    if has(AsyncOperationSignalKind::NeedsInput) {
        "needs_input"
    } else if has(AsyncOperationSignalKind::Completed) || has(AsyncOperationSignalKind::Failed) {
        "operation_result"
    } else if has(AsyncOperationSignalKind::Stopped) {
        "stopped"
    } else if has(AsyncOperationSignalKind::RecoverableToolError) {
        "recoverable_error"
    } else if has(AsyncOperationSignalKind::Progress) {
        "progress"
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
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

    use super::*;

    fn all_controls() -> AsyncControls {
        AsyncControls::new(AsyncControl::Inspect)
            .with(AsyncControl::SendInput)
            .with(AsyncControl::Stop)
            .with(AsyncControl::Wait)
    }

    fn shell_request(id: impl Into<String>) -> StartAsyncOp {
        StartAsyncOp {
            id: AsyncOpId::new(id),
            kind: AsyncOpKind::Shell,
            label: "shell".into(),
            parent_operation_id: None,
            parent_tool_name: Some("shell".into()),
            started_summary: "starting shell".into(),
            model_visible: true,
            controls: all_controls(),
        }
    }

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
                    controls: all_controls(),
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
    async fn recoverable_tool_error_wakes_wait_without_ending_operation() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("ability_build_routine_1"),
                    kind: AsyncOpKind::Ability,
                    label: "build_routine".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("use_ability".into()),
                    started_summary: "building routine".into(),
                    model_visible: true,
                    controls: all_controls(),
                },
                None,
            )
            .await;
        manager
            .drain_signals(AsyncOpWaitFilter::model_visible())
            .await;

        started
            .handle
            .recoverable_tool_error("configure_routine".into(), "validation failed".into(), None)
            .await;

        let result = manager
            .wait(1, AsyncOpWaitFilter::kind(Some(AsyncOpKind::Ability)))
            .await;
        assert_eq!(result.woken_by, "recoverable_error");
        assert_eq!(result.updates[0].status, "running");
        assert!(matches!(
            &result.updates[0].events[0],
            AsyncOpSignal::RecoverableToolError {
                tool,
                error,
                recommended_action: AsyncOpRecommendedAction::Wait,
            } if tool == "configure_routine" && error == "validation failed"
        ));
        assert!(manager.has_open_model_visible().await);
    }

    #[tokio::test]
    async fn task_execution_progress_stays_quiet_until_completion() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("task_execution_1"),
                    kind: AsyncOpKind::TaskExecution,
                    label: "release-check".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("watch_execution_run".into()),
                    started_summary: "starting routine".into(),
                    model_visible: true,
                    controls: all_controls(),
                },
                None,
            )
            .await;
        manager
            .drain_signals(AsyncOpWaitFilter::model_visible())
            .await;

        let waiting_manager = manager.clone();
        let mut wait = tokio::spawn(async move {
            waiting_manager
                .wait(
                    30,
                    AsyncOpWaitFilter::kind(Some(AsyncOpKind::TaskExecution)),
                )
                .await
        });
        tokio::task::yield_now().await;
        started
            .handle
            .progress("one step complete".into(), None, None)
            .await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut wait)
                .await
                .is_err()
        );
        started
            .handle
            .complete(
                AsyncOpSignal::Completed {
                    summary: "routine complete".into(),
                    output: None,
                },
                None,
            )
            .await;

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut wait)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.woken_by, "operation_result");
        assert_eq!(result.updates[0].kind, "task_execution");
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
                    controls: all_controls(),
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
    async fn inspect_retains_failed_operation_output() {
        let manager = AsyncOpManager::new();
        let started = manager
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new("shell_1"),
                    kind: AsyncOpKind::Shell,
                    label: "failing shell command".into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("shell".into()),
                    started_summary: "running shell command".into(),
                    model_visible: true,
                    controls: all_controls(),
                },
                None,
            )
            .await;
        let output = serde_json::json!({
            "success": false,
            "exit_code": 1,
            "stdout": "",
            "stderr": "command failed"
        });
        started
            .handle
            .complete(
                AsyncOpSignal::Failed {
                    error: "Shell command exited with exit status: 1".into(),
                    output: Some(output.clone()),
                },
                None,
            )
            .await;

        let inspected = manager
            .inspect(vec!["shell_1".into()], Some(AsyncOpKind::Shell), false, 10)
            .await;

        assert_eq!(inspected[0].status, "failed");
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
                    controls: all_controls(),
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
                    controls: all_controls(),
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
                    controls: all_controls(),
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
                    controls: all_controls(),
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
                    controls: all_controls(),
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

    #[tokio::test]
    async fn operation_token_is_cancelled_by_manager_parent_token() {
        let parent = CancellationToken::new();
        let manager = AsyncOpManager::with_cancel(parent.clone());
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
                    controls: all_controls(),
                },
                None,
            )
            .await;

        assert!(!started.handle.cancel_token().is_cancelled());
        parent.cancel();
        started.handle.cancel_token().cancelled().await;
        assert!(started.handle.cancel_token().is_cancelled());
    }

    #[tokio::test]
    async fn child_ask_unblocks_when_operation_is_cancelled() {
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
                    controls: all_controls(),
                },
                None,
            )
            .await;
        let child = started.child.clone();
        let ask = tokio::spawn(async move { child.ask("continue?".into(), None, None).await });
        let _ = manager
            .wait(1, AsyncOpWaitFilter::kind(Some(AsyncOpKind::Ability)))
            .await;

        started.handle.cancel_token().cancel();

        assert_eq!(ask.await.unwrap(), None);
    }

    #[tokio::test]
    async fn first_terminal_signal_wins() {
        let manager = AsyncOpManager::new();
        let started = manager.start(shell_request("shell-1"), None).await;
        manager.drain_signals(AsyncOpWaitFilter::all()).await;

        started
            .handle
            .complete(
                AsyncOpSignal::Completed {
                    summary: "completed first".into(),
                    output: Some(serde_json::json!({"winner": "completed"})),
                },
                None,
            )
            .await;
        started
            .handle
            .complete(
                AsyncOpSignal::Failed {
                    error: "late failure".into(),
                    output: Some(serde_json::json!({"winner": "failed"})),
                },
                None,
            )
            .await;

        let inspection = manager
            .inspect(vec!["shell-1".into()], None, false, 10)
            .await;
        assert_eq!(inspection[0].status, "completed");
        assert_eq!(
            inspection[0].latest_output,
            Some(serde_json::json!({"winner": "completed"}))
        );
        let updates = manager.drain_signals(AsyncOpWaitFilter::all()).await;
        assert_eq!(updates[0].events.len(), 1);
        assert!(matches!(
            updates[0].events[0],
            AsyncOpSignal::Completed { .. }
        ));
    }

    #[tokio::test]
    async fn input_is_not_delivered_after_completion() {
        let manager = AsyncOpManager::new();
        let started = manager.start(shell_request("shell-1"), None).await;
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

        let sent = manager
            .send_input(vec!["shell-1".into()], "too late".into())
            .await;

        assert_eq!(sent[0].status, "not_delivered");
        assert_eq!(sent[0].reason.as_deref(), Some("operation is completed"));
    }

    #[tokio::test]
    async fn join_attached_after_stop_is_aborted() {
        struct Dropped(Arc<AtomicBool>);

        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, AtomicOrdering::SeqCst);
            }
        }

        let manager = AsyncOpManager::new();
        let started = manager.start(shell_request("shell-1"), None).await;
        let dropped = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let join = tokio::spawn({
            let dropped = dropped.clone();
            async move {
                let _guard = Dropped(dropped);
                let _ = ready_tx.send(());
                std::future::pending::<()>().await;
            }
        });
        ready_rx.await.expect("operation task should start");

        manager.stop(vec!["shell-1".into()], None, None, None).await;
        started.handle.attach_join(join, None).await;

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !dropped.load(AtomicOrdering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("late-attached task should be aborted");
    }

    #[tokio::test]
    async fn panicked_operation_task_becomes_failed() {
        let manager = AsyncOpManager::new();
        let started = manager.start(shell_request("shell-1"), None).await;
        manager.drain_signals(AsyncOpWaitFilter::all()).await;
        started
            .handle
            .attach_join(tokio::spawn(async { panic!("operation panic") }), None)
            .await;

        let result = manager.wait(1, AsyncOpWaitFilter::all()).await;

        assert_eq!(result.woken_by, "operation_result");
        assert_eq!(result.updates[0].status, "failed");
        assert!(matches!(
            &result.updates[0].events[0],
            AsyncOpSignal::Failed { error, .. } if error.contains("panicked")
        ));
    }

    #[tokio::test]
    async fn terminal_operation_retention_is_bounded() {
        let manager = AsyncOpManager::new();
        for index in 0..(TERMINAL_OPERATION_CAP + 5) {
            let started = manager
                .start(shell_request(format!("shell-{index}")), None)
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
        }

        assert_eq!(
            manager.inner.operations.lock().await.len(),
            TERMINAL_OPERATION_CAP
        );
        assert!(
            manager
                .inspect(vec!["shell-0".into()], None, false, 10)
                .await
                .is_empty()
        );
        assert_eq!(
            manager
                .inspect(
                    vec![format!("shell-{}", TERMINAL_OPERATION_CAP + 4)],
                    None,
                    false,
                    10,
                )
                .await
                .len(),
            1
        );
    }
}
