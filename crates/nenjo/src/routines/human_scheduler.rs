//! Durable graph-state transitions for routines that wait for human review.
//!
//! Provider execution and host I/O intentionally remain outside this module.
//! Persisting the returned checkpoint lets a worker release its live model
//! turn and later restore without replaying completed side effects.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use uuid::Uuid;

use crate::Slug;
use crate::manifest::{
    RoutineEdgeCondition, RoutineManifest, RoutineStepManifest, RoutineStepType,
};
use crate::routines::handoff_schema::{validate_handoff_payload, validate_handoff_schema};
use crate::routines::human_materialization::{
    PendingHumanRequestDraft, ROUTINE_CHECKPOINT_CONTRACT, ResolvedHumanRequest, RoutineCheckpoint,
    RoutineExecutionOutcome, RoutineFailure,
};
use crate::routines::human_review::{
    HumanRequestId, HumanReviewInput, HumanReviewOutcome, HumanStepSpec,
};
use crate::routines::{RoutineHandoff, RoutineMetrics, StepResult};

/// Identity pinned into every durable scheduler checkpoint.
#[derive(Debug, Clone)]
pub struct RoutineCheckpointIdentity {
    /// Platform execution run being resumed.
    pub execution_run_id: Uuid,
    /// Owning task/session identity.
    pub task_id: Uuid,
    /// Package slug needed to reload the routine on a replacement worker.
    pub routine_slug: Slug,
    /// Digest of the normalized routine graph.
    pub graph_revision: String,
    #[doc(hidden)]
    pub input: Option<crate::routines::RoutineInput>,
}

impl RoutineCheckpointIdentity {
    /// Create externally supplied durable identities. The runner binds the
    /// task input immediately before execution.
    pub fn new(
        execution_run_id: Uuid,
        task_id: Uuid,
        routine_slug: Slug,
        graph_revision: impl Into<String>,
    ) -> Self {
        Self {
            execution_run_id,
            task_id,
            routine_slug,
            graph_revision: graph_revision.into(),
            input: None,
        }
    }
}

/// Pure, restart-safe scheduler for human-capable routine graphs.
pub struct HumanReviewScheduler<'a> {
    routine: &'a RoutineManifest,
    checkpoint: RoutineCheckpoint,
    drafts: HashMap<HumanRequestId, PendingHumanRequestDraft>,
}

impl<'a> HumanReviewScheduler<'a> {
    /// Create a scheduler at the graph entry steps.
    pub fn new(routine: &'a RoutineManifest, identity: RoutineCheckpointIdentity) -> Self {
        Self::new_with_config(
            routine,
            identity,
            crate::routines::RoutineExecutionConfig::default(),
        )
    }

    pub fn new_with_config(
        routine: &'a RoutineManifest,
        identity: RoutineCheckpointIdentity,
        retry_policy: crate::routines::RoutineExecutionConfig,
    ) -> Self {
        Self {
            routine,
            checkpoint: RoutineCheckpoint {
                contract_version: ROUTINE_CHECKPOINT_CONTRACT.to_string(),
                execution_run_id: identity.execution_run_id,
                task_id: identity.task_id,
                routine_slug: identity.routine_slug,
                graph_revision: identity.graph_revision,
                input: identity
                    .input
                    .expect("routine runner supplies checkpoint input"),
                retry_policy,
                step_results: HashMap::new(),
                traversed_edges: HashSet::new(),
                retry_counts: HashMap::new(),
                traversal_counts: HashMap::new(),
                ready: routine.entry_steps.clone(),
                running: Vec::new(),
                completed: Vec::new(),
                waiting: Vec::new(),
                handoffs: HashMap::new(),
                human_rounds: HashMap::new(),
                pending_requests: Vec::new(),
                pending_drafts: HashMap::new(),
                metrics: RoutineMetrics::new(),
                consumed_resolutions: HashMap::new(),
            },
            drafts: HashMap::new(),
        }
    }

    /// Restore state after validating execution and graph identity.
    pub fn restore(
        routine: &'a RoutineManifest,
        checkpoint: RoutineCheckpoint,
        identity: &RoutineCheckpointIdentity,
    ) -> Result<Self> {
        if checkpoint.contract_version != ROUTINE_CHECKPOINT_CONTRACT
            || checkpoint.execution_run_id != identity.execution_run_id
            || checkpoint.task_id != identity.task_id
            || checkpoint.routine_slug != identity.routine_slug
            || checkpoint.graph_revision != identity.graph_revision
        {
            bail!("routine checkpoint identity or contract does not match the execution");
        }
        validate_checkpoint_state(routine, &checkpoint)?;
        let drafts = checkpoint.pending_drafts.clone();
        Ok(Self {
            routine,
            checkpoint,
            drafts,
        })
    }

