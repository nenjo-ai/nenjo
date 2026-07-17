//! Harness-side routine graph executor.
//!
//! Task execution dispatches routine runs to the harness. This
//! module owns runtime step execution, parallel entry waves, fan-out/fan-in
//! scheduling, gate retry loops, and event emission for a single routine run.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::AgentBuilder;
use crate::Slug;
use crate::input::{AgentRun, AgentRunKind, GateInput, ProjectLocation, TaskInput};
use crate::manifest::{
    ProjectManifest, RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest,
    RoutineStepManifest, RoutineStepType,
};
use crate::provider::ProviderRuntime;
use crate::routines::graph::validate_routine_manifest;
use crate::routines::types::StepResult;
use crate::routines::{apply_session_binding_memory_scope, routing};

use super::RoutineEvent;
use super::types::{RoutineHandoff, RoutineState};
use crate::context::{
    RoutineContext, RoutineHandoffContext, RoutineHandoffsContext, RoutineStepContext,
};

const DEFAULT_GATE_ON_FAIL_MAX_ATTEMPTS: u32 = 3;

/// Build `RoutineContext` and `RoutineStepContext` from the current execution state.
fn build_routine_ctx(
    state: &RoutineState,
    step: &RoutineStepManifest,
) -> (RoutineContext, RoutineStepContext) {
    let routine = RoutineContext {
        name: state.routine_name.clone().unwrap_or_default(),
        slug: state
            .routine_name
            .as_deref()
            .map(crate::Slug::derive)
            .map(|slug| slug.to_string())
            .unwrap_or_default(),
        execution_id: state
            .input
            .execution_run_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        description: None,
        step: Default::default(),
        handoffs: routine_handoffs_context(state, step),
    };
    let step = RoutineStepContext {
        name: state.current_step_name.clone().unwrap_or_default(),
        step_type: state.current_step_type.clone().unwrap_or_default(),
        instructions: state.step_instructions.clone().unwrap_or_default(),
        metadata: state.step_metadata.clone().unwrap_or_default(),
    };
    (routine, step)
}

fn routine_handoffs_context(
    state: &RoutineState,
    step: &RoutineStepManifest,
) -> RoutineHandoffsContext {
    let items = state
        .handoffs_for(&step.slug)
        .iter()
        .map(|handoff| RoutineHandoffContext {
            source_step: handoff.source_step.to_string(),
            target_step: handoff.target_step.to_string(),
            purpose: handoff.purpose.clone(),
            summary: handoff.summary.clone(),
            payload: serde_json::to_string_pretty(&handoff.handoff)
                .unwrap_or_else(|_| handoff.handoff.to_string()),
        })
        .collect();
    RoutineHandoffsContext { items }
}

fn scope_tools_to_work_dir<P>(mut builder: AgentBuilder<P>, state: &RoutineState) -> AgentBuilder<P>
where
    P: ProviderRuntime,
{
    if let Some(ref git) = state.input.git
        && !git.work_dir.is_empty()
    {
        builder = builder.with_work_dir(&git.work_dir);
    }
    builder
}

fn project_manifest_for_slug<P>(provider: &P, project: Option<&Slug>) -> Option<ProjectManifest>
where
    P: ProviderRuntime,
{
    provider.find_project(project?).cloned()
}

/// Execute a routine once for a task dispatch. Scheduled tasks use the same
/// one-shot execution path after the task runtime admits them.
pub(crate) async fn execute_routine<P>(
    provider: &P,
    routine: &RoutineManifest,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
    cancel: &CancellationToken,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    execute_routine_once(provider, routine, state, events_tx, cancel).await
}

