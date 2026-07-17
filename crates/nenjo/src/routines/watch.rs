//! Runtime observation for routine executions.

use std::collections::HashSet;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

use crate::agents::{StartAsyncOperation, current_async_operation_runtime};
use crate::tools::AsyncOperationKind;
use crate::{RoutineEvent, Slug, Tool, ToolCategory, ToolOrigin, ToolResult};

static WATCH_OPERATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Current lifecycle state for a runtime execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeExecutionStatus {
    Running,
    Completed,
    Failed,
}

impl RuntimeExecutionStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Latest routine event included with a runtime progress snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeExecutionEvent {
    Started,
    StepStarted {
        step_slug: Slug,
        step_name: String,
        step_run_id: Uuid,
    },
    StepCompleted {
        step_slug: Slug,
        step_run_id: Uuid,
        duration_ms: u64,
    },
    StepFailed {
        step_slug: Slug,
        step_name: String,
        step_run_id: Uuid,
        error: String,
        duration_ms: u64,
    },
    Finished {
        passed: bool,
    },
}

/// Progress information sufficient for an agent to estimate work remaining.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeExecutionProgress {
    pub execution_run_id: Uuid,
    pub routine: Slug,
    pub status: RuntimeExecutionStatus,
    pub total_steps: usize,
    pub completed_steps: usize,
    pub active_steps: usize,
    pub remaining_steps: usize,
    pub failed_attempts: usize,
    pub event: RuntimeExecutionEvent,
}

impl RuntimeExecutionProgress {
    fn summary(&self) -> String {
        match &self.event {
            RuntimeExecutionEvent::Started => format!(
                "Routine {} started with {} steps remaining",
                self.routine, self.remaining_steps
            ),
            RuntimeExecutionEvent::StepStarted { step_name, .. } => format!(
                "Started {step_name}; {} of {} steps complete, {} remaining",
                self.completed_steps, self.total_steps, self.remaining_steps
            ),
            RuntimeExecutionEvent::StepCompleted { step_slug, .. } => format!(
                "Completed {step_slug}; {} of {} steps complete, {} remaining",
                self.completed_steps, self.total_steps, self.remaining_steps
            ),
            RuntimeExecutionEvent::StepFailed { step_name, .. } => format!(
                "Step {step_name} failed; {} steps remain",
                self.remaining_steps
            ),
            RuntimeExecutionEvent::Finished { passed: true } => {
                format!("Routine {} completed", self.routine)
            }
            RuntimeExecutionEvent::Finished { passed: false } => {
                format!("Routine {} failed", self.routine)
            }
        }
    }
}

/// A point-in-time runtime snapshot followed by latest-state notifications.
pub struct RuntimeExecutionSubscription {
    pub initial: RuntimeExecutionProgress,
    pub updates: watch::Receiver<RuntimeExecutionProgress>,
}

/// Runtime-agnostic source for active execution progress.
#[async_trait]
pub trait RuntimeExecutionWatcher: Send + Sync {
    async fn subscribe(&self, execution_run_id: Uuid) -> Result<RuntimeExecutionSubscription>;
}

/// Agent tool that watches executions through one concrete runtime watcher.
pub struct WatchExecutionRunTool<W> {
    watcher: W,
}

