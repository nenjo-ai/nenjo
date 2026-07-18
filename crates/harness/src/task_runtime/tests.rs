#[cfg(feature = "local-runtime")]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[cfg(feature = "local-runtime")]
use anyhow::Result;
use chrono::{DateTime, Utc};
use nenjo_events::{TaskScheduleDefinition, TaskScheduleEnd, TaskScheduleRecurrence};
#[cfg(feature = "local-runtime")]
use tempfile::tempdir;
#[cfg(feature = "local-runtime")]
use tokio::sync::Notify;
#[cfg(feature = "local-runtime")]
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{TaskContent, TaskExecutionTarget, TaskSchedule};
#[cfg(feature = "local-runtime")]
use super::{TaskExecutorOutcome, TaskRuntime, TaskSubmission, TaskTrigger};
#[cfg(feature = "local-runtime")]
use crate::local_runtime::FileTaskRuntimeStore;

fn definition(recurrence: TaskScheduleRecurrence) -> TaskScheduleDefinition {
    TaskScheduleDefinition {
        starts_at: "2026-07-15T12:00:00Z".parse().unwrap(),
        timezone: "UTC".to_string(),
        recurrence,
        end: TaskScheduleEnd::Never,
    }
}

fn schedule(definition: TaskScheduleDefinition) -> TaskSchedule {
    TaskSchedule {
        id: Uuid::new_v4(),
        task_id: Uuid::new_v4(),
        authorized_by: Uuid::new_v4(),
        definition,
        next_run_at: "2026-07-15T12:00:00Z".parse().unwrap(),
        occurrence_count: 0,
        enabled: true,
        runnable: true,
        project: None,
        target: TaskExecutionTarget::Agent("coder".to_string()),
        content: TaskContent {
            title: "task".to_string(),
            instructions: "work".to_string(),
            slug: None,
            labels: Vec::new(),
            status: None,
            priority: None,
        },
        revision: "1".to_string(),
    }
}

#[test]
fn schedule_preserves_the_canonical_definition_in_persistence() {
    let schedule = schedule(definition(TaskScheduleRecurrence::Daily { interval: 1 }));
    let value = serde_json::to_value(&schedule).unwrap();
    assert_eq!(value["definition"]["recurrence"]["frequency"], "daily");
    let restored: TaskSchedule = serde_json::from_value(value).unwrap();
    assert_eq!(restored.definition, schedule.definition);
}

#[test]
fn structured_finite_schedule_reports_no_next_occurrence() {
    let starts_at: DateTime<Utc> = "2026-07-15T12:00:00Z".parse().unwrap();
    let schedule = schedule(TaskScheduleDefinition {
        starts_at,
        timezone: "UTC".to_string(),
        recurrence: TaskScheduleRecurrence::Daily { interval: 1 },
        end: TaskScheduleEnd::After { occurrences: 1 },
    });
    assert_eq!(schedule.definition.next_after(starts_at, 1).unwrap(), None);
}

#[cfg(feature = "local-runtime")]
struct TrackingExecutor {
    active: AtomicUsize,
    max_active: AtomicUsize,
    completed: AtomicUsize,
    notify: Notify,
}

#[cfg(feature = "local-runtime")]
struct CancellableExecutor {
    started: Notify,
    release: Notify,
}

#[cfg(feature = "local-runtime")]
impl CancellableExecutor {
    async fn execute(
        &self,
        _submission: TaskSubmission,
        cancellation: CancellationToken,
    ) -> Result<TaskExecutorOutcome> {
        self.started.notify_one();
        cancellation.cancelled().await;
        self.release.notified().await;
        Ok(TaskExecutorOutcome::Cancelled)
    }
}

#[cfg(feature = "local-runtime")]
impl TrackingExecutor {
    async fn execute(
        &self,
        _submission: TaskSubmission,
        _cancellation: CancellationToken,
    ) -> Result<TaskExecutorOutcome> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.completed.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(TaskExecutorOutcome::Completed)
    }
}