/// Execute a routine once inside the harness runtime.
///
/// Multiple entry steps run in the same ready wave. A downstream step with
/// multiple activated incoming edges is scheduled only after all upstream steps
/// have completed successfully. Platform scheduling/dispatch does not happen
/// here.
pub(crate) async fn execute_routine_once<P>(
    provider: &P,
    routine: &RoutineManifest,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
    cancel: &CancellationToken,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let steps = &routine.steps;
    let edges = &routine.edges;

    if steps.is_empty() {
        anyhow::bail!("Routine '{}' has no steps", routine.name);
    }
    validate_routine_manifest(routine)
        .map_err(|error| anyhow::anyhow!("Routine graph is invalid: {error}"))?;

    let mut last_result = StepResult::default();
    let mut edge_traversals: HashMap<String, u32> = HashMap::new();
    let steps_by_slug: HashMap<_, _> = steps
        .iter()
        .map(|step| (step.slug.clone(), step.clone()))
        .collect();
    let mut outgoing_by_source: HashMap<_, Vec<_>> = HashMap::new();
    let mut incoming_by_target: HashMap<_, Vec<_>> = HashMap::new();
    for edge in edges {
        outgoing_by_source
            .entry(edge.source_step.clone())
            .or_insert_with(Vec::new)
            .push(edge.clone());
        incoming_by_target
            .entry(edge.target_step.clone())
            .or_insert_with(Vec::new)
            .push(edge.clone());
    }

    let mut ready: VecDeque<_> = routine.metadata.entry_steps.iter().cloned().collect();
    let mut scheduled: HashSet<_> = HashSet::new();
    let mut completed: HashSet<_> = HashSet::new();
    let mut traversed_edges: HashSet<String> = HashSet::new();
    let mut terminal_results = Vec::new();
    let max_waves = steps.len() * 100;

    for wave in 0..max_waves {
        if cancel.is_cancelled() {
            last_result = StepResult {
                passed: false,
                output: "Cancelled".to_string(),
                ..Default::default()
            };
            break;
        }

        let mut batch = Vec::new();
        while let Some(step_slug) = ready.pop_front() {
            if scheduled.contains(&step_slug) {
                continue;
            }
            let step = steps_by_slug
                .get(&step_slug)
                .with_context(|| format!("Ready step {step_slug} not found"))?
                .clone();
            let route_edges = outgoing_by_source
                .get(&step.slug)
                .cloned()
                .unwrap_or_default();
            scheduled.insert(step_slug);
            batch.push((step, route_edges));
        }

        if batch.is_empty() {
            break;
        }

        debug!(wave, steps = batch.len(), "Executing routine step wave");

        let mut tasks = tokio::task::JoinSet::new();
        for (step, route_edges) in batch {
            let provider = provider.clone();
            let events_tx = events_tx.clone();
            let mut step_state = state.clone();
            let routine_steps = steps.to_vec();
            let cancel = cancel.clone();
            tasks.spawn(async move {
                execute_scheduled_step(
                    &provider,
                    step,
                    &route_edges,
                    &routine_steps,
                    &mut step_state,
                    &events_tx,
                    &cancel,
                )
                .await
            });
        }

        let mut stop_after_wave = false;
        while let Some(joined) = tokio::select! {
            _ = cancel.cancelled() => {
                tasks.abort_all();
                None
            }
            joined = tasks.join_next() => joined,
        } {
            let execution = joined??;
            let step = execution.step;
            let step_result = execution.result;
            let step_status = execution.status;

            state.metrics.record_step(
                &step.slug,
                step_result.input_tokens,
                step_result.output_tokens,
            );

            state.record_step_result(step.slug.clone(), step_result.clone());
            completed.insert(step.slug.clone());
            scheduled.remove(&step.slug);
            last_result = step_result.clone();

            if step_status == StepExecutionStatus::ExecutionFailed {
                debug!(
                    step = %step.name,
                    "Routine step failed during execution, terminating routine"
                );
                stop_after_wave = true;
                continue;
            }

            if matches!(
                step.step_type,
                RoutineStepType::Terminal | RoutineStepType::TerminalFail
            ) {
                terminal_results.push(step_result);
                continue;
            }

            if step.step_type == RoutineStepType::Agent && !last_result.passed {
                debug!(
                    step = %step.name,
                    "Agent step returned fail verdict, terminating routine"
                );
                stop_after_wave = true;
                continue;
            }

            let outgoing = outgoing_by_source
                .get(&step.slug)
                .cloned()
                .unwrap_or_default();
            let activated = activated_edges(&step, &outgoing, last_result.passed)?;

            for edge in activated {
                if let Some(result) = exhausted_failure_for_edge(&step, edge, &mut edge_traversals)
                {
                    last_result = result;
                    stop_after_wave = true;
                    continue;
                }

                traversed_edges.insert(edge_key(edge));
                if let Some(handoff) = routine_handoff_for_edge(&step, &step_result, edge) {
                    state.record_handoff(handoff);
                }
                let target_slug = edge.target_step.clone();

                if scheduled.contains(&target_slug) {
                    continue;
                }
                if target_is_ready(
                    &target_slug,
                    &incoming_by_target,
                    &completed,
                    &traversed_edges,
                    state,
                    &steps_by_slug,
                )? {
                    ready.push_back(target_slug);
                }
            }
        }

        if cancel.is_cancelled() {
            last_result = StepResult {
                passed: false,
                output: "Cancelled".to_string(),
                ..Default::default()
            };
            break;
        }

        if stop_after_wave || !terminal_results.is_empty() {
            break;
        }
    }

    if let Some(result) = terminal_results.last() {
        last_result = result.clone();
    } else if last_result.passed && !cancel.is_cancelled() {
        last_result = StepResult {
            passed: false,
            output: "Routine ended before reaching a terminal step".to_string(),
            data: serde_json::json!({
                "reason": "no_terminal_reached",
            }),
            ..last_result
        };
    }

    let terminal_handoffs = if last_result.passed {
        terminal_results
            .iter()
            .filter(|result| result.passed)
            .flat_map(|result| state.handoffs_for(&result.step_slug).iter().cloned())
            .collect()
    } else {
        Vec::new()
    };

    let _ = events_tx.send(RoutineEvent::Done {
        task_id: state.input.task_id,
        result: last_result.clone(),
        handoffs: terminal_handoffs,
    });

    Ok(last_result)
}

