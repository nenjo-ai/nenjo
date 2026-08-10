//! Pure request preparation and resumable contracts for human-review execution.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Slug;
use crate::routines::human_review::HumanDecision;
use crate::routines::human_review::{
    ApprovalOptionSnapshot, HumanRequestId, HumanReviewError, HumanReviewInput, HumanStepSpec,
};
use crate::routines::{RoutineHandoff, RoutineMetrics, StepResult};

/// Current encrypted routine-checkpoint contract.
pub const ROUTINE_CHECKPOINT_CONTRACT: &str = "nenjo.routine-checkpoint.v1";

/// Request draft built from validated incoming edge handoffs.
#[derive(Debug, Clone)]
pub struct ValidatedHumanRequestDraft {
    spec: HumanStepSpec,
    inputs: Vec<HumanReviewInput>,
}

impl ValidatedHumanRequestDraft {
    /// Construct a draft after the scheduler validates every edge handoff.
    pub fn new(spec: HumanStepSpec, inputs: Vec<HumanReviewInput>) -> Self {
        Self { spec, inputs }
    }

    /// Borrow the validated request specification.
    pub fn spec(&self) -> &HumanStepSpec {
        &self.spec
    }

    /// Borrow the activated review inputs.
    pub fn inputs(&self) -> &[HumanReviewInput] {
        &self.inputs
    }

    /// Consume the draft into its validated components.
    pub fn into_parts(self) -> (HumanStepSpec, Vec<HumanReviewInput>) {
        (self.spec, self.inputs)
    }

    /// Render and snapshot this validated draft into an immutable request.
    pub fn prepare(
        self,
        context: &HumanMaterializationContext,
    ) -> Result<MaterializedHumanRequest, HumanReviewError> {
        let (spec, inputs) = self.into_parts();
        let option_snapshot = spec.snapshot_options(&inputs)?;
        Ok(MaterializedHumanRequest {
            request_id: stable_request_id(context),
            title: render_title(&spec.title_template, &context.task_title),
            inputs,
            option_snapshot,
        })
    }
}

/// Stable execution identity supplied during request preparation.
#[derive(Debug, Clone)]
pub struct HumanMaterializationContext {
    /// Current execution identity.
    pub execution_run_id: Uuid,
    /// Human step being opened.
    pub step_slug: Slug,
    /// One-based visit count for this human step.
    pub request_round: u32,
    /// Task title available to the package title template.
    pub task_title: String,
}

/// Materialized request safe to send to the platform.
#[derive(Debug, Clone)]
pub struct MaterializedHumanRequest {
    /// Deterministic request identity for this execution, step, and round.
    pub request_id: HumanRequestId,
    /// Rendered reviewer-facing title.
    pub title: String,
    /// Ordered activated incoming edge inputs.
    pub inputs: Vec<HumanReviewInput>,
    /// Immutable dynamic approval options.
    pub option_snapshot: ApprovalOptionSnapshot,
}

fn stable_request_id(context: &HumanMaterializationContext) -> HumanRequestId {
    HumanRequestId::new(Uuid::new_v5(
        &context.execution_run_id,
        format!(
            "human-request:{}:{}",
            context.step_slug, context.request_round
        )
        .as_bytes(),
    ))
}

fn render_title(template: &str, task_title: &str) -> String {
    template
        .replace("{{ task.title }}", task_title)
        .trim()
        .to_string()
}

/// Complete encrypted checkpoint plaintext owned by the execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineCheckpoint {
    /// Versioned plaintext shape; the host encrypts the serialized value.
    pub contract_version: String,
    /// Platform execution run identity.
    pub execution_run_id: Uuid,
    /// Owning task identity.
    pub task_id: Uuid,
    /// Package slug used to load the routine on a compatible worker.
    pub routine_slug: Slug,
    /// Digest that prevents restoring against a changed graph.
    pub graph_revision: String,
    /// Original routine input required to restore provider execution after a
    /// worker restart. The platform stores the whole checkpoint encrypted.
    pub input: crate::routines::RoutineInput,
    /// Latest committed result for each completed step.
    pub step_results: HashMap<Slug, StepResult>,
    /// Stable edge identities already traversed during this execution.
    pub traversed_edges: HashSet<String>,
    /// Retry attempt counts keyed by step identity.
    pub retry_counts: HashMap<String, u32>,
    /// Traversal counts used to enforce bounded human-mediated cycles.
    pub traversal_counts: HashMap<String, u32>,
    /// Steps eligible to start.
    pub ready: Vec<Slug>,
    /// Steps currently executing.
    pub running: Vec<Slug>,
    /// Steps completed in the current execution state.
    pub completed: Vec<Slug>,
    /// Human steps with an open request.
    pub waiting: Vec<Slug>,
    /// Validated edge handoffs grouped by target step.
    pub handoffs: HashMap<Slug, Vec<RoutineHandoff>>,
    /// Latest request round opened for each human step.
    pub human_rounds: HashMap<Slug, u32>,
    /// Request identities that still require a resolution.
    pub pending_requests: Vec<HumanRequestId>,
    /// Unpublished or replayable drafts retained inside the encrypted
    /// checkpoint until their request is resolved.
    #[serde(default)]
    pub pending_drafts: HashMap<HumanRequestId, PendingHumanRequestDraft>,
    /// Accumulated routine execution metrics.
    pub metrics: RoutineMetrics,
    /// Highest resolution revision consumed for each request.
    pub consumed_resolutions: HashMap<HumanRequestId, u64>,
}

