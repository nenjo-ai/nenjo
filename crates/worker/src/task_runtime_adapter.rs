//! Worker protocol and execution adapters for the harness-owned task runtime.

use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use dashmap::DashMap;
use nenjo_events::{
    Command, Response, TaskEncryptedContent, TaskExecutionOrigin,
    TaskExecutionState as WireTaskExecutionState, TaskExecutionTrigger, TaskScheduleAssignment,
};
use nenjo_harness::{
    TaskContent, TaskExecutionState, TaskExecutorOutcome, TaskRuntimeEvent, TaskSchedule,
    TaskSubmission, TaskTrigger,
};
use uuid::Uuid;

use crate::crypto::{WorkerAuthProvider, decrypt_text_with_provider};
use crate::event_bridge::{ExecutionTaskArtifactsResponse, execution_task_artifacts_response};
use crate::event_loop::{ResponseSender, RoutedResponse};
use crate::handlers::task::{TaskExecuteRequest, WorkerTaskHarnessExt};
use crate::runtime::CommandContext;

/// Adapt a decoded manual command into the transport-independent inbox model.
pub(crate) fn task_submission(command: Command, requested_by: Uuid) -> Result<TaskSubmission> {
    let Command::TaskExecute {
        task_id,
        project,
        target,
        execution_run_id,
        trigger,
        payload,
        encrypted_payload: _,
    } = command
    else {
        anyhow::bail!("task runtime accepts only task.execute commands");
    };
    let payload = payload.context("task.execute missing payload after command decode")?;
    let trigger = match trigger {
        TaskExecutionTrigger::Manual => TaskTrigger::Manual,
        TaskExecutionTrigger::Retry => TaskTrigger::Retry,
    };
    Ok(TaskSubmission {
        requested_by,
        task_id,
        execution_run_id,
        project,
        target,
        content: task_content(payload),
        trigger,
    })
}

/// Decrypt task instructions and remove transport ciphertext before local persistence.
pub(crate) async fn hydrate_task_schedule(
    auth_provider: &WorkerAuthProvider,
    mut schedule: TaskScheduleAssignment,
) -> Result<TaskScheduleAssignment> {
    let Some(encrypted) = schedule.encrypted_payload.take() else {
        return Ok(schedule);
    };
    if encrypted.object_id != schedule.task_id
        || encrypted.object_type != "task_content"
        || encrypted.encryption_scope.as_deref() != Some("org")
    {
        anyhow::bail!("task schedule ciphertext identity is invalid");
    }
    let plaintext = decrypt_text_with_provider(auth_provider, &encrypted).await?;
    let content: TaskEncryptedContent =
        serde_json::from_str(&plaintext).context("parse decrypted task schedule content")?;
    schedule
        .payload
        .as_mut()
        .context("scheduled task is missing plaintext metadata")?
        .instructions = content.instructions;
    Ok(schedule)
}