struct StepExecution {
    step: RoutineStepManifest,
    result: StepResult,
    status: StepExecutionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepExecutionStatus {
    Completed,
    ExecutionFailed,
}

async fn execute_scheduled_step<P>(
    provider: &P,
    step: RoutineStepManifest,
    route_edges: &[RoutineEdgeManifest],
    routine_steps: &[RoutineStepManifest],
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
    cancel: &CancellationToken,
) -> Result<StepExecution>
where
    P: ProviderRuntime,
{
    debug!(
        step = %step.name,
        step_type = %step.step_type,
        "Executing routine step"
    );

    let step_run_id = state.step_run_id_for(&step.slug);
    prepare_step_state(state, &step);

    let _ = events_tx.send(RoutineEvent::StepStarted {
        step_slug: step.slug.clone(),
        step_run_id,
        step_name: step.name.clone(),
        step_type: step.step_type.to_string(),
    });

    let step_start = std::time::Instant::now();
    let result = execute_step(StepExecutionParams {
        provider,
        step: &step,
        route_edges,
        routine_steps,
        step_run_id,
        state,
        events_tx,
        cancel,
    })
    .await;
    let duration_ms = step_start.elapsed().as_millis() as u64;

    let result = match result {
        Ok(step_result) => {
            let _ = events_tx.send(RoutineEvent::StepCompleted {
                step_slug: step.slug.clone(),
                step_run_id,
                result: step_result.clone(),
                duration_ms,
            });
            (step_result, StepExecutionStatus::Completed)
        }
        Err(e) => {
            let error_msg = format!("{e:#}");
            warn!(step = %step.name, error = %error_msg, "Step failed");

            let _ = events_tx.send(RoutineEvent::StepFailed {
                step_slug: step.slug.clone(),
                step_run_id,
                step_name: step.name.clone(),
                step_type: step.step_type.to_string(),
                error: error_msg.clone(),
                duration_ms,
            });

            (
                StepResult {
                    passed: false,
                    output: error_msg,
                    data: serde_json::json!({
                        "reason": "execution_error",
                    }),
                    step_slug: step.slug.clone(),
                    step_name: step.name.clone(),
                    ..Default::default()
                },
                StepExecutionStatus::ExecutionFailed,
            )
        }
    };

    Ok(StepExecution {
        step,
        result: result.0,
        status: result.1,
    })
}

fn prepare_step_state(state: &mut RoutineState, step: &RoutineStepManifest) {
    state.current_step_name = Some(step.name.clone());
    state.current_step_type = Some(step.step_type.to_string());
    state.step_instructions = step
        .config
        .get("instructions")
        .or_else(|| step.config.get("description"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    state.step_metadata = step.config.get("metadata").map(|v| {
        if v.is_object() || v.is_array() || v.is_string() {
            serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
        } else {
            warn!(
                step = %step.name,
                raw = %v,
                "Step metadata is not a valid JSON object/array/string -- using raw value"
            );
            v.to_string()
        }
    });
}

fn activated_edges<'a>(
    step: &RoutineStepManifest,
    outgoing: &'a [RoutineEdgeManifest],
    passed: bool,
) -> Result<Vec<&'a RoutineEdgeManifest>> {
    if step.step_type == RoutineStepType::Gate
        && outgoing
            .iter()
            .any(|edge| edge.condition == RoutineEdgeCondition::Always)
    {
        bail!(
            "Gate step '{}' must route with on_pass/on_fail edges, not always",
            step.name
        );
    }

    if step.step_type == RoutineStepType::Agent && !passed {
        return Ok(Vec::new());
    }

    Ok(outgoing
        .iter()
        .filter(|edge| edge_matches_result(edge, passed))
        .collect())
}

fn edge_matches_result(edge: &RoutineEdgeManifest, passed: bool) -> bool {
    match edge.condition {
        RoutineEdgeCondition::Always => true,
        RoutineEdgeCondition::OnPass => passed,
        RoutineEdgeCondition::OnFail => !passed,
    }
}

fn routine_handoff_for_edge(
    step: &RoutineStepManifest,
    result: &StepResult,
    edge: &RoutineEdgeManifest,
) -> Option<RoutineHandoff> {
    let dynamic =
        route_next_step_handoff(result, &edge.target_step).unwrap_or_else(|| DynamicRouteHandoff {
            handoff: serde_json::json!({
                "output": result.output,
                "data": result.data,
            }),
            summary: None,
        });
    let purpose = edge_metadata_string(edge, "purpose");

    Some(RoutineHandoff {
        source_step: step.slug.clone(),
        target_step: edge.target_step.clone(),
        handoff: dynamic.handoff,
        purpose,
        summary: dynamic.summary,
        edge_condition: edge.condition,
    })
}

#[derive(Debug)]
struct DynamicRouteHandoff {
    handoff: serde_json::Value,
    summary: Option<String>,
}

fn route_next_step_handoff(result: &StepResult, target_step: &Slug) -> Option<DynamicRouteHandoff> {
    result
        .data
        .get("route_next_steps")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find_map(|next| {
            let target = next
                .get("target_step")
                .and_then(serde_json::Value::as_str)?;
            if target != target_step.to_string() {
                return None;
            }
            let handoff = next.get("handoff")?.clone();
            Some(DynamicRouteHandoff {
                handoff,
                summary: json_string_field(next, "summary"),
            })
        })
}

fn edge_metadata_string(edge: &RoutineEdgeManifest, key: &str) -> Option<String> {
    json_string_field(&edge.metadata, key)
}

fn json_string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn exhausted_failure_for_edge(
    step: &RoutineStepManifest,
    edge: &RoutineEdgeManifest,
    edge_traversals: &mut HashMap<String, u32>,
) -> Option<StepResult> {
    let key = edge_key(edge);
    let traversal_count = edge_traversals.entry(edge_key(edge)).or_default();
    if let Some(max_attempts) = edge_max_attempts(step, edge)
        && *traversal_count >= max_attempts
    {
        return Some(StepResult {
            passed: false,
            output: format!("Routine edge {key} exhausted after {max_attempts} attempts"),
            data: serde_json::json!({
                "reason": "retry_exhausted",
                "edge": key,
                "max_attempts": max_attempts,
            }),
            step_slug: step.slug.clone(),
            step_name: step.name.clone(),
            ..Default::default()
        });
    }

    *traversal_count += 1;
    None
}

fn target_is_ready(
    target_slug: &crate::Slug,
    incoming_by_target: &HashMap<crate::Slug, Vec<RoutineEdgeManifest>>,
    completed: &HashSet<crate::Slug>,
    traversed_edges: &HashSet<String>,
    state: &RoutineState,
    steps_by_slug: &HashMap<crate::Slug, RoutineStepManifest>,
) -> Result<bool> {
    let Some(incoming) = incoming_by_target.get(target_slug) else {
        return Ok(true);
    };

    for edge in incoming {
        let Some(source_result) = state.step_results.get(&edge.source_step) else {
            if edge.condition == RoutineEdgeCondition::OnFail {
                continue;
            }
            return Ok(false);
        };
        if !completed.contains(&edge.source_step) {
            return Ok(false);
        }
        let Some(source_step) = steps_by_slug.get(&edge.source_step) else {
            bail!(
                "validated graph is missing incoming source step '{}'",
                edge.source_step
            );
        };
        if source_step.step_type == RoutineStepType::Agent && !source_result.passed {
            return Ok(false);
        }
        if edge_matches_result(edge, source_result.passed)
            && !traversed_edges.contains(&edge_key(edge))
        {
            return Ok(false);
        }
    }

    Ok(true)
}

fn edge_max_attempts(
    current_step: &RoutineStepManifest,
    edge: &RoutineEdgeManifest,
) -> Option<u32> {
    let configured = edge
        .metadata
        .get("max_attempts")
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok());

