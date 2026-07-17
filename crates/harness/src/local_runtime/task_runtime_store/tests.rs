use chrono::{Duration, Utc};
use nenjo_events::{TaskScheduleDefinition, TaskScheduleEnd, TaskScheduleRecurrence};
use tempfile::tempdir;
use uuid::Uuid;

use super::FileTaskRuntimeStore;
use crate::task_runtime::{
    CancellationOutcome, EnqueueOutcome, OccurrenceOutcome, TaskContent, TaskExecutionState,
    TaskExecutionTarget, TaskInboxItem, TaskRuntimeStore, TaskSchedule, TaskSubmission,
    TaskTrigger,
};

fn submission(task_id: Uuid, execution_run_id: Uuid) -> TaskSubmission {
    TaskSubmission {
        requested_by: Uuid::new_v4(),
        task_id,
        execution_run_id,
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
        trigger: TaskTrigger::Manual,
    }
}

fn schedule(task_id: Uuid, next_run_at: chrono::DateTime<Utc>) -> TaskSchedule {
    TaskSchedule {
        id: Uuid::new_v4(),
        task_id,
        authorized_by: Uuid::new_v4(),
        definition: TaskScheduleDefinition {
            starts_at: next_run_at,
            timezone: "UTC".to_string(),
            recurrence: TaskScheduleRecurrence::Interval {
                every: 5,
                unit: nenjo_events::TaskScheduleIntervalUnit::Minutes,
            },
            end: TaskScheduleEnd::Never,
        },
        next_run_at,
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

#[tokio::test]
async fn enqueue_is_idempotent_and_rejects_same_task_overlap() {
    let dir = tempdir().unwrap();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let task_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let item = TaskInboxItem::queued(submission(task_id, run_id), Utc::now());
    assert!(matches!(
        store.enqueue(item.clone()).await.unwrap(),
        EnqueueOutcome::Inserted(_)
    ));
    assert_eq!(
        store.enqueue(item).await.unwrap(),
        EnqueueOutcome::Duplicate
    );
    let overlapping = TaskInboxItem::queued(submission(task_id, Uuid::new_v4()), Utc::now());
    assert!(store.enqueue(overlapping).await.is_err());
}

#[tokio::test]
async fn running_receipts_recover_as_queued_and_are_persisted() {
    let dir = tempdir().unwrap();
    let task_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    store
        .enqueue(TaskInboxItem::queued(
            submission(task_id, run_id),
            Utc::now(),
        ))
        .await
        .unwrap();
    store
        .transition(run_id, TaskExecutionState::Running)
        .await
        .unwrap();
    drop(store);
    let recovered = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let receipts = recovered.recoverable().await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].recovered);
    drop(recovered);
    let reopened = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    assert_eq!(reopened.recoverable().await.unwrap().len(), 1);
}

#[tokio::test]
async fn queued_cancellation_is_atomic_and_terminal() {
    let dir = tempdir().unwrap();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let run_id = Uuid::new_v4();
    store
        .enqueue(TaskInboxItem::queued(
            submission(Uuid::new_v4(), run_id),
            Utc::now(),
        ))
        .await
        .unwrap();
    let outcome = store.cancel(run_id).await.unwrap();
    assert!(matches!(outcome, CancellationOutcome::Queued(_)));
    assert!(store.recoverable().await.unwrap().is_empty());
    assert!(matches!(
        store.cancel(run_id).await.unwrap(),
        CancellationOutcome::Inactive
    ));
}

#[tokio::test]
async fn cancellation_before_delivery_survives_restart_and_consumes_submission() {
    let dir = tempdir().unwrap();
    let run_id = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    assert!(matches!(
        store.cancel(run_id).await.unwrap(),
        CancellationOutcome::Recorded
    ));
    drop(store);

    let reopened = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let outcome = reopened
        .enqueue(TaskInboxItem::queued(
            submission(task_id, run_id),
            Utc::now(),
        ))
        .await
        .unwrap();
    let EnqueueOutcome::Cancelled(item) = outcome else {
        panic!("early cancellation must consume the delayed submission");
    };
    assert_eq!(item.state, TaskExecutionState::Cancelled);
    assert!(reopened.recoverable().await.unwrap().is_empty());
}

