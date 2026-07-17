use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::store::{CancellationOutcome, EnqueueOutcome, OccurrenceOutcome, TaskRuntimeStore};
use super::types::{
    TaskExecutionState, TaskExecutorOutcome, TaskInboxItem, TaskRuntimeEvent, TaskSubmission,
    TaskTrigger,
};

/// Durable inbox, shared concurrency gate, and local schedule coordinator.
pub struct TaskRuntime<S, Execute> {
    inner: Arc<TaskRuntimeInner<S, Execute>>,
}

struct TaskRuntimeInner<S, Execute> {
    store: Arc<S>,
    execute: Execute,
    semaphore: Arc<Semaphore>,
    dispatch_tx: mpsc::UnboundedSender<TaskInboxItem>,
    dispatch_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<TaskInboxItem>>>,
    events_tx: mpsc::UnboundedSender<TaskRuntimeEvent>,
    events_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<TaskRuntimeEvent>>>,
    shutdown: CancellationToken,
    running: Mutex<HashMap<uuid::Uuid, CancellationToken>>,
    started: AtomicBool,
}

impl<S, Execute> Clone for TaskRuntime<S, Execute> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<S, Execute, ExecuteFuture> TaskRuntime<S, Execute>
where
    S: TaskRuntimeStore + 'static,
    Execute: Fn(TaskSubmission, CancellationToken) -> ExecuteFuture + Send + Sync + 'static,
    ExecuteFuture: Future<Output = Result<TaskExecutorOutcome>> + Send + 'static,
{
    /// Construct an always-active task runtime with a bounded execution limit.
    pub fn new(store: Arc<S>, execute: Execute, max_concurrency: usize) -> Self {
        let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(TaskRuntimeInner {
                store,
                execute,
                semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
                dispatch_tx,
                dispatch_rx: tokio::sync::Mutex::new(Some(dispatch_rx)),
                events_tx,
                events_rx: tokio::sync::Mutex::new(Some(events_rx)),
                shutdown: CancellationToken::new(),
                running: Mutex::new(HashMap::new()),
                started: AtomicBool::new(false),
            }),
        }
    }

    /// Take the lossless lifecycle stream. A runtime has one host event bridge.
    pub async fn events(&self) -> Result<mpsc::UnboundedReceiver<TaskRuntimeEvent>> {
        self.inner
            .events_rx
            .lock()
            .await
            .take()
            .context("task runtime lifecycle receiver was already taken")
    }

    /// Recover interrupted work and start inbox and schedule coordination.
    pub async fn start(&self) -> Result<()> {
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut receiver = self
            .inner
            .dispatch_rx
            .lock()
            .await
            .take()
            .context("task runtime dispatch receiver is unavailable")?;
        for item in self.inner.store.recoverable().await? {
            self.emit(item.clone());
            self.inner
                .dispatch_tx
                .send(item)
                .map_err(|_| anyhow!("task runtime stopped during recovery"))?;
        }

        let runtime = self.clone();
        tokio::spawn(async move {
            let mut executions = JoinSet::new();
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    Some(item) = receiver.recv() => {
                        let runtime = runtime.clone();
                        executions.spawn(async move { runtime.execute_item(item).await });
                    }
                    _ = tick.tick() => {
                        if let Err(error) = runtime.evaluate_schedules(Utc::now()).await {
                            warn!(%error, "Failed to evaluate task schedules");
                        }
                    }
                    joined = executions.join_next(), if !executions.is_empty() => {
                        match joined {
                            Some(Ok(Err(error))) => {
                                warn!(%error, "Task inbox execution failed");
                            }
                            Some(Err(error)) => {
                                warn!(%error, "Task inbox execution panicked");
                            }
                            Some(Ok(Ok(()))) | None => {}
                        }
                    }
                    _ = runtime.inner.shutdown.cancelled() => break,
                }
            }
            executions.abort_all();
            while executions.join_next().await.is_some() {}
        });
        Ok(())
    }

    /// Persist a manual or retry submission before returning to its transport.
    pub async fn submit(&self, submission: TaskSubmission) -> Result<EnqueueOutcome> {
        let outcome = self
            .inner
            .store
            .enqueue(TaskInboxItem::queued(submission, Utc::now()))
            .await?;
        if let EnqueueOutcome::Inserted(item) = &outcome {
            self.emit(item.as_ref().clone());
            self.inner
                .dispatch_tx
                .send(item.as_ref().clone())
                .map_err(|_| anyhow!("task runtime is stopped"))?;
        } else if let EnqueueOutcome::Cancelled(item) = &outcome {
            self.emit(item.as_ref().clone());
        }
        Ok(outcome)
    }

    /// Cancel a queued receipt immediately or signal its running host execution.
    pub async fn cancel(&self, execution_run_id: uuid::Uuid) -> Result<()> {
        match self.inner.store.cancel(execution_run_id).await? {
            CancellationOutcome::Queued(item) => self.emit(*item),
            CancellationOutcome::Running(item) => {
                self.emit(*item);
                if let Some(cancellation) =
                    self.running_executions()?.get(&execution_run_id).cloned()
                {
                    cancellation.cancel();
                }
            }
            CancellationOutcome::Recorded | CancellationOutcome::Inactive => {}
        }
        Ok(())
    }

    /// Replace hydrated definitions while retaining locally advanced unchanged schedules.
    pub async fn replace_schedules(
        &self,
        schedules: Vec<super::types::TaskSchedule>,
    ) -> Result<()> {
        self.inner.store.replace_schedules(schedules).await
    }

    /// Stop local scheduling and queued execution dispatch.
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
    }

    fn emit(&self, item: TaskInboxItem) {
        let _ = self.inner.events_tx.send(TaskRuntimeEvent { item });
    }

    async fn execute_item(&self, item: TaskInboxItem) -> Result<()> {
        let _permit = self
            .inner
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .context("task execution semaphore closed")?;
        let execution_run_id = item.submission.execution_run_id;
        let cancellation = CancellationToken::new();
        self.running_executions()?
            .insert(execution_run_id, cancellation.clone());
        let running = match self
            .inner
            .store
            .transition(execution_run_id, TaskExecutionState::Running)
            .await
        {
            Ok(Some(item)) => item,
            Ok(None) => {
                self.remove_running(execution_run_id)?;
                return Ok(());
            }
            Err(error) => {
                self.remove_running(execution_run_id)?;
                return Err(error).context("start queued task");
            }
        };
        self.emit(running.clone());
        let result = (self.inner.execute)(running.submission.clone(), cancellation).await;
        self.remove_running(execution_run_id)?;
        let state = match result {
            Ok(TaskExecutorOutcome::Completed) => TaskExecutionState::Completed,
            Ok(TaskExecutorOutcome::Failed(error)) => TaskExecutionState::Failed { error },
            Ok(TaskExecutorOutcome::Cancelled) => TaskExecutionState::Cancelled,
            Err(error) => TaskExecutionState::Failed {
                error: error_chain(&error),
            },
        };
        if let Some(item) = self.inner.store.transition(execution_run_id, state).await? {
            self.emit(item)
        }
        Ok(())
    }

    fn running_executions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<uuid::Uuid, CancellationToken>>> {
        self.inner
            .running
            .lock()
            .map_err(|_| anyhow!("task cancellation registry is poisoned"))
    }

    fn remove_running(&self, execution_run_id: uuid::Uuid) -> Result<()> {
        self.running_executions()?.remove(&execution_run_id);
        Ok(())
    }

    async fn evaluate_schedules(&self, now: DateTime<Utc>) -> Result<()> {
        for schedule in self.inner.store.schedules().await? {
            if !schedule.enabled || schedule.next_run_at > now {
                continue;
            }
            let scheduled_for = schedule.next_run_at;
            let next_run_at = schedule
                .definition
                .next_after(now, schedule.occurrence_count.saturating_add(1))
                .context("evaluate task schedule definition")?;
            let execution_run_id =
                uuid::Uuid::new_v5(&schedule.id, scheduled_for.to_rfc3339().as_bytes());
            let submission = TaskSubmission {
                requested_by: schedule.authorized_by,
                task_id: schedule.task_id,
                execution_run_id,
                project: schedule.project.clone(),
                target: schedule.target.clone(),
                content: schedule.content.clone(),
                trigger: TaskTrigger::Schedule {
                    schedule_id: schedule.id,
                    scheduled_for,
                    next_run_at,
                    assignment_revision: schedule.revision.clone(),
                },
            };
            let item = TaskInboxItem::queued(submission, now);
            let rejection = (!schedule.runnable).then(|| "task is not runnable".to_string());
            match self
                .inner
                .store
                .materialize_occurrence(schedule.id, scheduled_for, next_run_at, item, rejection)
                .await?
            {
                OccurrenceOutcome::Enqueued(item) => {
                    self.emit(item.as_ref().clone());
                    self.inner
                        .dispatch_tx
                        .send(*item)
                        .map_err(|_| anyhow!("task runtime is stopped"))?;
                }
                OccurrenceOutcome::Rejected(item) => self.emit(*item),
                OccurrenceOutcome::Duplicate | OccurrenceOutcome::Stale => {}
            }
        }
        Ok(())
    }
}

fn error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}