/// Non-terminal routine outcome used by resumable execution hosts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoutineExecutionOutcome {
    /// A terminal routine step completed.
    Completed(StepResult),
    /// No runnable work remains while human requests are pending.
    Suspended {
        checkpoint: Box<RoutineCheckpoint>,
        pending_requests: Vec<HumanRequestId>,
        /// Validated drafts that the host must materialize and durably open
        /// before advertising the suspension.
        drafts: Vec<PendingHumanRequestDraft>,
    },
    /// Execution failed before a valid terminal result or suspension.
    Failed(RoutineFailure),
}

/// One immutable review round prepared by the scheduler for host
/// materialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingHumanRequestDraft {
    /// Deterministic request identity.
    pub request_id: HumanRequestId,
    /// Human step that owns the request.
    pub step_slug: Slug,
    /// One-based visit number for this human step.
    pub round: u32,
    /// Validated human-step request contract.
    pub spec: HumanStepSpec,
    /// Ordered, validated incoming edge handoffs to review.
    pub inputs: Vec<HumanReviewInput>,
}

/// A platform resolution supplied while restoring a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedHumanRequest {
    /// Request being resolved.
    pub request_id: HumanRequestId,
    /// Monotonic revision committed by the platform.
    pub resolution_revision: u64,
    /// Validated reviewer decision.
    pub decision: HumanDecision,
    /// RFC 3339 platform resolution timestamp.
    pub resolved_at: String,
}

/// Serializable typed routine failure for durable execution history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineFailure {
    /// Stable machine-readable failure code.
    pub code: String,
    /// Reviewer- and operator-readable failure summary.
    pub summary: String,
    /// Step associated with the failure, when known.
    pub step_slug: Option<Slug>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn preparation_renders_title_and_snapshots_multiple_inputs() {
        let spec = HumanStepSpec::parse(json!({
            "title": "Review {{ task.title }}",
            "approval": {"fields": [{
                "id": "selected",
                "label": "Select components",
                "type": "multi_select",
                "required": true,
                "options": {"type": "inputs", "inputs": [
                    {"input": "api", "pointer": "/components", "value": "/id", "label": "/name"},
                    {"input": "web", "pointer": "/components", "value": "/id", "label": "/name"}
                ]}
            }]}
        }))
        .unwrap();
        let schema = json!({"type": "object"});
        let inputs = vec![
            HumanReviewInput {
                input: "api".into(),
                source_name: "API".into(),
                purpose: None,
                schema: schema.clone(),
                value: json!({"components": [{"id": "server", "name": "Server"}]}),
            },
            HumanReviewInput {
                input: "web".into(),
                source_name: "Web".into(),
                purpose: None,
                schema,
                value: json!({"components": [{"id": "dashboard", "name": "Dashboard"}]}),
            },
        ];
        let execution_run_id = Uuid::new_v4();
        let context = HumanMaterializationContext {
            execution_run_id,
            step_slug: Slug::derive("review"),
            request_round: 1,
            task_title: "Release".into(),
        };

        let first = ValidatedHumanRequestDraft::new(spec.clone(), inputs.clone())
            .prepare(&context)
            .unwrap();
        let replay = ValidatedHumanRequestDraft::new(spec, inputs)
            .prepare(&context)
            .unwrap();

        assert_eq!(first.title, "Review Release");
        assert_eq!(first.inputs.len(), 2);
        assert_eq!(first.option_snapshot.fields["selected"].len(), 2);
        assert_eq!(first.request_id, replay.request_id);
    }
}
