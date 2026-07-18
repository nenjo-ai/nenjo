//! Harness-owned session lifecycle helpers for task and routine-step execution.

use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    ExecutionPhase, SessionCheckpointUpdate, SessionKind, SessionOwnerKind, SessionRefs,
    SessionRuntimeEvent, SessionStatus, SessionTransition, SessionUpsert, TaskSessionUpsert,
    WorktreeSnapshot,
};
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use crate::session::{TurnEventContext, session_runtime_events_from_turn_event};
use crate::{Harness, ProviderRuntime};

/// Derive the project memory namespace used by a task session.
pub fn task_memory_namespace(agent_name: Option<&str>, project_slug: &str) -> Option<String> {
    agent_name.map(|agent_name| {
        MemoryScope::new(
            agent_name,
            if project_slug.is_empty() {
                None
            } else {
                Some(project_slug)
            },
        )
        .project
    })
}

/// Identity and durable state for one task session upsert.
#[derive(Clone)]
pub struct TaskSessionRecord<'a> {
    pub task_id: Uuid,
    pub memory_namespace: Option<&'a str>,
    pub execution_run_id: Uuid,
    pub status: SessionStatus,
}

/// Whether a task-session write must finish before execution continues.
#[derive(Clone, Copy)]
pub enum SessionUpsertMode {
    Await,
    Spawn,
}

/// Persist the canonical task session shape through the configured runtime.
pub async fn upsert_task_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    params: &TaskSessionRecord<'_>,
    routine_slug: Option<&str>,
    project_slug: &str,
    agent_name: Option<&str>,
    agent_slug: Option<&str>,
    mode: SessionUpsertMode,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let upsert = TaskSessionUpsert {
        task_id: params.task_id,
        status: params.status,
        project: (!project_slug.is_empty()).then(|| project_slug.to_string()),
        agent: agent_slug.map(ToString::to_string),
        routine: routine_slug.map(ToString::to_string),
        execution_run_id: params.execution_run_id,
        memory_namespace: params.memory_namespace.map(ToOwned::to_owned),
        metadata: json!({
            "source": "harness_task",
            "project_slug": project_slug,
            "agent_name": agent_name,
        }),
    };

    match mode {
        SessionUpsertMode::Await => {
            if let Err(error) = harness.sessions().upsert_task(upsert).await {
                warn!(error = %error, task_id = %params.task_id, "Failed to upsert task session");
            }
        }
        SessionUpsertMode::Spawn => {
            let harness = harness.clone();
            let task_id = params.task_id;
            tokio::spawn(async move {
                if let Err(error) = harness.sessions().upsert_task(upsert).await {
                    warn!(error = %error, task_id = %task_id, "Failed to upsert task session");
                }
            });
        }
    }
}

/// Session identity for one agent-bearing routine step.
pub struct RoutineStepSessionRecord<'a> {
    pub parent_task_id: Uuid,
    pub step_run_id: Uuid,
    pub step_slug: &'a str,
    pub step_name: &'a str,
    pub project_slug: &'a str,
    pub routine_slug: Option<&'a str>,
    pub execution_run_id: Uuid,
    pub agent_slug: Option<&'a str>,
    pub agent_name: Option<&'a str>,
    pub memory_namespace: Option<&'a str>,
}

/// Build the routine-step upsert event shared by initial and streamed writes.
pub fn routine_step_session_upsert_event(
    params: &RoutineStepSessionRecord<'_>,
) -> SessionRuntimeEvent {
    SessionRuntimeEvent::SessionUpsert(SessionUpsert {
        session_id: params.step_run_id,
        kind: SessionKind::Task,
        status: SessionStatus::Active,
        agent: params.agent_slug.map(ToOwned::to_owned),
        project: (!params.project_slug.is_empty()).then(|| params.project_slug.to_string()),
        task_id: Some(params.parent_task_id),
        routine: params.routine_slug.map(ToOwned::to_owned),
        execution_run_id: Some(params.execution_run_id),
        parent_session_id: Some(params.parent_task_id),
        lease: None,
        memory_namespace: params.memory_namespace.map(ToOwned::to_owned),
        refs: SessionRefs {
            memory_namespace: params.memory_namespace.map(ToOwned::to_owned),
            ..Default::default()
        },
        metadata: json!({
            "source": "harness_routine_step",
            "project_slug": params.project_slug,
            "routine_slug": params.routine_slug,
            "parent_task_id": params.parent_task_id,
            "step_slug": params.step_slug,
            "step_run_id": params.step_run_id,
            "step_name": params.step_name,
            "agent_slug": params.agent_slug,
            "agent_name": params.agent_name,
        }),
    })
}