    /// Return runnable step slugs. Human steps remain here until opened.
    pub fn ready_steps(&self) -> Vec<Slug> {
        self.checkpoint
            .ready
            .iter()
            .filter(|slug| self.target_inputs_ready(slug))
            .cloned()
            .collect()
    }

    /// Return whether every concurrently active producer for a ready human
    /// step has finished and at least one current-round handoff is available.
    pub fn human_inputs_ready(&self, step_slug: &Slug) -> bool {
        let handoff_count = self.checkpoint.handoffs.get(step_slug).map_or(0, Vec::len);
        handoff_count > 0
            && !self.routine.edges.iter().any(|edge| {
                edge.target_step == *step_slug
                    && (self.checkpoint.ready.contains(&edge.source_step)
                        || self.checkpoint.running.contains(&edge.source_step))
            })
    }

    /// Mark an automatic step running and remove it from the ready set.
    pub fn start_step(&mut self, step_slug: &Slug) -> Result<()> {
        let step = self.require_step(step_slug)?;
        if step.step_type == RoutineStepType::Human {
            bail!("human step '{step_slug}' must be opened, not started automatically");
        }
        if !self.checkpoint.ready.contains(step_slug) || !self.target_inputs_ready(step_slug) {
            bail!("step '{step_slug}' is not ready to start");
        }
        remove_value(&mut self.checkpoint.ready, step_slug);
        push_unique(&mut self.checkpoint.running, step_slug.clone());
        Ok(())
    }

    /// Commit an automatic step result and activate matching outgoing edges.
    pub fn complete_step(
        &mut self,
        step_slug: &Slug,
        result: StepResult,
        handoffs: Vec<RoutineHandoff>,
    ) -> Result<()> {
        let step_type = self.require_step(step_slug)?.step_type;
        if step_type == RoutineStepType::Human {
            bail!("human steps complete only through apply_resolution");
        }
        if !self.checkpoint.running.contains(step_slug) {
            bail!("step '{step_slug}' is not running");
        }
        remove_value(&mut self.checkpoint.running, step_slug);
        push_unique(&mut self.checkpoint.completed, step_slug.clone());
        self.checkpoint
            .step_results
            .insert(step_slug.clone(), result.clone());
        for handoff in handoffs {
            if handoff.source_step != *step_slug {
                bail!("handoff source does not match completed step");
            }
            self.checkpoint
                .handoffs
                .entry(handoff.target_step.clone())
                .or_default()
                .push(handoff);
        }
        let conditions = if step_type == RoutineStepType::Gate {
            vec![if result.passed {
                RoutineEdgeCondition::OnPass
            } else {
                RoutineEdgeCondition::OnFail
            }]
        } else if result.passed {
            vec![RoutineEdgeCondition::Always, RoutineEdgeCondition::OnPass]
        } else {
            vec![RoutineEdgeCondition::OnFail]
        };
        self.activate_edges(step_slug, &conditions);
        Ok(())
    }