impl<W> WatchExecutionRunTool<W> {
    pub fn new(watcher: W) -> Self {
        Self { watcher }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchExecutionRunArgs {
    execution_run_id: Uuid,
}

#[async_trait]
impl<W> Tool for WatchExecutionRunTool<W>
where
    W: RuntimeExecutionWatcher + 'static,
{
    fn name(&self) -> &str {
        "watch_execution_run"
    }

    fn description(&self) -> &str {
        "Start a model-visible async watch for an active routine execution owned by this worker. Routine step progress is recorded without waking the agent; completion wakes the agent."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "execution_run_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "An active local routine execution run ID returned by dispatch_task or retry_execution_run."
                }
            },
            "required": ["execution_run_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let args: WatchExecutionRunArgs =
            serde_json::from_value(args).context("invalid watch_execution_run arguments")?;
        let output =
            start_runtime_execution_watch(Some(&self.watcher), args.execution_run_id).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Host
    }
}

/// Concrete watcher for routine executions running inside this process.
#[derive(Clone, Default)]
pub struct LocalRoutineExecutionWatcher {
    entries: Arc<DashMap<Uuid, Arc<LocalRoutineExecutionEntry>>>,
}

struct LocalRoutineExecutionEntry {
    state: Mutex<LocalRoutineExecutionState>,
    updates: watch::Sender<RuntimeExecutionProgress>,
}

struct LocalRoutineExecutionState {
    execution_run_id: Uuid,
    routine: Slug,
    total_steps: usize,
    completed: HashSet<Slug>,
    active: HashSet<Uuid>,
    failed_attempts: usize,
    status: RuntimeExecutionStatus,
}

/// Publisher retained for the lifetime of one local routine execution.
pub struct LocalRoutineExecutionRegistration {
    execution_run_id: Uuid,
    entry: Arc<LocalRoutineExecutionEntry>,
    registry: Weak<DashMap<Uuid, Arc<LocalRoutineExecutionEntry>>>,
}

impl LocalRoutineExecutionWatcher {
    pub fn start(
        &self,
        execution_run_id: Uuid,
        routine: Slug,
        total_steps: usize,
    ) -> LocalRoutineExecutionRegistration {
        let state = LocalRoutineExecutionState {
            execution_run_id,
            routine,
            total_steps,
            completed: HashSet::new(),
            active: HashSet::new(),
            failed_attempts: 0,
            status: RuntimeExecutionStatus::Running,
        };
        let initial = state.snapshot(RuntimeExecutionEvent::Started);
        let (updates, _) = watch::channel(initial);
        let entry = Arc::new(LocalRoutineExecutionEntry {
            state: Mutex::new(state),
            updates,
        });
        self.entries.insert(execution_run_id, entry.clone());
        LocalRoutineExecutionRegistration {
            execution_run_id,
            entry,
            registry: Arc::downgrade(&self.entries),
        }
    }
}

impl LocalRoutineExecutionRegistration {
    pub fn publish(&self, event: &RoutineEvent) {
        let mut state = self.entry.state.lock();
        let event = match event {
            RoutineEvent::StepStarted {
                step_slug,
                step_run_id,
                step_name,
                ..
            } => {
                state.active.insert(*step_run_id);
                RuntimeExecutionEvent::StepStarted {
                    step_slug: step_slug.clone(),
                    step_name: step_name.clone(),
                    step_run_id: *step_run_id,
                }
            }
            RoutineEvent::StepCompleted {
                step_slug,
                step_run_id,
                duration_ms,
                ..
            } => {
                state.active.remove(step_run_id);
                state.completed.insert(step_slug.clone());
                RuntimeExecutionEvent::StepCompleted {
                    step_slug: step_slug.clone(),
                    step_run_id: *step_run_id,
                    duration_ms: *duration_ms,
                }
            }
            RoutineEvent::StepFailed {
                step_slug,
                step_run_id,
                step_name,
                error,
                duration_ms,
                ..
            } => {
                state.active.remove(step_run_id);
                state.failed_attempts = state.failed_attempts.saturating_add(1);
                RuntimeExecutionEvent::StepFailed {
                    step_slug: step_slug.clone(),
                    step_name: step_name.clone(),
                    step_run_id: *step_run_id,
                    error: error.clone(),
                    duration_ms: *duration_ms,
                }
            }
            RoutineEvent::Done { result, .. } => {
                state.active.clear();
                state.status = if result.passed {
                    RuntimeExecutionStatus::Completed
                } else {
                    RuntimeExecutionStatus::Failed
                };
                RuntimeExecutionEvent::Finished {
                    passed: result.passed,
                }
            }
            RoutineEvent::AgentEvent { .. } => return,
        };
        let progress = state.snapshot(event);
        drop(state);
        self.entry.updates.send_replace(progress);
    }
}

impl LocalRoutineExecutionState {
    fn snapshot(&self, event: RuntimeExecutionEvent) -> RuntimeExecutionProgress {
        let completed_steps = self.completed.len().min(self.total_steps);
        let remaining_steps = if self.status == RuntimeExecutionStatus::Running {
            self.total_steps.saturating_sub(completed_steps)
        } else {
            0
        };
        RuntimeExecutionProgress {
            execution_run_id: self.execution_run_id,
            routine: self.routine.clone(),
            status: self.status,
            total_steps: self.total_steps,
            completed_steps,
            active_steps: self.active.len(),
            remaining_steps,
            failed_attempts: self.failed_attempts,
            event,
        }
    }
}

impl Drop for LocalRoutineExecutionRegistration {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        if let Some(entry) = registry.get(&self.execution_run_id)
            && Arc::ptr_eq(entry.value(), &self.entry)
        {
            drop(entry);
            registry.remove(&self.execution_run_id);
        }
    }
}

