//! Atomic file-backed persistence for the harness task runtime.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::task_runtime::{
    CancellationOutcome, EnqueueOutcome, OccurrenceOutcome, TaskExecutionState, TaskInboxItem,
    TaskRuntimeStore, TaskSchedule,
};

#[derive(Debug, Default, Serialize, Deserialize)]
struct TaskRuntimeSnapshot {
    #[serde(default)]
    inbox: Vec<TaskInboxItem>,
    /// Run IDs cancelled before their queue delivery reached this worker.
    #[serde(default)]
    cancellations: Vec<Uuid>,
    #[serde(default)]
    schedules: Vec<TaskSchedule>,
}

fn truncate(value: &mut String, max_chars: usize) {
    if value.chars().count() > max_chars {
        *value = value.chars().take(max_chars).collect();
    }
}

/// File-backed task inbox and locally advanced schedule state.
pub struct FileTaskRuntimeStore {
    path: PathBuf,
    terminal_receipts: usize,
    snapshot: Mutex<TaskRuntimeSnapshot>,
}

impl FileTaskRuntimeStore {
    /// Open task runtime state and make interrupted executions recoverable.
    pub fn open(state_dir: PathBuf, terminal_receipts: usize) -> Result<Self> {
        fs::create_dir_all(&state_dir)
            .with_context(|| format!("create task runtime directory {}", state_dir.display()))?;
        let path = state_dir.join("task-runtime.json");
        let mut snapshot = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse task runtime {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                TaskRuntimeSnapshot::default()
            }
            Err(error) => return Err(error).context("read task runtime"),
        };
        let now = Utc::now();
        let mut recovered = false;
        for item in &mut snapshot.inbox {
            if item.state == TaskExecutionState::Running {
                item.state = TaskExecutionState::Queued;
                item.updated_at = now;
                item.revision = item.revision.saturating_add(1);
                item.recovered = true;
                recovered = true;
            }
        }
        let store = Self {
            path,
            terminal_receipts: terminal_receipts.max(1),
            snapshot: Mutex::new(snapshot),
        };
        if recovered {
            let snapshot = store
                .snapshot
                .try_lock()
                .expect("new task runtime store cannot be locked");
            store.persist_blocking(&snapshot)?;
        }
        Ok(store)
    }

    fn persist_blocking(&self, snapshot: &TaskRuntimeSnapshot) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(snapshot).context("serialize task runtime")?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, bytes)
            .with_context(|| format!("write task runtime {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .with_context(|| format!("replace task runtime {}", self.path.display()))
    }

    fn prune(&self, snapshot: &mut TaskRuntimeSnapshot) {
        let terminal_count = snapshot
            .inbox
            .iter()
            .filter(|item| item.state.is_terminal())
            .count();
        let mut remove = terminal_count.saturating_sub(self.terminal_receipts);
        snapshot.inbox.retain(|item| {
            if remove > 0 && item.state.is_terminal() {
                remove -= 1;
                false
            } else {
                true
            }
        });
        if snapshot.cancellations.len() > self.terminal_receipts {
            let remove = snapshot.cancellations.len() - self.terminal_receipts;
            snapshot.cancellations.drain(..remove);
        }
    }

    fn insert_item(
        snapshot: &mut TaskRuntimeSnapshot,
        item: TaskInboxItem,
    ) -> Result<EnqueueOutcome> {
        if snapshot.inbox.iter().any(|existing| {
            existing.submission.execution_run_id == item.submission.execution_run_id
        }) {
            return Ok(EnqueueOutcome::Duplicate);
        }
        if let Some(index) = snapshot
            .cancellations
            .iter()
            .position(|run_id| *run_id == item.submission.execution_run_id)
        {
            snapshot.cancellations.remove(index);
            let mut cancelled = item;
            cancelled.state = TaskExecutionState::Cancelled;
            cancelled.updated_at = Utc::now();
            cancelled.revision = cancelled.revision.saturating_add(1);
            snapshot.inbox.push(cancelled.clone());
            return Ok(EnqueueOutcome::Cancelled(Box::new(cancelled)));
        }
        if snapshot.inbox.iter().any(|existing| {
            existing.submission.task_id == item.submission.task_id && !existing.state.is_terminal()
        }) {
            bail!("task already has a queued or running execution");
        }
        snapshot.inbox.push(item.clone());
        Ok(EnqueueOutcome::Inserted(Box::new(item)))
    }
}

#[async_trait]
impl TaskRuntimeStore for FileTaskRuntimeStore {
    async fn enqueue(&self, item: TaskInboxItem) -> Result<EnqueueOutcome> {
        let mut snapshot = self.snapshot.lock().await;
        let outcome = Self::insert_item(&mut snapshot, item)?;
        if matches!(
            outcome,
            EnqueueOutcome::Inserted(_) | EnqueueOutcome::Cancelled(_)
        ) {
            self.prune(&mut snapshot);
            self.persist_blocking(&snapshot)?;
        }
        Ok(outcome)
    }

    async fn recoverable(&self) -> Result<Vec<TaskInboxItem>> {
        Ok(self
            .snapshot
            .lock()
            .await
            .inbox
            .iter()
            .filter(|item| item.state == TaskExecutionState::Queued)
            .cloned()
            .collect())
    }