/// Persist a hydrated plaintext assignment snapshot for offline restoration.
pub(crate) fn persist_schedule_cache(
    path: &Path,
    schedules: &[TaskScheduleAssignment],
) -> Result<()> {
    if schedules
        .iter()
        .any(|schedule| schedule.encrypted_payload.is_some())
    {
        anyhow::bail!("task schedule cache accepts only hydrated plaintext assignments");
    }
    let parent = path.parent().context("task schedule cache has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(schedules)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

/// Adapt one decrypted platform assignment into a validated harness schedule.
pub(crate) fn task_schedule(schedule: TaskScheduleAssignment) -> Result<TaskSchedule> {
    let payload = schedule
        .payload
        .context("scheduled task is missing hydrated content")?;
    Ok(TaskSchedule {
        id: schedule.id,
        task_id: schedule.task_id,
        authorized_by: schedule.authorized_by_user_id,
        definition: schedule.definition,
        next_run_at: chrono::DateTime::parse_from_rfc3339(&schedule.next_run_at)
            .context("scheduled task has invalid next_run_at")?
            .with_timezone(&chrono::Utc),
        occurrence_count: schedule.occurrence_count,
        enabled: schedule.enabled,
        runnable: schedule.runnable,
        project: schedule.project,
        target: schedule.target,
        content: task_content(payload),
        revision: schedule.updated_at,
    })
}

fn task_content(payload: nenjo_events::TaskExecuteContent) -> TaskContent {
    TaskContent {
        title: payload.title,
        instructions: payload.instructions.unwrap_or_default(),
        slug: payload.slug,
        labels: payload.labels,
        status: payload.status,
        priority: payload.priority,
    }
}

/// Convert a durable harness transition into the platform lifecycle protocol.
pub(crate) fn task_runtime_response(event: TaskRuntimeEvent) -> Response {
    let item = event.item;
    let state = match item.state {
        TaskExecutionState::Queued => WireTaskExecutionState::Queued,
        TaskExecutionState::Running => WireTaskExecutionState::Running,
        TaskExecutionState::Completed => WireTaskExecutionState::Completed,
        TaskExecutionState::Failed { error } => WireTaskExecutionState::Failed { error },
        TaskExecutionState::Cancelled => WireTaskExecutionState::Cancelled,
        TaskExecutionState::Rejected { reason } => WireTaskExecutionState::Rejected { reason },
    };
    let origin = match item.submission.trigger {
        TaskTrigger::Manual => TaskExecutionOrigin::Manual,
        TaskTrigger::Retry => TaskExecutionOrigin::Retry,
        TaskTrigger::Schedule {
            schedule_id,
            scheduled_for,
            next_run_at,
            assignment_revision,
        } => TaskExecutionOrigin::Schedule {
            schedule_id,
            scheduled_for: scheduled_for.to_rfc3339(),
            next_run_at: next_run_at.map(|value| value.to_rfc3339()),
            assignment_revision,
            project: item.submission.project.clone(),
            target: item.submission.target.clone(),
        },
    };
    Response::TaskExecutionState {
        execution_run_id: item.submission.execution_run_id,
        task_id: item.submission.task_id,
        state,
        origin,
        revision: item.revision,
        recovered: item.recovered,
    }
}

pub(crate) type PendingTaskArtifacts = Arc<DashMap<Uuid, Response>>;

/// Responses emitted for one durable task-runtime transition.
pub(crate) struct TaskRuntimeResponses {
    pub(crate) lifecycle: Response,
    pub(crate) artifacts: Option<Response>,
}

/// Order provider-specific terminal data after its durable lifecycle event.
pub(crate) fn task_runtime_responses(
    event: TaskRuntimeEvent,
    pending_artifacts: &PendingTaskArtifacts,
) -> TaskRuntimeResponses {
    let execution_run_id = event.item.submission.execution_run_id;
    let task_id = event.item.submission.task_id;
    let terminal = event.item.state.is_terminal();
    let lifecycle = task_runtime_response(event);
    let artifacts = terminal.then(|| {
        pending_artifacts
            .remove(&execution_run_id)
            .map(|(_, response)| response)
            .unwrap_or_else(|| {
                execution_task_artifacts_response(ExecutionTaskArtifactsResponse {
                    execution_run_id,
                    task_id: Some(task_id),
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    attachments: Vec::new(),
                })
            })
    });
    TaskRuntimeResponses {
        lifecycle,
        artifacts,
    }
}

/// Worker context needed to route a persisted submission through the harness.
#[derive(Clone)]
pub(crate) struct WorkerTaskExecutor {
    pub(crate) base_context: CommandContext,
    pub(crate) response_tx: tokio::sync::mpsc::UnboundedSender<RoutedResponse>,
    pub(crate) system_response_tx: ResponseSender,
    pub(crate) pending_artifacts: PendingTaskArtifacts,
}

impl WorkerTaskExecutor {
    /// Execute one task submission with the actor and response routes it was accepted under.
    pub(crate) async fn execute(
        &self,
        submission: TaskSubmission,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<TaskExecutorOutcome> {
        let mut context = self.base_context.clone();
        context.actor_user_id = submission.requested_by;
        context.response_tx =
            ResponseSender::for_actor(self.response_tx.clone(), submission.requested_by);
        context.org_response_tx = self.system_response_tx.clone();
        let result = context
            .harness
            .handle_task_execute(
                &context.task_context(),
                TaskExecuteRequest {
                    task_id: submission.task_id,
                    project: submission.project.as_deref(),
                    target: &submission.target,
                    execution_run_id: submission.execution_run_id,
                    title: &submission.content.title,
                    instructions: &submission.content.instructions,
                    slug: submission.content.slug.as_deref(),
                    labels: &submission.content.labels,
                    status: submission.content.status.as_deref(),
                    priority: submission.content.priority.as_deref(),
                    cancellation,
                },
            )
            .await?;
        if !matches!(&result.outcome, TaskExecutorOutcome::Cancelled) {
            self.pending_artifacts
                .insert(submission.execution_run_id, result.artifacts);
        }
        Ok(result.outcome)
    }
}

#[cfg(test)]
mod tests {
    use nenjo_events::{
        Command, EncryptedPayload, ExecutionEventPayload, Response, TaskExecuteContent,
        TaskExecutionState as WireTaskExecutionState, TaskExecutionTarget, TaskExecutionTrigger,
        TaskScheduleAssignment, TaskScheduleDefinition, TaskScheduleEnd, TaskScheduleRecurrence,
    };
    use nenjo_harness::{
        TaskContent, TaskExecutionState, TaskInboxItem, TaskRuntimeEvent, TaskSubmission,
        TaskTrigger,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{
        PendingTaskArtifacts, persist_schedule_cache, task_runtime_responses, task_submission,
    };
    use crate::event_bridge::{ExecutionTaskArtifactsResponse, execution_task_artifacts_response};

    fn task_command(trigger: TaskExecutionTrigger) -> Command {
        Command::TaskExecute {
            task_id: Uuid::new_v4(),
            project: None,
            target: TaskExecutionTarget::Agent("coder".to_string()),
            execution_run_id: Uuid::new_v4(),
            trigger,
            payload: Some(TaskExecuteContent {
                title: "task".to_string(),
                instructions: Some("work".to_string()),
                slug: None,
                labels: Vec::new(),
                status: None,
                priority: None,
            }),
            encrypted_payload: None,
        }
    }

    fn schedule_assignment() -> TaskScheduleAssignment {
        TaskScheduleAssignment {
            id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            authorized_by_user_id: Uuid::new_v4(),
            definition: TaskScheduleDefinition {
                starts_at: "2026-07-17T12:00:00Z".parse().unwrap(),
                timezone: "UTC".to_string(),
                recurrence: TaskScheduleRecurrence::Daily { interval: 1 },
                end: TaskScheduleEnd::Never,
            },
            occurrence_count: 0,
            next_run_at: "2026-07-18T12:00:00Z".to_string(),
            enabled: true,
            project: None,
            target: TaskExecutionTarget::Agent("coder".to_string()),
            payload: Some(TaskExecuteContent {
                title: "Plain task".to_string(),
                instructions: Some("stored as plain JSON".to_string()),
                slug: Some("plain-task".to_string()),
                labels: vec!["ops".to_string()],
                status: None,
                priority: None,
            }),
            encrypted_payload: None,
            runnable: true,
            updated_at: "2026-07-17T12:00:00Z".to_string(),
        }
    }

    fn runtime_event(state: TaskExecutionState) -> TaskRuntimeEvent {
        let now = chrono::Utc::now();
        TaskRuntimeEvent {
            item: TaskInboxItem {
                submission: TaskSubmission {
                    requested_by: Uuid::new_v4(),
                    task_id: Uuid::new_v4(),
                    execution_run_id: Uuid::new_v4(),
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
                },
                state,
                queued_at: now,
                updated_at: now,
                revision: 2,
                recovered: false,
            },
        }
    }

    #[test]
    fn projectless_command_stays_projectless_in_harness_submission() {
        let submission =
            task_submission(task_command(TaskExecutionTrigger::Retry), Uuid::new_v4()).unwrap();
        assert!(submission.project.is_none());
        assert_eq!(submission.trigger, nenjo_harness::TaskTrigger::Retry);
    }

    #[test]
    fn schedule_cache_persists_plaintext_task_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("task_schedules.json");
        persist_schedule_cache(&path, &[schedule_assignment()]).unwrap();

        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value[0]["payload"]["instructions"], "stored as plain JSON");
        assert!(value[0].get("encrypted_payload").is_none());
    }

    #[test]
    fn schedule_cache_rejects_transport_ciphertext() {
        let dir = tempdir().unwrap();
        let mut schedule = schedule_assignment();
        schedule.encrypted_payload = Some(EncryptedPayload {
            account_id: Uuid::new_v4(),
            encryption_scope: Some("org".to_string()),
            object_id: schedule.task_id,
            object_type: "task_content".to_string(),
            algorithm: "xchacha20poly1305".to_string(),
            key_version: 1,
            nonce: "nonce".to_string(),
            ciphertext: "ciphertext".to_string(),
        });

        let error = persist_schedule_cache(&dir.path().join("task_schedules.json"), &[schedule])
            .unwrap_err();
        assert!(error.to_string().contains("hydrated plaintext"));
    }

    #[test]
    fn terminal_runtime_transition_drains_artifacts_after_lifecycle() {
        let event = runtime_event(TaskExecutionState::Completed);
        let execution_run_id = event.item.submission.execution_run_id;
        let task_id = event.item.submission.task_id;
        let pending = PendingTaskArtifacts::default();
        pending.insert(
            execution_run_id,
            execution_task_artifacts_response(ExecutionTaskArtifactsResponse {
                execution_run_id,
                task_id: Some(task_id),
                total_input_tokens: 11,
                total_output_tokens: 12,
                attachments: Vec::new(),
            }),
        );

        let responses = task_runtime_responses(event, &pending);

        assert!(matches!(
            responses.lifecycle,
            Response::TaskExecutionState {
                state: WireTaskExecutionState::Completed,
                ..
            }
        ));
        assert!(matches!(
            responses.artifacts,
            Some(Response::ExecutionEvent {
                event: ExecutionEventPayload::TaskArtifacts(ref artifacts),
                ..
            }) if artifacts.total_input_tokens == 11 && artifacts.total_output_tokens == 12
        ));
        assert!(!pending.contains_key(&execution_run_id));
    }

    #[test]
    fn running_runtime_transition_does_not_drain_artifacts() {
        let event = runtime_event(TaskExecutionState::Running);
        let execution_run_id = event.item.submission.execution_run_id;
        let task_id = event.item.submission.task_id;
        let pending = PendingTaskArtifacts::default();
        pending.insert(
            execution_run_id,
            execution_task_artifacts_response(ExecutionTaskArtifactsResponse {
                execution_run_id,
                task_id: Some(task_id),
                total_input_tokens: 1,
                total_output_tokens: 2,
                attachments: Vec::new(),
            }),
        );

        let responses = task_runtime_responses(event, &pending);

        assert!(responses.artifacts.is_none());
        assert!(pending.contains_key(&execution_run_id));
    }
}