#[async_trait]
impl RuntimeExecutionWatcher for LocalRoutineExecutionWatcher {
    async fn subscribe(&self, execution_run_id: Uuid) -> Result<RuntimeExecutionSubscription> {
        let entry = self.entries.get(&execution_run_id).ok_or_else(|| {
            anyhow!(
                "This worker does not own an active local routine execution for run {execution_run_id}. The run may be queued, assigned to another worker, already finished, or target a direct agent; watch_execution_run only follows active routines owned by this worker"
            )
        })?;
        let updates = entry.updates.subscribe();
        let initial = updates.borrow().clone();
        Ok(RuntimeExecutionSubscription { initial, updates })
    }
}

/// Register one runtime execution stream as a model-visible async operation.
pub async fn start_runtime_execution_watch<W: RuntimeExecutionWatcher>(
    watcher: Option<&W>,
    execution_run_id: Uuid,
) -> Result<Value> {
    let watcher = watcher.context(
        "watch_execution_run is unavailable because this worker has no runtime execution watcher",
    )?;
    let subscription = watcher.subscribe(execution_run_id).await?;
    let runtime = current_async_operation_runtime().context(
        "watch_execution_run must run inside an agent turn with async operations enabled",
    )?;
    let initial = subscription.initial;
    if initial.status.is_terminal() {
        bail!(
            "routine execution {execution_run_id} is already terminal; runtime watches only follow active runs"
        );
    }

    let sequence = WATCH_OPERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let operation_id = format!("task_execution_{execution_run_id}_{sequence}");
    let handle = runtime
        .start(StartAsyncOperation {
            id: operation_id.clone(),
            kind: AsyncOperationKind::TaskExecution,
            label: format!("{} ({execution_run_id})", initial.routine),
            parent_operation_id: None,
            parent_tool_name: Some("watch_execution_run".to_string()),
            started_summary: initial.summary(),
            model_visible: true,
        })
        .await;

    let mut updates = subscription.updates;
    let bridge = handle.clone();
    let join = tokio::spawn(async move {
        let cancellation = bridge.cancel_token();
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => break,
                changed = updates.changed() => {
                    if changed.is_err() {
                        bridge.fail(format!(
                            "Local routine execution {execution_run_id} ended before a terminal event; it may have been cancelled or the worker stopped"
                        )).await;
                        break;
                    }
                    let progress = updates.borrow_and_update().clone();
                    let output = serde_json::to_value(&progress).ok();
                    match progress.status {
                        RuntimeExecutionStatus::Running => {
                            bridge
                                .progress(
                                    progress.summary(),
                                    serde_json::to_string(&progress).ok(),
                                )
                                .await;
                        }
                        RuntimeExecutionStatus::Completed => {
                            bridge.complete(progress.summary(), output).await;
                            break;
                        }
                        RuntimeExecutionStatus::Failed => {
                            bridge.fail(progress.summary()).await;
                            break;
                        }
                    }
                }
            }
        }
    });
    handle.attach_join(join).await;

    Ok(json!({
        "type": "operation_started",
        "operation_id": operation_id,
        "kind": AsyncOperationKind::TaskExecution.as_str(),
        "execution_run_id": execution_run_id,
        "progress": initial,
        "next_step": {
            "tool": "wait_operations",
            "kind": AsyncOperationKind::TaskExecution.as_str(),
            "reason": "Wait for routine step progress"
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines::StepResult;

    #[tokio::test]
    async fn local_watcher_reports_step_counts_and_a_clear_missing_run_error() {
        let watcher = LocalRoutineExecutionWatcher::default();
        let run_id = Uuid::new_v4();
        let error = watcher.subscribe(run_id).await.err().unwrap();
        assert!(
            error
                .to_string()
                .contains("does not own an active local routine")
        );

        let registration = watcher.start(run_id, Slug::parse("release-check").unwrap(), 2);
        let mut subscription = watcher.subscribe(run_id).await.unwrap();
        registration.publish(&RoutineEvent::StepCompleted {
            step_slug: Slug::parse("test").unwrap(),
            step_run_id: Uuid::new_v4(),
            result: StepResult::default(),
            duration_ms: 12,
        });
        subscription.updates.changed().await.unwrap();
        let progress = subscription.updates.borrow_and_update().clone();
        assert_eq!(progress.completed_steps, 1);
        assert_eq!(progress.remaining_steps, 1);
    }
}