    /// Create the immutable request draft for one ready human-step visit.
    pub fn open_human(&mut self, step_slug: &Slug) -> Result<PendingHumanRequestDraft> {
        let step = self.require_step(step_slug)?.clone();
        if step.step_type != RoutineStepType::Human {
            bail!("step '{step_slug}' is not human");
        }
        if !self.checkpoint.ready.contains(step_slug) {
            bail!("human step '{step_slug}' is not ready");
        }
        let spec = HumanStepSpec::parse(
            step.config
                .get("request")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("human step '{step_slug}' has no request"))?,
        )?;
        if !self.human_inputs_ready(step_slug) {
            bail!("human step '{step_slug}' is still waiting for incoming review inputs");
        }
        let handoffs = self
            .checkpoint
            .handoffs
            .get(step_slug)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut seen_inputs = HashSet::new();
        let inputs = handoffs
            .iter()
            .map(|handoff| {
                let edge = self
                    .routine
                    .edges
                    .iter()
                    .find(|edge| {
                        edge.source_step == handoff.source_step
                            && edge.target_step == handoff.target_step
                            && edge.condition == handoff.edge_condition
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("human handoff does not match a routine edge")
                    })?;
                let input = handoff.source_step.to_string();
                if !seen_inputs.insert(input.clone()) {
                    bail!(
                        "human step '{step_slug}' received duplicate input '{input}' in one round"
                    );
                }
                let schema = edge
                    .handoff_schema
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("handoff_schema is required"))?;
                validate_handoff_schema(schema)?;
                let schema = schema.clone();
                validate_handoff_payload(&schema, &handoff.handoff)?;
                let source_name = self.require_step(&handoff.source_step)?.name.clone();
                Ok(HumanReviewInput {
                    input,
                    source_name,
                    purpose: handoff.purpose.clone(),
                    schema,
                    value: handoff.handoff.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        // The immutable draft owns this activation. Clearing consumed input
        // ensures a changes-requested revisit starts from exactly one new
        // upstream handoff rather than accumulating prior rounds.
        self.checkpoint.handoffs.remove(step_slug);
        let round = self
            .checkpoint
            .human_rounds
            .get(step_slug)
            .copied()
            .unwrap_or(0)
            + 1;
        self.checkpoint
            .human_rounds
            .insert(step_slug.clone(), round);
        let request_id = stable_request_id(self.checkpoint.execution_run_id, step_slug, round);
        let draft = PendingHumanRequestDraft {
            request_id,
            step_slug: step_slug.clone(),
            round,
            spec,
            inputs,
        };
        remove_value(&mut self.checkpoint.ready, step_slug);
        push_unique(&mut self.checkpoint.waiting, step_slug.clone());
        push_unique(&mut self.checkpoint.pending_requests, request_id);
        self.checkpoint
            .pending_drafts
            .insert(request_id, draft.clone());
        self.drafts.insert(request_id, draft.clone());
        Ok(draft)
    }

    /// Consume a platform resolution once and route every matching outcome edge.
    pub fn apply_resolution(&mut self, resolution: ResolvedHumanRequest) -> Result<StepResult> {
        if let Some(consumed) = self
            .checkpoint
            .consumed_resolutions
            .get(&resolution.request_id)
        {
            if *consumed == resolution.resolution_revision {
                let slug = self.step_slug_for_request(resolution.request_id)?;
                return self
                    .checkpoint
                    .step_results
                    .get(&slug)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!("consumed resolution is missing its human step result")
                    });
            }
            bail!("continuation revision conflicts with a consumed resolution");
        }
        if !self
            .checkpoint
            .pending_requests
            .contains(&resolution.request_id)
        {
            bail!("human request is not pending in this checkpoint");
        }
        let step_slug = self.step_slug_for_request(resolution.request_id)?;
        let step = self.require_step(&step_slug)?.clone();
        let round = self.checkpoint.human_rounds[&step.slug];
        let outcome = resolution.decision.outcome();
        let data = resolution_handoff(resolution.request_id, round, &resolution);
        let result = StepResult {
            passed: outcome == HumanReviewOutcome::Approved,
            output: outcome.as_str().to_string(),
            data: data.clone(),
            step_slug: step.slug.clone(),
            step_name: step.name.clone(),
            ..Default::default()
        };
        self.checkpoint
            .consumed_resolutions
            .insert(resolution.request_id, resolution.resolution_revision);
        remove_value(
            &mut self.checkpoint.pending_requests,
            &resolution.request_id,
        );
        self.checkpoint
            .pending_drafts
            .remove(&resolution.request_id);
        self.drafts.remove(&resolution.request_id);
        remove_value(&mut self.checkpoint.waiting, &step.slug);
        push_unique(&mut self.checkpoint.completed, step.slug.clone());
        self.checkpoint
            .step_results
            .insert(step.slug.clone(), result.clone());
        let condition = match outcome {
            HumanReviewOutcome::Approved => RoutineEdgeCondition::Approved,
            HumanReviewOutcome::ChangesRequested => RoutineEdgeCondition::ChangesRequested,
            HumanReviewOutcome::Rejected => RoutineEdgeCondition::Rejected,
        };
        for edge in self
            .routine
            .edges
            .iter()
            .filter(|edge| edge.source_step == step.slug && edge.condition == condition)
        {
            self.checkpoint
                .handoffs
                .entry(edge.target_step.clone())
                .or_default()
                .push(RoutineHandoff {
                    source_step: step.slug.clone(),
                    target_step: edge.target_step.clone(),
                    handoff: data.clone(),
                    purpose: None,
                    summary: None,
                    edge_condition: condition,
                });
        }
        self.activate_edges(&step.slug, &[condition]);
        Ok(result)
    }

    /// Snapshot the complete scheduler state.
    pub fn checkpoint(&self) -> RoutineCheckpoint {
        self.checkpoint.clone()
    }

    /// Replace accumulated provider metrics before checkpoint persistence.
    pub(crate) fn set_metrics(&mut self, metrics: RoutineMetrics) {
        self.checkpoint.metrics = metrics;
    }