#[tokio::test]
async fn lifecycle_rejects_invalid_transitions_and_ignores_terminal_replays() {
    let dir = tempdir().unwrap();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let run_id = Uuid::new_v4();
    store
        .enqueue(TaskInboxItem::queued(
            submission(Uuid::new_v4(), run_id),
            Utc::now(),
        ))
        .await
        .unwrap();
    assert!(
        store
            .transition(run_id, TaskExecutionState::Completed)
            .await
            .is_err()
    );
    store
        .transition(run_id, TaskExecutionState::Running)
        .await
        .unwrap();
    store
        .transition(run_id, TaskExecutionState::Completed)
        .await
        .unwrap();
    assert!(
        store
            .transition(run_id, TaskExecutionState::Running)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn scheduled_occurrence_advances_and_enqueues_atomically() {
    let dir = tempdir().unwrap();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let scheduled_for = Utc::now() - Duration::minutes(1);
    let next_run_at = Utc::now() + Duration::minutes(4);
    let schedule = schedule(Uuid::new_v4(), scheduled_for);
    let schedule_id = schedule.id;
    let submission = TaskSubmission {
        requested_by: schedule.authorized_by,
        task_id: schedule.task_id,
        execution_run_id: Uuid::new_v5(&schedule_id, scheduled_for.to_rfc3339().as_bytes()),
        project: None,
        target: schedule.target.clone(),
        content: schedule.content.clone(),
        trigger: TaskTrigger::Schedule {
            schedule_id,
            scheduled_for,
            next_run_at: Some(next_run_at),
            assignment_revision: "v1".to_string(),
        },
    };
    store.replace_schedules(vec![schedule]).await.unwrap();
    assert!(matches!(
        store
            .materialize_occurrence(
                schedule_id,
                scheduled_for,
                Some(next_run_at),
                TaskInboxItem::queued(submission.clone(), Utc::now()),
                None
            )
            .await
            .unwrap(),
        OccurrenceOutcome::Enqueued(_)
    ));
    assert_eq!(store.schedules().await.unwrap()[0].next_run_at, next_run_at);
    assert!(matches!(
        store
            .materialize_occurrence(
                schedule_id,
                scheduled_for,
                Some(next_run_at),
                TaskInboxItem::queued(submission, Utc::now()),
                None
            )
            .await
            .unwrap(),
        OccurrenceOutcome::Stale
    ));
}

#[tokio::test]
async fn final_occurrence_disables_schedule_and_records_progress_atomically() {
    let dir = tempdir().unwrap();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let scheduled_for = Utc::now() - Duration::minutes(1);
    let schedule = schedule(Uuid::new_v4(), scheduled_for);
    let schedule_id = schedule.id;
    let submission = TaskSubmission {
        requested_by: schedule.authorized_by,
        task_id: schedule.task_id,
        execution_run_id: Uuid::new_v5(&schedule_id, scheduled_for.to_rfc3339().as_bytes()),
        project: None,
        target: schedule.target.clone(),
        content: schedule.content.clone(),
        trigger: TaskTrigger::Schedule {
            schedule_id,
            scheduled_for,
            next_run_at: None,
            assignment_revision: "v1".to_string(),
        },
    };
    store.replace_schedules(vec![schedule]).await.unwrap();
    store
        .materialize_occurrence(
            schedule_id,
            scheduled_for,
            None,
            TaskInboxItem::queued(submission, Utc::now()),
            None,
        )
        .await
        .unwrap();
    let persisted = &store.schedules().await.unwrap()[0];
    assert!(!persisted.enabled);
    assert_eq!(persisted.occurrence_count, 1);
}

#[tokio::test]
async fn terminal_receipts_are_pruned_to_the_configured_bound() {
    let dir = tempdir().unwrap();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 1).unwrap();
    for _ in 0..2 {
        let run_id = Uuid::new_v4();
        store
            .enqueue(TaskInboxItem::queued(
                submission(Uuid::new_v4(), run_id),
                Utc::now(),
            ))
            .await
            .unwrap();
        store
            .transition(run_id, TaskExecutionState::Running)
            .await
            .unwrap();
        store
            .transition(run_id, TaskExecutionState::Completed)
            .await
            .unwrap();
    }
    drop(store);
    let reopened = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 1).unwrap();
    let snapshot = reopened.snapshot.lock().await;
    assert_eq!(snapshot.inbox.len(), 1);
    assert!(snapshot.inbox[0].state.is_terminal());
}

#[tokio::test]
async fn unchanged_snapshot_does_not_rewind_local_schedule_progress() {
    let dir = tempdir().unwrap();
    let store = FileTaskRuntimeStore::open(dir.path().to_path_buf(), 10).unwrap();
    let original = Utc::now() - Duration::minutes(5);
    let advanced = Utc::now() + Duration::minutes(5);
    let mut incoming = schedule(Uuid::new_v4(), original);
    let schedule_id = incoming.id;
    store
        .replace_schedules(vec![incoming.clone()])
        .await
        .unwrap();
    let submission = TaskSubmission {
        requested_by: incoming.authorized_by,
        task_id: incoming.task_id,
        execution_run_id: Uuid::new_v4(),
        project: None,
        target: incoming.target.clone(),
        content: incoming.content.clone(),
        trigger: TaskTrigger::Schedule {
            schedule_id,
            scheduled_for: original,
            next_run_at: Some(advanced),
            assignment_revision: "v1".to_string(),
        },
    };
    store
        .materialize_occurrence(
            schedule_id,
            original,
            Some(advanced),
            TaskInboxItem::queued(submission, Utc::now()),
            None,
        )
        .await
        .unwrap();
    store
        .replace_schedules(vec![incoming.clone()])
        .await
        .unwrap();
    assert_eq!(store.schedules().await.unwrap()[0].next_run_at, advanced);
    incoming.revision = "2".to_string();
    store.replace_schedules(vec![incoming]).await.unwrap();
    assert_eq!(store.schedules().await.unwrap()[0].next_run_at, original);
}