#[cfg(feature = "local-runtime")]
#[tokio::test]
async fn manual_and_scheduled_tasks_share_one_concurrency_gate() {
    let dir = tempdir().unwrap();
    let store = Arc::new(FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap());
    let executor = Arc::new(TrackingExecutor {
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
        notify: Notify::new(),
    });
    let execute = {
        let executor = executor.clone();
        move |submission, cancellation| {
            let executor = executor.clone();
            async move { executor.execute(submission, cancellation).await }
        }
    };
    let runtime = TaskRuntime::new(store, execute, 1);
    let mut due = schedule(definition(TaskScheduleRecurrence::Interval {
        every: 5,
        unit: nenjo_events::TaskScheduleIntervalUnit::Minutes,
    }));
    due.next_run_at = Utc::now() - chrono::Duration::minutes(1);
    runtime.replace_schedules(vec![due]).await.unwrap();
    runtime.start().await.unwrap();
    runtime
        .submit(TaskSubmission {
            requested_by: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            execution_run_id: Uuid::new_v4(),
            project: None,
            target: TaskExecutionTarget::Agent("coder".to_string()),
            content: TaskContent {
                title: "manual".to_string(),
                instructions: "work".to_string(),
                slug: None,
                labels: Vec::new(),
                status: None,
                priority: None,
            },
            trigger: TaskTrigger::Manual,
        })
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while executor.completed.load(Ordering::SeqCst) < 2 {
            executor.notify.notified().await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown();
    assert_eq!(executor.max_active.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "local-runtime")]
#[tokio::test]
async fn handled_executor_failure_is_persisted_as_failed() {
    let dir = tempdir().unwrap();
    let store = Arc::new(FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap());
    let runtime = TaskRuntime::new(
        store,
        |_submission, _cancellation| async {
            Ok(TaskExecutorOutcome::Failed("expected failure".to_string()))
        },
        1,
    );
    let mut events = runtime.events().await.unwrap();
    runtime.start().await.unwrap();
    runtime
        .submit(TaskSubmission {
            requested_by: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            execution_run_id: Uuid::new_v4(),
            project: None,
            target: TaskExecutionTarget::Agent("coder".to_string()),
            content: TaskContent {
                title: "failing task".to_string(),
                instructions: "fail".to_string(),
                slug: None,
                labels: Vec::new(),
                status: None,
                priority: None,
            },
            trigger: TaskTrigger::Manual,
        })
        .await
        .unwrap();

    let failed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.expect("task runtime event stream");
            if matches!(event.item.state, super::TaskExecutionState::Failed { .. }) {
                break event.item;
            }
        }
    })
    .await
    .unwrap();
    runtime.shutdown();
    assert_eq!(
        failed.state,
        super::TaskExecutionState::Failed {
            error: "expected failure".to_string()
        }
    );
}

#[cfg(feature = "local-runtime")]
#[tokio::test]
async fn cancelling_running_work_flows_through_the_host_executor() {
    let dir = tempdir().unwrap();
    let store = Arc::new(FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap());
    let executor = Arc::new(CancellableExecutor {
        started: Notify::new(),
        release: Notify::new(),
    });
    let execute = {
        let executor = executor.clone();
        move |submission, cancellation| {
            let executor = executor.clone();
            async move { executor.execute(submission, cancellation).await }
        }
    };
    let runtime = TaskRuntime::new(store, execute, 1);
    let mut events = runtime.events().await.unwrap();
    runtime.start().await.unwrap();
    let run_id = Uuid::new_v4();
    runtime
        .submit(TaskSubmission {
            requested_by: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            execution_run_id: run_id,
            project: None,
            target: TaskExecutionTarget::Agent("coder".to_string()),
            content: TaskContent {
                title: "cancel me".to_string(),
                instructions: "wait".to_string(),
                slug: None,
                labels: Vec::new(),
                status: None,
                priority: None,
            },
            trigger: TaskTrigger::Manual,
        })
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        executor.started.notified(),
    )
    .await
    .unwrap();
    runtime.cancel(run_id).await.unwrap();
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.expect("task runtime event stream");
            if event.item.state == super::TaskExecutionState::Cancelled {
                break event.item;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(cancelled.submission.execution_run_id, run_id);
    executor.release.notify_one();
    runtime.shutdown();
}