    /// Derive a terminal or suspended outcome when no local work remains.
    pub fn outcome(&self) -> Option<RoutineExecutionOutcome> {
        if !self.checkpoint.ready.is_empty() || !self.checkpoint.running.is_empty() {
            return None;
        }
        if !self.checkpoint.pending_requests.is_empty() {
            return Some(RoutineExecutionOutcome::Suspended {
                checkpoint: Box::new(self.checkpoint()),
                pending_requests: self.checkpoint.pending_requests.clone(),
                drafts: self.drafts.values().cloned().collect(),
            });
        }
        let terminals = self
            .checkpoint
            .completed
            .iter()
            .filter_map(|slug| {
                let step = self.routine.steps.iter().find(|step| &step.slug == slug)?;
                matches!(
                    step.step_type,
                    RoutineStepType::Terminal | RoutineStepType::TerminalFail
                )
                .then(|| self.checkpoint.step_results.get(slug).cloned())
                .flatten()
            })
            .collect::<Vec<_>>();
        match terminals.as_slice() {
            [result] => Some(RoutineExecutionOutcome::Completed(result.clone())),
            [] => None,
            _ => Some(RoutineExecutionOutcome::Failed(RoutineFailure {
                code: "ambiguous_terminal_outcome".to_string(),
                summary: "Parallel routine branches completed more than one terminal step"
                    .to_string(),
                step_slug: None,
            })),
        }
    }

    fn target_inputs_ready(&self, target: &Slug) -> bool {
        self.target_inputs_ready_with(target, &[])
    }

    fn target_inputs_ready_with(&self, target: &Slug, newly_activated: &[Slug]) -> bool {
        self.routine
            .edges
            .iter()
            .filter(|edge| edge.target_step == *target)
            .all(|edge| {
                let Some(result) = self.checkpoint.step_results.get(&edge.source_step) else {
                    if self.checkpoint.ready.contains(&edge.source_step)
                        || self.checkpoint.running.contains(&edge.source_step)
                        || self.checkpoint.waiting.contains(&edge.source_step)
                        || newly_activated.contains(&edge.source_step)
                    {
                        return false;
                    }
                    return true;
                };
                if !edge_condition_matches_result(edge.condition, result) {
                    return true;
                }
                self.checkpoint.traversed_edges.contains(&edge_key(edge))
            })
    }

    fn target_has_activated_input(&self, target: &Slug) -> bool {
        self.routine
            .edges
            .iter()
            .filter(|edge| edge.target_step == *target)
            .any(|edge| {
                self.checkpoint
                    .step_results
                    .get(&edge.source_step)
                    .is_some_and(|result| edge_condition_matches_result(edge.condition, result))
                    && self.checkpoint.traversed_edges.contains(&edge_key(edge))
            })
    }

    fn require_step(&self, slug: &Slug) -> Result<&RoutineStepManifest> {
        self.routine
            .steps
            .iter()
            .find(|step| &step.slug == slug)
            .ok_or_else(|| anyhow::anyhow!("checkpoint references unknown step '{slug}'"))
    }

    fn step_slug_for_request(&self, request_id: HumanRequestId) -> Result<Slug> {
        self.checkpoint
            .human_rounds
            .iter()
            .find_map(|(slug, round)| {
                (stable_request_id(self.checkpoint.execution_run_id, slug, *round) == request_id)
                    .then(|| slug.clone())
            })
            .ok_or_else(|| anyhow::anyhow!("human request does not belong to this checkpoint"))
    }

    fn activate_edges(&mut self, source: &Slug, conditions: &[RoutineEdgeCondition]) {
        let edges = self
            .routine
            .edges
            .iter()
            .filter(|edge| edge.source_step == *source)
            .cloned()
            .collect::<Vec<_>>();
        let mut targets = Vec::new();
        let mut activated_targets = Vec::new();
        for edge in edges {
            if conditions.contains(&edge.condition) {
                let key = edge_key(&edge);
                self.checkpoint.traversed_edges.insert(key.clone());
                *self.checkpoint.traversal_counts.entry(key).or_default() += 1;
                push_unique(&mut activated_targets, edge.target_step.clone());
            }
            push_unique(&mut targets, edge.target_step);
        }
        for target in &targets {
            if self.target_has_activated_input(target)
                && self.target_inputs_ready_with(target, &activated_targets)
            {
                let target_is_active = self.checkpoint.ready.contains(target)
                    || self.checkpoint.running.contains(target)
                    || self.checkpoint.waiting.contains(target);
                if !target_is_active {
                    remove_value(&mut self.checkpoint.completed, target);
                    self.checkpoint.step_results.remove(target);
                }
                push_unique(&mut self.checkpoint.ready, target.clone());
            }
        }
    }
}