    async fn transition(
        &self,
        execution_run_id: Uuid,
        mut state: TaskExecutionState,
    ) -> Result<Option<TaskInboxItem>> {
        let mut snapshot = self.snapshot.lock().await;
        let Some(item) = snapshot
            .inbox
            .iter_mut()
            .find(|item| item.submission.execution_run_id == execution_run_id)
        else {
            return Ok(None);
        };
        if item.state.is_terminal() {
            return Ok(None);
        }
        let valid = matches!(
            (&item.state, &state),
            (TaskExecutionState::Queued, TaskExecutionState::Running)
                | (TaskExecutionState::Queued, TaskExecutionState::Cancelled)
                | (
                    TaskExecutionState::Queued,
                    TaskExecutionState::Failed { .. }
                )
                | (
                    TaskExecutionState::Queued,
                    TaskExecutionState::Rejected { .. }
                )
                | (TaskExecutionState::Running, TaskExecutionState::Completed)
                | (
                    TaskExecutionState::Running,
                    TaskExecutionState::Failed { .. }
                )
                | (TaskExecutionState::Running, TaskExecutionState::Cancelled)
        );
        if !valid {
            bail!(
                "invalid task inbox transition from {:?} to {:?}",
                item.state,
                state
            );
        }
        match &mut state {
            TaskExecutionState::Failed { error } => truncate(error, 500),
            TaskExecutionState::Rejected { reason } => truncate(reason, 500),
            TaskExecutionState::Queued
            | TaskExecutionState::Running
            | TaskExecutionState::Completed
            | TaskExecutionState::Cancelled => {}
        }
        item.state = state;
        item.revision = item.revision.saturating_add(1);
        item.updated_at = Utc::now();
        let changed = item.clone();
        self.prune(&mut snapshot);
        self.persist_blocking(&snapshot)?;
        Ok(Some(changed))
    }

    async fn cancel(&self, execution_run_id: Uuid) -> Result<CancellationOutcome> {
        let mut snapshot = self.snapshot.lock().await;
        let Some(item) = snapshot
            .inbox
            .iter_mut()
            .find(|item| item.submission.execution_run_id == execution_run_id)
        else {
            if snapshot.cancellations.contains(&execution_run_id) {
                return Ok(CancellationOutcome::Inactive);
            }
            snapshot.cancellations.push(execution_run_id);
            self.prune(&mut snapshot);
            self.persist_blocking(&snapshot)?;
            return Ok(CancellationOutcome::Recorded);
        };
        match &item.state {
            TaskExecutionState::Queued => {
                item.state = TaskExecutionState::Cancelled;
                item.revision = item.revision.saturating_add(1);
                item.updated_at = Utc::now();
                let changed = item.clone();
                self.prune(&mut snapshot);
                self.persist_blocking(&snapshot)?;
                Ok(CancellationOutcome::Queued(Box::new(changed)))
            }
            TaskExecutionState::Running => {
                item.state = TaskExecutionState::Cancelled;
                item.revision = item.revision.saturating_add(1);
                item.updated_at = Utc::now();
                let changed = item.clone();
                self.prune(&mut snapshot);
                self.persist_blocking(&snapshot)?;
                Ok(CancellationOutcome::Running(Box::new(changed)))
            }
            TaskExecutionState::Completed
            | TaskExecutionState::Failed { .. }
            | TaskExecutionState::Cancelled
            | TaskExecutionState::Rejected { .. } => Ok(CancellationOutcome::Inactive),
        }
    }

    async fn schedules(&self) -> Result<Vec<TaskSchedule>> {
        Ok(self.snapshot.lock().await.schedules.clone())
    }

    async fn replace_schedules(&self, schedules: Vec<TaskSchedule>) -> Result<()> {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.schedules = schedules
            .into_iter()
            .map(|mut incoming| {
                if let Some(existing) = snapshot.schedules.iter().find(|schedule| {
                    schedule.id == incoming.id && schedule.revision == incoming.revision
                }) && (existing.next_run_at > incoming.next_run_at || !existing.enabled)
                {
                    incoming.next_run_at = existing.next_run_at;
                    incoming.occurrence_count = existing.occurrence_count;
                    incoming.enabled = existing.enabled;
                }
                incoming
            })
            .collect();
        self.persist_blocking(&snapshot)
    }

    async fn materialize_occurrence(
        &self,
        schedule_id: Uuid,
        scheduled_for: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
        mut item: TaskInboxItem,
        rejection: Option<String>,
    ) -> Result<OccurrenceOutcome> {
        let mut snapshot = self.snapshot.lock().await;
        let Some(schedule) = snapshot
            .schedules
            .iter_mut()
            .find(|schedule| schedule.id == schedule_id)
        else {
            return Ok(OccurrenceOutcome::Stale);
        };
        if schedule.next_run_at != scheduled_for {
            return Ok(OccurrenceOutcome::Stale);
        }
        schedule.occurrence_count = schedule.occurrence_count.saturating_add(1);
        if let Some(next_run_at) = next_run_at {
            schedule.next_run_at = next_run_at;
        } else {
            schedule.enabled = false;
        }

        let duplicate = snapshot.inbox.iter().any(|existing| {
            existing.submission.execution_run_id == item.submission.execution_run_id
        });
        let overlap = snapshot.inbox.iter().any(|existing| {
            existing.submission.task_id == item.submission.task_id && !existing.state.is_terminal()
        });
        let outcome = if duplicate {
            OccurrenceOutcome::Duplicate
        } else if let Some(reason) = rejection {
            item.state = TaskExecutionState::Rejected { reason };
            snapshot.inbox.push(item.clone());
            OccurrenceOutcome::Rejected(Box::new(item))
        } else if overlap {
            item.state = TaskExecutionState::Rejected {
                reason: "task already has a queued or running execution".to_string(),
            };
            snapshot.inbox.push(item.clone());
            OccurrenceOutcome::Rejected(Box::new(item))
        } else {
            snapshot.inbox.push(item.clone());
            OccurrenceOutcome::Enqueued(Box::new(item))
        };
        self.prune(&mut snapshot);
        self.persist_blocking(&snapshot)?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests;