    configured.or_else(|| {
        (current_step.step_type == RoutineStepType::Gate
            && edge.condition == RoutineEdgeCondition::OnFail)
            .then_some(DEFAULT_GATE_ON_FAIL_MAX_ATTEMPTS)
    })
}

fn edge_key(edge: &RoutineEdgeManifest) -> String {
    let condition = match edge.condition {
        RoutineEdgeCondition::Always => "always",
        RoutineEdgeCondition::OnPass => "on_pass",
        RoutineEdgeCondition::OnFail => "on_fail",
    };
    format!("{}:{}:{}", edge.source_step, condition, edge.target_step)
}

struct StepExecutionParams<'a, P> {
    provider: &'a P,
    step: &'a RoutineStepManifest,
    route_edges: &'a [RoutineEdgeManifest],
    routine_steps: &'a [RoutineStepManifest],
    step_run_id: Uuid,
    state: &'a mut RoutineState,
    events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    cancel: &'a CancellationToken,
}

struct AgentStepParams<'a, P> {
    provider: &'a P,
    step: &'a RoutineStepManifest,
    route_edges: &'a [RoutineEdgeManifest],
    routine_steps: &'a [RoutineStepManifest],
    step_run_id: Uuid,
    state: &'a RoutineState,
    events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    cancel: &'a CancellationToken,
}