fn validate_checkpoint_state(
    routine: &RoutineManifest,
    checkpoint: &RoutineCheckpoint,
) -> Result<()> {
    let mut active_states = HashSet::new();
    for (state, slugs) in [
        ("ready", &checkpoint.ready),
        ("running", &checkpoint.running),
        ("completed", &checkpoint.completed),
        ("waiting", &checkpoint.waiting),
    ] {
        for slug in slugs {
            let step = routine
                .steps
                .iter()
                .find(|step| &step.slug == slug)
                .ok_or_else(|| {
                    anyhow::anyhow!("checkpoint {state} state references unknown step '{slug}'")
                })?;
            if !active_states.insert(slug.clone()) {
                bail!("checkpoint step '{slug}' appears in more than one execution state");
            }
            if state == "waiting" && step.step_type != RoutineStepType::Human {
                bail!("checkpoint waiting state references non-human step '{slug}'");
            }
        }
    }
    for slug in checkpoint.step_results.keys() {
        if !checkpoint.completed.contains(slug) {
            bail!("checkpoint result for '{slug}' is not in completed state");
        }
    }
    let pending = checkpoint
        .pending_requests
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    if pending.len() != checkpoint.pending_requests.len() {
        bail!("checkpoint contains duplicate pending human requests");
    }
    if pending.len() != checkpoint.waiting.len()
        || checkpoint.waiting.iter().any(|step_slug| {
            checkpoint.human_rounds.get(step_slug).is_none_or(|round| {
                !pending.contains(&stable_request_id(
                    checkpoint.execution_run_id,
                    step_slug,
                    *round,
                ))
            })
        })
    {
        bail!("checkpoint waiting steps do not match pending human requests");
    }
    for (request_id, draft) in &checkpoint.pending_drafts {
        if !pending.contains(request_id)
            || !checkpoint.waiting.contains(&draft.step_slug)
            || stable_request_id(checkpoint.execution_run_id, &draft.step_slug, draft.round)
                != *request_id
        {
            bail!("checkpoint contains an invalid pending human-request draft");
        }
    }
    if checkpoint
        .consumed_resolutions
        .keys()
        .any(|request_id| pending.contains(request_id))
    {
        bail!("checkpoint marks one human resolution both pending and consumed");
    }
    Ok(())
}

fn edge_key(edge: &crate::manifest::RoutineEdgeManifest) -> String {
    let condition = match edge.condition {
        RoutineEdgeCondition::Always => "always",
        RoutineEdgeCondition::OnPass => "on_pass",
        RoutineEdgeCondition::OnFail => "on_fail",
        RoutineEdgeCondition::Approved => "approved",
        RoutineEdgeCondition::ChangesRequested => "changes_requested",
        RoutineEdgeCondition::Rejected => "rejected",
    };
    format!("{}:{condition}:{}", edge.source_step, edge.target_step)
}

fn edge_condition_matches_result(condition: RoutineEdgeCondition, result: &StepResult) -> bool {
    match condition {
        RoutineEdgeCondition::Always => true,
        RoutineEdgeCondition::OnPass => result.passed,
        RoutineEdgeCondition::OnFail => !result.passed,
        RoutineEdgeCondition::Approved => result.output == HumanReviewOutcome::Approved.as_str(),
        RoutineEdgeCondition::ChangesRequested => {
            result.output == HumanReviewOutcome::ChangesRequested.as_str()
        }
        RoutineEdgeCondition::Rejected => result.output == HumanReviewOutcome::Rejected.as_str(),
    }
}

fn stable_request_id(run_id: Uuid, step_slug: &Slug, round: u32) -> HumanRequestId {
    HumanRequestId::new(Uuid::new_v5(
        &run_id,
        format!("human-request:{step_slug}:{round}").as_bytes(),
    ))
}

fn resolution_handoff(
    request_id: HumanRequestId,
    round: u32,
    resolution: &ResolvedHumanRequest,
) -> serde_json::Value {
    let mut value = serde_json::to_value(&resolution.decision).expect("decision serializes");
    let object = value.as_object_mut().expect("decision is a tagged object");
    object.insert("request_id".to_string(), serde_json::json!(request_id));
    object.insert("round".to_string(), serde_json::json!(round));
    object.insert(
        "resolved_at".to_string(),
        serde_json::json!(resolution.resolved_at),
    );
    value
}