/// Record one routine-step turn event through the harness session writer.
pub fn record_routine_step_turn_event<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    params: &RoutineStepSessionRecord<'_>,
    agent_id: Option<Uuid>,
    event: &nenjo::TurnEvent,
    include_upsert: bool,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let context = TurnEventContext {
        session_id: params.step_run_id,
        turn_id: None,
        agent_id,
        agent_name: params.agent_name.map(ToOwned::to_owned),
        recorded_at: Utc::now(),
    };
    let mut events = Vec::new();
    if include_upsert {
        events.push(routine_step_session_upsert_event(params));
    }
    events.extend(session_runtime_events_from_turn_event(&context, event));
    harness.sessions().record_events_best_effort(
        params.step_run_id,
        SessionOwnerKind::Task,
        events,
    );
}

/// Mark a routine-step session terminal without blocking stream processing.
pub fn transition_routine_step_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    step_run_id: Uuid,
    status: SessionStatus,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    harness.sessions().record_events_best_effort(
        step_run_id,
        SessionOwnerKind::Task,
        vec![SessionRuntimeEvent::Transition(SessionTransition {
            session_id: step_run_id,
            worker_id: "harness".to_string(),
            phase: Some(ExecutionPhase::Finalizing),
            status,
        })],
    );
}

/// Persist a task checkpoint through the harness session runtime.
pub async fn update_task_checkpoint<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    task_id: Uuid,
    phase: ExecutionPhase,
    worktree: Option<WorktreeSnapshot>,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    if let Err(error) = harness
        .sessions()
        .update_checkpoint(SessionCheckpointUpdate {
            session_id: task_id,
            phase,
            worktree,
            active_tool_name: None,
        })
        .await
    {
        warn!(error = %error, task_id = %task_id, "Failed to update task checkpoint through session runtime");
    }
}

/// Persist a task pause, resume, cancellation, or terminal transition.
pub async fn transition_task_session<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    worker_id: &str,
    task_id: Uuid,
    phase: Option<ExecutionPhase>,
    status: SessionStatus,
) where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let _ = harness
        .sessions()
        .transition(SessionTransition {
            session_id: task_id,
            worker_id: worker_id.to_string(),
            phase,
            status,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use nenjo_sessions::SessionRuntimeEvent;
    use uuid::Uuid;

    use super::{RoutineStepSessionRecord, routine_step_session_upsert_event};

    #[test]
    fn routine_step_session_uses_step_run_id_with_parent_task_metadata() {
        let parent_task_id = Uuid::new_v4();
        let step_run_id = Uuid::new_v4();
        let execution_run_id = Uuid::new_v4();
        let event = routine_step_session_upsert_event(&RoutineStepSessionRecord {
            parent_task_id,
            step_run_id,
            step_slug: "agent_step",
            step_name: "Agent Step",
            project_slug: "demo",
            routine_slug: Some("daily_routine"),
            execution_run_id,
            agent_slug: Some("nenji"),
            agent_name: Some("Nenji"),
            memory_namespace: Some("demo-memory"),
        });

        let SessionRuntimeEvent::SessionUpsert(upsert) = event else {
            panic!("expected routine step session upsert");
        };
        assert_eq!(upsert.session_id, step_run_id);
        assert_eq!(upsert.task_id, Some(parent_task_id));
        assert_eq!(upsert.parent_session_id, Some(parent_task_id));
        assert_eq!(upsert.execution_run_id, Some(execution_run_id));
        assert_eq!(upsert.agent.as_deref(), Some("nenji"));
        assert_eq!(upsert.routine.as_deref(), Some("daily_routine"));
        assert_eq!(
            upsert.metadata["parent_task_id"],
            parent_task_id.to_string()
        );
        assert_eq!(upsert.metadata["step_run_id"], step_run_id.to_string());
        assert_eq!(upsert.metadata["step_slug"], "agent_step");
    }
}