struct GateStepParams<'a, P> {
    provider: &'a P,
    step: &'a RoutineStepManifest,
    route_edges: &'a [RoutineEdgeManifest],
    routine_steps: &'a [RoutineStepManifest],
    step_run_id: Uuid,
    state: &'a RoutineState,
    events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    cancel: &'a CancellationToken,
}

/// Execute a single step based on its type.
async fn execute_step<P>(params: StepExecutionParams<'_, P>) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let StepExecutionParams {
        provider,
        step,
        route_edges,
        routine_steps,
        step_run_id,
        state,
        events_tx,
        cancel,
    } = params;
    match step.step_type {
        RoutineStepType::Agent => {
            execute_agent_step(AgentStepParams {
                provider,
                step,
                route_edges,
                routine_steps,
                step_run_id,
                state,
                events_tx,
                cancel,
            })
            .await
        }
        RoutineStepType::Gate => {
            execute_gate_step(GateStepParams {
                provider,
                step,
                route_edges,
                routine_steps,
                step_run_id,
                state,
                events_tx,
                cancel,
            })
            .await
        }
        RoutineStepType::Council => {
            super::council::execute_council(provider, step, step_run_id, state, events_tx, cancel)
                .await
        }
        RoutineStepType::Terminal => {
            // Terminal step: return the most recently completed step result.
            let mut last = state.last_step_result().cloned().unwrap_or_default();
            last.passed = true;
            last.step_slug = step.slug.clone();
            last.step_name = step.name.clone();
            Ok(StepResult { ..last })
        }
        RoutineStepType::TerminalFail => {
            let reason = step
                .config
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("Routine terminated with failure")
                .to_string();
            Ok(StepResult {
                passed: false,
                output: reason,
                step_slug: step.slug.clone(),
                step_name: step.name.clone(),
                ..Default::default()
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Agent step
// ---------------------------------------------------------------------------

async fn execute_agent_step<P>(params: AgentStepParams<'_, P>) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let AgentStepParams {
        provider,
        step,
        route_edges,
        routine_steps,
        step_run_id,
        state,
        events_tx,
        cancel,
    } = params;
    let agent = step
        .agent
        .as_ref()
        .with_context(|| format!("Agent step '{}' is missing agent", step.name))?;

    let mut builder = apply_session_binding_memory_scope(
        provider.agent(agent).await?,
        state.input.session_binding.as_ref(),
    );
    if let Some(project_manifest) =
        project_manifest_for_slug(provider, state.input.project.as_ref())
    {
        builder = builder.with_project_context(&project_manifest);
    }

    // Resolve project context from manifest so agent prompts can reference
    // {{ project.name }}, {{ project.description }}, etc.
    // Inject routine + step context into the agent's render vars.
    let (routine_ctx, step_ctx) = build_routine_ctx(state, step);
    builder = builder
        .with_routine_context(routine_ctx)
        .with_step_context(step_ctx)
        .with_tool_current_session_id(step_run_id);

    builder = scope_tools_to_work_dir(builder, state);

    let builder = super::with_routine_step_max_turns(builder, step).with_tool(
        routing::route_next_steps_tool(
            route_edges,
            routine_steps,
            routing::RoutingStepKind::Agent,
        )?,
    );
    let runner = builder.build().await?;

    debug!(step = %step.name, "Building task for agent step");
    let task = attach_location(
        AgentRun::task(build_task(state, state.input.instructions.clone())?),
        state,
    );

    let output = routing::execute_with_route_next_steps(routing::ExecuteRouteNextStepsParams {
        runner: &runner,
        task,
        project: state.input.project.clone(),
        step_slug: step.slug.clone(),
        step_run_id,
        step_kind: routing::RoutingStepKind::Agent,
        route_edges,
        routine_steps,
        events_tx,
        cancel,
    })
    .await?;

    let decision = routing::resolve_route_next_steps(&output.messages)?;
    let step_output = routing::route_next_steps_display_output(&decision, &output.text);
    let data = serde_json::json!({
        "verdict": if decision.passed { "pass" } else { "fail" },
        "reasoning": decision.reasoning,
        "output": decision.output,
        "route_next_steps": decision.next_steps,
        "route_next_steps_arguments": decision.arguments,
    });

    Ok(StepResult {
        task_id: output.task_id.or(state.input.task_id),
        passed: decision.passed,
        output: step_output,
        data,
        step_slug: step.slug.clone(),
        step_name: step.name.clone(),
        input_tokens: output.input_tokens,
        output_tokens: output.output_tokens,
        tool_calls: output.tool_calls,
        messages: output.messages,
    })
}

// ---------------------------------------------------------------------------
// Gate step
// ---------------------------------------------------------------------------

async fn execute_gate_step<P>(params: GateStepParams<'_, P>) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let GateStepParams {
        provider,
        step,
        route_edges,
        routine_steps,
        step_run_id,
        state,
        events_tx,
        cancel,
    } = params;
    let agent = step
        .agent
        .as_ref()
        .with_context(|| format!("Gate step '{}' is missing agent", step.name))?;

    let (routine_ctx, step_ctx) = build_routine_ctx(state, step);
    let mut builder = apply_session_binding_memory_scope(
        provider.agent(agent).await?,
        state.input.session_binding.as_ref(),
    );
    if let Some(project_manifest) =
        project_manifest_for_slug(provider, state.input.project.as_ref())
    {
        builder = builder.with_project_context(&project_manifest);
    }
    let builder = builder
        .with_routine_context(routine_ctx)
        .with_step_context(step_ctx)
        .with_tool_current_session_id(step_run_id);
    let builder = scope_tools_to_work_dir(builder, state);
    let runner = super::with_routine_step_max_turns(builder, step)
        .with_tool(routing::route_next_steps_tool(
            route_edges,
            routine_steps,
            routing::RoutingStepKind::Gate,
        )?)
        .build()
        .await?;

    let previous_result = state.last_step_result().cloned().unwrap_or_default();

    let task = attach_location(
        AgentRun {
            kind: AgentRunKind::Gate(GateInput {
                previous_result,
                project: state.input.project.clone(),
                task: Some(build_task(state, state.input.instructions.clone())?),
            }),
            execution: Default::default(),
        },
        state,
    );

    let output = routing::execute_with_route_next_steps(routing::ExecuteRouteNextStepsParams {
        runner: &runner,
        task,
        project: state.input.project.clone(),
        step_slug: step.slug.clone(),
        step_run_id,
        step_kind: routing::RoutingStepKind::Gate,
        route_edges,
        routine_steps,
        events_tx,
        cancel,
    })
    .await?;

    let decision = routing::resolve_route_next_steps(&output.messages)?;
    let step_output = routing::route_next_steps_display_output(&decision, &output.text);

    // Keep the structured verdict and reasoning available to event consumers
    // and downstream routine handoffs.
    let data = serde_json::json!({
        "verdict": if decision.passed { "pass" } else { "fail" },
        "reasoning": decision.reasoning,
        "output": decision.output,
        "route_next_steps": decision.next_steps,
        "route_next_steps_arguments": decision.arguments,
    });

    Ok(StepResult {
        task_id: output.task_id.or(state.input.task_id),
        passed: decision.passed,
        output: step_output,
        data,
        step_slug: step.slug.clone(),
        step_name: step.name.clone(),
        input_tokens: output.input_tokens,
        output_tokens: output.output_tokens,
        tool_calls: output.tool_calls,
        messages: output.messages,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build task input from the routine state.
fn build_task(state: &RoutineState, description: String) -> Result<TaskInput> {
    Ok(TaskInput {
        task_id: state.input.task_id.unwrap_or_else(Uuid::nil),
        title: state.input.title.clone(),
        instructions: description,
        labels: state.input.labels.clone(),
        project: state.input.project.clone(),
        status: state.input.status.clone(),
        priority: state.input.priority.clone(),
        slug: state.input.slug.clone(),
    })
}

fn attach_location(mut run: AgentRun, state: &RoutineState) -> AgentRun {
    if let Some(git) = state.input.git.clone() {
        run.execution.project_location = Some(ProjectLocation::from_git(git));
    }
    run
}