fn remove_value<T: PartialEq>(values: &mut Vec<T>, target: &T) {
    values.retain(|value| value != target);
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::manifest::RoutineEdgeManifest;
    use crate::routines::human_materialization::ResolvedHumanRequest;

    fn step(
        slug: &str,
        step_type: RoutineStepType,
        config: serde_json::Value,
    ) -> RoutineStepManifest {
        RoutineStepManifest {
            slug: Slug::derive(slug),
            routine: Slug::derive("release"),
            name: slug.to_string(),
            step_type,
            council: None,
            agent: None,
            config,
            order_index: 0,
        }
    }

    fn edge(from: &str, to: &str, condition: RoutineEdgeCondition) -> RoutineEdgeManifest {
        let handoff_schema = if matches!(
            condition,
            RoutineEdgeCondition::Always
                | RoutineEdgeCondition::OnPass
                | RoutineEdgeCondition::OnFail
        ) {
            Some(json!({
                "type": "object",
                "required": ["summary"],
                "properties": {"summary": {"type": "string"}},
                "additionalProperties": false
            }))
        } else {
            None
        };
        RoutineEdgeManifest {
            routine: Slug::derive("release"),
            source_step: Slug::derive(from),
            target_step: Slug::derive(to),
            condition,
            purpose: None,
            handoff_instructions: None,
            handoff_schema,
            max_retries: None,
        }
    }

    fn routine() -> RoutineManifest {
        RoutineManifest {
            name: "Release".to_string(),
            slug: Slug::derive("release"),
            description: None,
            entry_steps: vec![Slug::derive("prepare")],
            steps: vec![
                step("prepare", RoutineStepType::Agent, json!({})),
                step(
                    "review",
                    RoutineStepType::Human,
                    json!({"request": {
                        "title": "Review {{ task.title }}"
                    }}),
                ),
                step("revise", RoutineStepType::Agent, json!({})),
                step("done", RoutineStepType::Terminal, json!({})),
                step("rejected", RoutineStepType::TerminalFail, json!({})),
            ],
            edges: vec![
                edge("prepare", "review", RoutineEdgeCondition::Always),
                edge("review", "done", RoutineEdgeCondition::Approved),
                edge("review", "revise", RoutineEdgeCondition::ChangesRequested),
                edge("review", "rejected", RoutineEdgeCondition::Rejected),
                edge("revise", "review", RoutineEdgeCondition::Always),
            ],
        }
    }

    fn identity(run: Uuid) -> RoutineCheckpointIdentity {
        RoutineCheckpointIdentity {
            execution_run_id: run,
            task_id: Uuid::new_v4(),
            routine_slug: Slug::derive("release"),
            graph_revision: "graph-v1".to_string(),
            input: Some(crate::routines::RoutineInput::new("Release", "Prepare")),
        }
    }

    fn result(slug: &str) -> StepResult {
        StepResult {
            passed: true,
            step_slug: Slug::derive(slug),
            step_name: slug.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn checkpoint_snapshots_retry_policy_for_resumed_execution() {
        let routine = routine();
        let identity = identity(Uuid::new_v4());
        let original = crate::routines::RoutineExecutionConfig::new(
            crate::routines::GateRetryLimit::new(5),
            crate::routines::GateRetryLimit::new(8),
        )
        .unwrap();
        let scheduler = HumanReviewScheduler::new_with_config(&routine, identity.clone(), original);
        let checkpoint = scheduler.checkpoint();

        // A caller may now have a different worker configuration, but restore
        // consumes only the policy persisted with this run.
        let restored = HumanReviewScheduler::restore(&routine, checkpoint, &identity).unwrap();
        assert_eq!(restored.checkpoint().retry_policy, original);
    }

    #[test]
    fn suspends_restores_consumes_once_and_creates_a_new_changes_round() {
        let routine = routine();
        let run = Uuid::new_v4();
        let identity = identity(run);
        let mut scheduler = HumanReviewScheduler::new(&routine, identity.clone());
        scheduler.start_step(&Slug::derive("prepare")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("prepare"),
                result("prepare"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("prepare"),
                    target_step: Slug::derive("review"),
                    handoff: json!({"summary": "Ready"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        let first = scheduler.open_human(&Slug::derive("review")).unwrap();
        assert!(matches!(
            scheduler.outcome(),
            Some(RoutineExecutionOutcome::Suspended { .. })
        ));
        let mut platform_checkpoint = scheduler.checkpoint();
        platform_checkpoint.pending_drafts.clear();
        let mut platform_restored =
            HumanReviewScheduler::restore(&routine, platform_checkpoint, &identity).unwrap();
        assert!(matches!(
            platform_restored.outcome(),
            Some(RoutineExecutionOutcome::Suspended { drafts, .. }) if drafts.is_empty()
        ));
        platform_restored
            .apply_resolution(ResolvedHumanRequest {
                request_id: first.request_id,
                resolution_revision: 1,
                decision: serde_json::from_value(json!({"outcome": "approved"})).unwrap(),
                resolved_at: "2026-07-27T18:42:00Z".to_string(),
            })
            .expect("platform checkpoint without drafts remains resumable");

        let mut restored =
            HumanReviewScheduler::restore(&routine, scheduler.checkpoint(), &identity).unwrap();
        assert!(matches!(
            restored.outcome(),
            Some(RoutineExecutionOutcome::Suspended { drafts, .. }) if drafts.iter().any(|draft| draft.request_id == first.request_id)
        ));
        let resolution = ResolvedHumanRequest {
            request_id: first.request_id,
            resolution_revision: 2,
            decision: serde_json::from_value(json!({
                "outcome": "changes_requested",
                "instructions": "Add rollback"
            }))
            .unwrap(),
            resolved_at: "2026-07-27T18:42:00Z".to_string(),
        };
        let accepted = restored.apply_resolution(resolution.clone()).unwrap();
        assert_eq!(accepted.output, "changes_requested");
        assert!(restored.checkpoint().pending_drafts.is_empty());
        assert_eq!(
            restored.apply_resolution(resolution).unwrap().output,
            "changes_requested"
        );

        restored.start_step(&Slug::derive("revise")).unwrap();
        restored
            .complete_step(
                &Slug::derive("revise"),
                result("revise"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("revise"),
                    target_step: Slug::derive("review"),
                    handoff: json!({"summary": "Revised"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        let second = restored.open_human(&Slug::derive("review")).unwrap();
        assert_eq!(second.round, 2);
        assert_ne!(first.request_id, second.request_id);
        let checkpoint = restored.checkpoint();
        assert!(checkpoint.waiting.contains(&Slug::derive("review")));
        assert!(!checkpoint.completed.contains(&Slug::derive("review")));
        assert!(
            !checkpoint
                .step_results
                .contains_key(&Slug::derive("review"))
        );
    }

    #[test]
    fn waits_for_concurrent_producers_and_preserves_each_activated_input() {
        let mut routine = routine();
        routine.entry_steps.push(Slug::derive("audit"));
        routine
            .steps
            .push(step("audit", RoutineStepType::Agent, json!({})));
        routine
            .edges
            .push(edge("audit", "review", RoutineEdgeCondition::Always));
        let mut scheduler = HumanReviewScheduler::new(&routine, identity(Uuid::new_v4()));

        scheduler.start_step(&Slug::derive("prepare")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("prepare"),
                result("prepare"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("prepare"),
                    target_step: Slug::derive("review"),
                    handoff: json!({"summary": "Draft ready"}),
                    purpose: Some("Release draft".to_string()),
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        assert!(!scheduler.human_inputs_ready(&Slug::derive("review")));

        scheduler.start_step(&Slug::derive("audit")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("audit"),
                result("audit"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("audit"),
                    target_step: Slug::derive("review"),
                    handoff: json!({"summary": "Audit ready"}),
                    purpose: Some("Independent audit".to_string()),
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();

        let draft = scheduler.open_human(&Slug::derive("review")).unwrap();
        assert_eq!(draft.inputs.len(), 2);
        assert_eq!(draft.inputs[0].input, "prepare");
        assert_eq!(draft.inputs[1].input, "audit");
        assert_eq!(draft.inputs[1].value, json!({"summary": "Audit ready"}));
    }

    #[test]
    fn fans_out_approved_resolution_to_every_matching_target() {
        let mut routine = routine();
        routine
            .steps
            .push(step("deliver", RoutineStepType::Agent, json!({})));
        routine
            .steps
            .push(step("archive", RoutineStepType::Agent, json!({})));
        routine
            .edges
            .iter_mut()
            .find(|edge| {
                edge.source_step == Slug::derive("review")
                    && edge.condition == RoutineEdgeCondition::Approved
            })
            .expect("base approved edge")
            .target_step = Slug::derive("deliver");
        routine
            .edges
            .push(edge("review", "archive", RoutineEdgeCondition::Approved));
        routine
            .edges
            .push(edge("deliver", "done", RoutineEdgeCondition::Always));
        routine
            .edges
            .push(edge("archive", "done", RoutineEdgeCondition::Always));
        let mut scheduler = HumanReviewScheduler::new(&routine, identity(Uuid::new_v4()));

        scheduler.start_step(&Slug::derive("prepare")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("prepare"),
                result("prepare"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("prepare"),
                    target_step: Slug::derive("review"),
                    handoff: json!({"summary": "Ready"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        let request = scheduler.open_human(&Slug::derive("review")).unwrap();
        scheduler
            .apply_resolution(ResolvedHumanRequest {
                request_id: request.request_id,
                resolution_revision: 1,
                decision: serde_json::from_value(json!({"outcome": "approved"})).unwrap(),
                resolved_at: "2026-07-27T18:42:00Z".to_string(),
            })
            .unwrap();

        let checkpoint = scheduler.checkpoint();
        for target in ["deliver", "archive"] {
            let target = Slug::derive(target);
            assert!(checkpoint.ready.contains(&target));
            let handoffs = &checkpoint.handoffs[&target];
            assert_eq!(handoffs.len(), 1);
            assert_eq!(handoffs[0].edge_condition, RoutineEdgeCondition::Approved);
            assert_eq!(handoffs[0].handoff["outcome"], "approved");
        }
        assert!(!checkpoint.ready.contains(&Slug::derive("revise")));
        assert!(!checkpoint.ready.contains(&Slug::derive("rejected")));
    }

    #[test]
    fn approved_fan_out_waits_for_every_activated_join_input() {
        let mut routine = routine();
        routine.edges.retain(|edge| {
            !(edge.source_step == Slug::derive("review")
                && edge.condition == RoutineEdgeCondition::Approved)
        });
        routine
            .steps
            .push(step("left", RoutineStepType::Agent, json!({})));
        routine
            .steps
            .push(step("right", RoutineStepType::Agent, json!({})));
        routine
            .edges
            .push(edge("review", "left", RoutineEdgeCondition::Approved));
        routine
            .edges
            .push(edge("review", "right", RoutineEdgeCondition::Approved));
        routine
            .edges
            .push(edge("left", "done", RoutineEdgeCondition::Always));
        routine
            .edges
            .push(edge("right", "done", RoutineEdgeCondition::Always));
        let mut scheduler = HumanReviewScheduler::new(&routine, identity(Uuid::new_v4()));

        scheduler.start_step(&Slug::derive("prepare")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("prepare"),
                result("prepare"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("prepare"),
                    target_step: Slug::derive("review"),
                    handoff: json!({"summary": "Ready"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        let request = scheduler.open_human(&Slug::derive("review")).unwrap();
        scheduler
            .apply_resolution(ResolvedHumanRequest {
                request_id: request.request_id,
                resolution_revision: 1,
                decision: serde_json::from_value(json!({"outcome": "approved"})).unwrap(),
                resolved_at: "2026-07-27T18:42:00Z".to_string(),
            })
            .unwrap();

        scheduler.start_step(&Slug::derive("left")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("left"),
                result("left"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("left"),
                    target_step: Slug::derive("done"),
                    handoff: json!({"summary": "Left complete"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        assert!(!scheduler.checkpoint().ready.contains(&Slug::derive("done")));
        assert_eq!(scheduler.ready_steps(), vec![Slug::derive("right")]);

        scheduler.start_step(&Slug::derive("right")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("right"),
                result("right"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("right"),
                    target_step: Slug::derive("done"),
                    handoff: json!({"summary": "Right complete"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        assert_eq!(scheduler.ready_steps(), vec![Slug::derive("done")]);
    }

    #[test]
    fn join_is_reevaluated_when_an_optional_gate_branch_does_not_activate() {
        let routine = RoutineManifest {
            name: "Conditional join".to_string(),
            slug: Slug::derive("conditional_join"),
            description: None,
            entry_steps: vec![Slug::derive("prepare"), Slug::derive("verify")],
            steps: vec![
                step("prepare", RoutineStepType::Agent, json!({})),
                step("verify", RoutineStepType::Gate, json!({})),
                step("done", RoutineStepType::Terminal, json!({})),
                step("gate_passed", RoutineStepType::Terminal, json!({})),
            ],
            edges: vec![
                edge("prepare", "done", RoutineEdgeCondition::Always),
                edge("verify", "done", RoutineEdgeCondition::OnFail),
                edge("verify", "gate_passed", RoutineEdgeCondition::OnPass),
            ],
        };
        let mut checkpoint_identity = identity(Uuid::new_v4());
        checkpoint_identity.routine_slug = routine.slug.clone();
        let mut scheduler = HumanReviewScheduler::new(&routine, checkpoint_identity);

        scheduler.start_step(&Slug::derive("prepare")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("prepare"),
                result("prepare"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("prepare"),
                    target_step: Slug::derive("done"),
                    handoff: json!({"summary": "Prepared"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        assert!(!scheduler.checkpoint().ready.contains(&Slug::derive("done")));

        scheduler.start_step(&Slug::derive("verify")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("verify"),
                result("verify"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("verify"),
                    target_step: Slug::derive("gate_passed"),
                    handoff: json!({"summary": "Passed"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::OnPass,
                }],
            )
            .unwrap();

        let ready = scheduler.ready_steps();
        assert!(ready.contains(&Slug::derive("done")));
        assert!(ready.contains(&Slug::derive("gate_passed")));
    }

    #[test]
    fn restore_rejects_overlapping_checkpoint_step_states() {
        let routine = routine();
        let identity = identity(Uuid::new_v4());
        let mut scheduler = HumanReviewScheduler::new(&routine, identity.clone());
        scheduler.start_step(&Slug::derive("prepare")).unwrap();
        scheduler
            .complete_step(
                &Slug::derive("prepare"),
                result("prepare"),
                vec![RoutineHandoff {
                    source_step: Slug::derive("prepare"),
                    target_step: Slug::derive("review"),
                    handoff: json!({"summary": "Ready"}),
                    purpose: None,
                    summary: None,
                    edge_condition: RoutineEdgeCondition::Always,
                }],
            )
            .unwrap();
        scheduler.open_human(&Slug::derive("review")).unwrap();
        let mut checkpoint = scheduler.checkpoint();
        checkpoint.completed.push(Slug::derive("review"));

        assert!(
            HumanReviewScheduler::restore(&routine, checkpoint, &identity)
                .err()
                .expect("overlapping checkpoint state must fail")
                .to_string()
                .contains("more than one execution state")
        );
    }
}
