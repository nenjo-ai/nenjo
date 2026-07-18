//! Compact execution-run documents returned by task tools.
//!
//! The platform execution API also serves dashboard projections, so its wire
//! records contain database identities, counts, and a durable command snapshot.
//! Agent tools expose only the stable task concepts needed to inspect and watch
//! work without leaking that internal representation.

use anyhow::{Context, Result, bail};
use nenjo::Slug;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::responses::TaskTarget;

/// Canonical lifecycle state of a task execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRunStatus {
    Pending,
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl ExecutionRunStatus {
    /// Return whether no further worker lifecycle transition is expected.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Origin of a task execution run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionRunTrigger {
    Manual,
    Retry,
    Schedule { scheduled_for: String },
}

/// Compact task execution metadata returned by list and watch tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRunSummary {
    pub id: Uuid,
    pub task_slug: Slug,
    pub status: ExecutionRunStatus,
    pub trigger: ExecutionRunTrigger,
    pub target: TaskTarget,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result returned by `list_task_execution_runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRunsListResult {
    pub execution_runs: Vec<ExecutionRunSummary>,
}

/// Compact result returned by dispatch, retry, and cancellation commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRunMutationResult {
    pub execution_run_id: Uuid,
    pub status: ExecutionRunStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PlatformExecutionRunRecord {
    id: Uuid,
    task_slug: Option<Slug>,
    status: ExecutionRunStatus,
    trigger: Option<PlatformExecutionRunTrigger>,
    agent_slug: Option<Slug>,
    routine_slug: Option<Slug>,
    scheduled_for: Option<String>,
    created_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    cancelled_at: Option<String>,
    error_code: Option<String>,
    error_summary: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlatformExecutionRunTrigger {
    Manual,
    Schedule,
    Retry,
}

impl PlatformExecutionRunRecord {
    fn into_summary(self) -> Result<ExecutionRunSummary> {
        let task_slug = self
            .task_slug
            .context("platform execution response did not include task_slug")?;
        let trigger = match self
            .trigger
            .context("platform execution response did not include trigger")?
        {
            PlatformExecutionRunTrigger::Manual => ExecutionRunTrigger::Manual,
            PlatformExecutionRunTrigger::Retry => ExecutionRunTrigger::Retry,
            PlatformExecutionRunTrigger::Schedule => ExecutionRunTrigger::Schedule {
                scheduled_for: self.scheduled_for.context(
                    "scheduled platform execution response did not include scheduled_for",
                )?,
            },
        };
        let target = match (self.agent_slug, self.routine_slug) {
            (Some(slug), None) => TaskTarget::Agent { slug },
            (None, Some(slug)) => TaskTarget::Routine { slug },
            (None, None) => bail!("platform execution response did not include a target slug"),
            (Some(_), Some(_)) => bail!("platform execution response included multiple targets"),
        };
        let error = self.error_summary.or(self.error_code);
        Ok(ExecutionRunSummary {
            id: self.id,
            task_slug,
            status: self.status,
            trigger,
            target,
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.completed_at.or(self.cancelled_at),
            error,
        })
    }
}

/// Convert one dashboard-oriented platform record to the agent contract.
#[cfg(test)]
pub(crate) fn execution_run_summary(run: Value) -> Result<ExecutionRunSummary> {
    serde_json::from_value::<PlatformExecutionRunRecord>(run)
        .context("execution response did not match the platform execution contract")?
        .into_summary()
}

/// Convert a platform execution list to compact agent summaries.
pub(crate) fn execution_run_summaries(runs: Value) -> Result<Vec<ExecutionRunSummary>> {
    let records: Vec<PlatformExecutionRunRecord> = serde_json::from_value(runs)
        .context("execution list response did not match the platform execution contract")?;
    records
        .into_iter()
        .map(PlatformExecutionRunRecord::into_summary)
        .collect()
}

/// Convert a task execution command response without retaining its internals.
pub(crate) fn execution_mutation_result(run: Value) -> Result<ExecutionRunMutationResult> {
    #[derive(Deserialize)]
    struct MutationRecord {
        id: Uuid,
        status: ExecutionRunStatus,
    }

    let run: MutationRecord = serde_json::from_value(run)
        .context("execution command response did not match the platform execution contract")?;
    Ok(ExecutionRunMutationResult {
        execution_run_id: run.id,
        status: run.status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn execution_summary_omits_platform_internals() {
        let run_id = Uuid::new_v4();
        let summary = execution_run_summary(json!({
            "id": run_id,
            "org_id": Uuid::new_v4(),
            "project_id": Uuid::new_v4(),
            "task_id": Uuid::new_v4(),
            "task_slug": "implement-loader-b1c3a204",
            "task_title": "Implement loader",
            "status": "running",
            "trigger": "manual",
            "agent_slug": null,
            "routine_slug": "code-generation-pipeline",
            "scheduled_for": null,
            "created_at": "2026-07-18T01:00:33Z",
            "updated_at": "2026-07-18T01:00:40Z",
            "started_at": "2026-07-18T01:00:34Z",
            "completed_at": null,
            "cancelled_at": null,
            "error_code": null,
            "error_summary": null,
            "config": {"task_command": {"encrypted_payload": {"ciphertext": "secret"}}},
            "task_counts": {"running": 1, "total": 1},
            "completion_percentage": 0
        }))
        .unwrap();

        assert_eq!(
            serde_json::to_value(summary).unwrap(),
            json!({
                "id": run_id,
                "task_slug": "implement-loader-b1c3a204",
                "status": "running",
                "trigger": {"type": "manual"},
                "target": {"type": "routine", "slug": "code-generation-pipeline"},
                "created_at": "2026-07-18T01:00:33Z",
                "started_at": "2026-07-18T01:00:34Z"
            })
        );
    }

    #[test]
    fn scheduled_summary_requires_and_groups_its_occurrence_time() {
        let summary = execution_run_summary(json!({
            "id": Uuid::new_v4(),
            "task_slug": "daily-report-a1b2c3d4",
            "status": "queued",
            "trigger": "schedule",
            "agent_slug": "reporter",
            "routine_slug": null,
            "scheduled_for": "2026-07-18T09:00:00Z",
            "created_at": "2026-07-18T09:00:00Z",
            "started_at": null,
            "completed_at": null,
            "cancelled_at": null,
            "error_code": null,
            "error_summary": null
        }))
        .unwrap();

        assert_eq!(
            serde_json::to_value(summary).unwrap()["trigger"],
            json!({"type": "schedule", "scheduled_for": "2026-07-18T09:00:00Z"})
        );
    }

    #[test]
    fn mutation_result_keeps_only_the_handle_and_state() {
        let run_id = Uuid::new_v4();
        let result = execution_mutation_result(json!({
            "id": run_id,
            "org_id": Uuid::new_v4(),
            "task_id": Uuid::new_v4(),
            "status": "queued",
            "config": {"private": true}
        }))
        .unwrap();

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({"execution_run_id": run_id, "status": "queued"})
        );
    }
}
