//! DAG executor — walks routine steps following conditional edges.

use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::AgentBuilder;
use crate::manifest::{
    RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest, RoutineStepManifest,
    RoutineStepType,
};
use crate::provider::Provider;
use crate::routines::types::StepResult;
use crate::routines::{apply_session_binding_memory_scope, gate};
use crate::types::TaskType;

use super::RoutineEvent;
use super::types::{CronMode, CronStepConfig, RoutineState};
use crate::context::{RoutineContext, RoutineStepContext};

/// Build `RoutineContext` and `RoutineStepContext` from the current execution state.
fn build_routine_ctx(state: &RoutineState) -> (RoutineContext, RoutineStepContext) {
    let routine = RoutineContext {
        id: state.routine_id,
        name: state.routine_name.clone().unwrap_or_default(),
        execution_id: state
            .input
            .execution_run_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        description: None,
        step: Default::default(),
    };
    let step = RoutineStepContext {
        name: state.current_step_name.clone().unwrap_or_default(),
        step_type: state.current_step_type.clone().unwrap_or_default(),
        metadata: state.step_metadata.clone().unwrap_or_default(),
    };
    (routine, step)
}

fn scope_tools_to_work_dir(mut builder: AgentBuilder, state: &RoutineState) -> AgentBuilder {
    if let Some(ref git) = state.input.git
        && !git.work_dir.is_empty()
    {
        builder = builder.with_work_dir(&git.work_dir);
    }
    builder
}

/// Execute a routine once (one-shot). For cron execution, the Provider
/// wraps this in the cron poll loop.
pub(crate) async fn execute_routine(
    provider: &Provider,
    routine: &RoutineManifest,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
    cancel: &CancellationToken,
) -> Result<StepResult> {
    execute_routine_once(provider, routine, state, events_tx, cancel).await
}

/// Execute a routine once (the core DAG walk).
///
/// Finds the entry step, executes it, follows the first matching outgoing edge,
/// and repeats until a terminal node or no matching edge is found.
pub(crate) async fn execute_routine_once(
    provider: &Provider,
    routine: &RoutineManifest,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
    cancel: &CancellationToken,
) -> Result<StepResult> {
    let steps = &routine.steps;
    let edges = &routine.edges;

    if steps.is_empty() {
        anyhow::bail!("Routine '{}' has no steps", routine.name);
    }

    // Find entry step
    let entry_step_ids = &routine.metadata.entry_step_ids;

    let entry_step = if !entry_step_ids.is_empty() {
        steps
            .iter()
            .find(|s| entry_step_ids.contains(&s.id))
            .with_context(|| "Entry step ID from metadata not found in steps")?
    } else {
        // Fallback: find step with no incoming edges
        let targets: HashSet<Uuid> = edges.iter().map(|e| e.target_step_id).collect();
        steps
            .iter()
            .find(|s| !targets.contains(&s.id))
            .or_else(|| steps.iter().min_by_key(|s| s.order_index))
            .context("Could not determine entry step")?
    };

    let mut current_step = entry_step.clone();
    let mut last_result = StepResult::default();
    let max_iterations = steps.len() * 100;

    for iteration in 0..max_iterations {
        if cancel.is_cancelled() {
            last_result = StepResult {
                passed: false,
                output: "Cancelled".to_string(),
                ..Default::default()
            };
            break;
        }

        debug!(
            iteration,
            step = %current_step.name,
            step_type = %current_step.step_type,
            "Executing routine step"
        );

        let step_run_id = Uuid::new_v4();
        state.current_step_name = Some(current_step.name.clone());
        state.current_step_type = Some(current_step.step_type.to_string());
        state.current_agent_id = current_step.agent_id;
        // Parse step metadata — expect a JSON value. If the raw config value
        // is not valid JSON (e.g. a bare string without quotes), serialize it
        // as-is and log a warning so the user can fix their routine config.
        state.step_metadata = current_step.config.get("metadata").map(|v| {
            if v.is_object() || v.is_array() || v.is_string() {
                serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
            } else {
                warn!(
                    step = %current_step.name,
                    raw = %v,
                    "Step metadata is not a valid JSON object/array/string — using raw value"
                );
                v.to_string()
            }
        });

        // Emit step started
        let _ = events_tx.send(RoutineEvent::StepStarted {
            step_id: current_step.id,
            step_run_id,
            step_name: current_step.name.clone(),
            step_type: current_step.step_type.to_string(),
            agent_id: current_step.agent_id,
        });

        // Execute the step
        let step_start = std::time::Instant::now();
        let result = execute_step(provider, &current_step, step_run_id, state, events_tx).await;
        let duration_ms = step_start.elapsed().as_millis() as u64;

        match result {
            Ok(step_result) => {
                // Record metrics
                state.metrics.record_step(
                    current_step.id,
                    step_result.input_tokens,
                    step_result.output_tokens,
                );

                let _ = events_tx.send(RoutineEvent::StepCompleted {
                    step_id: current_step.id,
                    step_run_id,
                    result: step_result.clone(),
                    duration_ms,
                });

                // Store gate feedback if this step failed (for on_fail edges).
                // For structured-evaluation steps, prefer the structured reasoning from the
                // pass_verdict tool over the raw LLM output.
                if !step_result.passed
                    && matches!(
                        current_step.step_type,
                        RoutineStepType::Gate | RoutineStepType::Cron | RoutineStepType::Council
                    )
                {
                    let feedback = step_result
                        .data
                        .get("reasoning")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| step_result.output.clone());
                    state.gate_feedback = Some(feedback);
                }

                state
                    .step_results
                    .insert(current_step.id, step_result.clone());
                last_result = step_result;
            }
            Err(e) => {
                let error_msg = format!("{e:#}");
                warn!(step = %current_step.name, error = %error_msg, "Step failed");

                let _ = events_tx.send(RoutineEvent::StepFailed {
                    step_id: current_step.id,
                    step_run_id,
                    error: error_msg.clone(),
                    duration_ms,
                });

                last_result = StepResult {
                    passed: false,
                    output: error_msg,
                    step_id: current_step.id,
                    step_name: current_step.name.clone(),
                    ..Default::default()
                };
                state
                    .step_results
                    .insert(current_step.id, last_result.clone());
            }
        }

        if cancel.is_cancelled() {
            last_result = StepResult {
                passed: false,
                output: "Cancelled".to_string(),
                step_id: current_step.id,
                step_name: current_step.name.clone(),
                ..Default::default()
            };
            break;
        }

        // Check for terminal steps
        if matches!(
            current_step.step_type,
            RoutineStepType::Terminal | RoutineStepType::TerminalFail
        ) {
            break;
        }

        // Agent-step fail verdicts are terminal for the routine.
        if current_step.step_type == RoutineStepType::Agent && !last_result.passed {
            debug!(
                step = %current_step.name,
                "Agent step returned fail verdict, terminating routine"
            );
            break;
        }

        // Follow the first matching edge
        let outgoing: Vec<_> = edges
            .iter()
            .filter(|e| e.source_step_id == current_step.id)
            .collect();

        if current_step.step_type == RoutineStepType::Gate
            && outgoing
                .iter()
                .any(|edge| edge.condition == RoutineEdgeCondition::Always)
        {
            bail!(
                "Gate step '{}' must route with on_pass/on_fail edges, not always",
                current_step.name
            );
        }

        let next_edge = choose_next_edge(&outgoing, last_result.passed);

        match next_edge {
            Some(edge) => {
                current_step = steps
                    .iter()
                    .find(|s| s.id == edge.target_step_id)
                    .with_context(|| format!("Edge target step {} not found", edge.target_step_id))?
                    .clone();
            }
            None => {
                debug!(step = %current_step.name, "No matching outgoing edge, routine complete");
                break;
            }
        }
    }

    let _ = events_tx.send(RoutineEvent::Done {
        task_id: state.input.task_id,
        result: last_result.clone(),
    });

    Ok(last_result)
}

fn choose_next_edge<'a>(
    outgoing: &'a [&'a RoutineEdgeManifest],
    passed: bool,
) -> Option<&'a RoutineEdgeManifest> {
    let preferred = if passed {
        RoutineEdgeCondition::OnPass
    } else {
        RoutineEdgeCondition::OnFail
    };

    outgoing
        .iter()
        .copied()
        .find(|edge| edge.condition == preferred)
        .or_else(|| {
            outgoing
                .iter()
                .copied()
                .find(|edge| edge.condition == RoutineEdgeCondition::Always)
        })
}

/// Execute a single step based on its type.
async fn execute_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    match step.step_type {
        RoutineStepType::Agent => {
            execute_agent_step(provider, step, step_run_id, state, events_tx).await
        }
        RoutineStepType::Gate => {
            execute_gate_step(provider, step, step_run_id, state, events_tx).await
        }
        RoutineStepType::Council => {
            super::council::execute_council(provider, step, step_run_id, state, events_tx).await
        }
        RoutineStepType::Cron => {
            execute_cron_step(provider, step, step_run_id, state, events_tx).await
        }
        RoutineStepType::Terminal => {
            // Terminal step: return the most recent step result
            let last = state
                .step_results
                .values()
                .last()
                .cloned()
                .unwrap_or_default();
            Ok(StepResult {
                passed: true,
                output: last.output,
                step_id: step.id,
                step_name: step.name.clone(),
                ..Default::default()
            })
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
                step_id: step.id,
                step_name: step.name.clone(),
                ..Default::default()
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Agent step
// ---------------------------------------------------------------------------

async fn execute_agent_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let agent_id = step
        .agent_id
        .with_context(|| format!("Agent step '{}' is missing agent_id", step.name))?;

    let mut builder = apply_session_binding_memory_scope(
        provider.agent_by_id(agent_id).await?,
        state.input.session_binding.as_ref(),
    );

    // Resolve project context from manifest so agent prompts can reference
    // {{ project.name }}, {{ project.description }}, etc.
    if !state.input.project_id.is_nil()
        && let Some(project) = provider
            .manifest()
            .projects
            .iter()
            .find(|p| p.id == state.input.project_id)
    {
        builder = builder.with_project_context(project);
    }

    // Inject routine + step context into the agent's render vars.
    let (routine_ctx, step_ctx) = build_routine_ctx(state);
    builder = builder
        .with_routine_context(routine_ctx)
        .with_step_context(step_ctx);

    builder = scope_tools_to_work_dir(builder, state);

    let runner = super::with_agent_step_tools(super::with_routine_step_max_turns(builder, step))
        .build()
        .await?;

    // Build the task description from template context
    let task_description = build_task_description(step, state);

    // If the routine was triggered by a cron, use TaskType::Cron so the
    // agent's cron_task template is selected instead of task_execution.
    debug!(is_cron = state.input.is_cron_trigger, step = %step.name, "Building task for agent step");
    let task = if state.input.is_cron_trigger {
        // Pass task context only when the cron step runs inside a
        // task-triggered routine (i.e. there's a real task_id).
        let inner_task = state
            .input
            .task_id
            .map(|_| build_task(state, task_description));
        crate::types::TaskType::Cron {
            task: inner_task,
            project_id: state.input.project_id,
            schedule: crate::routines::types::CronSchedule::Interval(
                std::time::Duration::from_secs(0),
            ),
            start_at: None,
            timeout: std::time::Duration::from_secs(0),
        }
    } else {
        build_task_type(step, state, task_description)
    };

    let output = gate::execute_with_pass_verdict(
        &runner,
        task,
        state.input.project_id,
        step.id,
        step_run_id,
        events_tx,
    )
    .await?;

    let verdict = gate::resolve_pass_verdict(&output.messages)?;
    let step_output = gate::pass_verdict_display_output(&verdict, &output.text);
    let data = serde_json::json!({
        "verdict": if verdict.passed { "pass" } else { "fail" },
        "reasoning": verdict.reasoning,
        "output": verdict.output,
    });

    Ok(StepResult {
        passed: verdict.passed,
        output: step_output,
        data,
        step_id: step.id,
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

async fn execute_gate_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let agent_id = step
        .agent_id
        .with_context(|| format!("Gate step '{}' is missing agent_id", step.name))?;

    let (routine_ctx, step_ctx) = build_routine_ctx(state);
    let builder = apply_session_binding_memory_scope(
        provider.agent_by_id(agent_id).await?,
        state.input.session_binding.as_ref(),
    )
    .with_routine_context(routine_ctx)
    .with_step_context(step_ctx);
    let builder = scope_tools_to_work_dir(builder, state);
    let runner = super::with_agent_step_tools(super::with_routine_step_max_turns(builder, step))
        .build()
        .await?;

    let criteria = step
        .config
        .get("criteria")
        .and_then(|v| v.as_str())
        .unwrap_or("Evaluate whether the previous step output is acceptable.");

    let previous_result = state
        .step_results
        .values()
        .last()
        .cloned()
        .unwrap_or_default();

    let task = TaskType::Gate {
        previous_result,
        criteria: criteria.to_string(),
        project_id: state.input.project_id,
        task: Some(build_task(state, state.input.description.clone())),
    };

    let output = gate::execute_with_pass_verdict(
        &runner,
        task,
        state.input.project_id,
        step.id,
        step_run_id,
        events_tx,
    )
    .await?;

    let verdict = gate::resolve_pass_verdict(&output.messages)?;
    let step_output = gate::pass_verdict_display_output(&verdict, &output.text);

    // Store verdict + reasoning in `data` so the event bus and gate_feedback
    // can surface structured information instead of raw LLM text.
    let data = serde_json::json!({
        "verdict": if verdict.passed { "pass" } else { "fail" },
        "reasoning": verdict.reasoning,
        "output": verdict.output,
    });

    Ok(StepResult {
        passed: verdict.passed,
        output: step_output,
        data,
        step_id: step.id,
        step_name: step.name.clone(),
        input_tokens: output.input_tokens,
        output_tokens: output.output_tokens,
        tool_calls: output.tool_calls,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Cron step
// ---------------------------------------------------------------------------

async fn execute_cron_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let config = CronStepConfig::from_config(&step.config, step.agent_id, None)?;

    debug!(cycle = 1u32, step = %step.name, "Cron cycle");

    let cycle_result = match &config.mode {
        CronMode::Agent(agent_id) => {
            let (routine_ctx, step_ctx) = build_routine_ctx(state);
            let builder = apply_session_binding_memory_scope(
                provider.agent_by_id(*agent_id).await?,
                state.input.session_binding.as_ref(),
            )
            .with_routine_context(routine_ctx)
            .with_step_context(step_ctx);
            let builder = scope_tools_to_work_dir(builder, state);
            let runner =
                super::with_agent_step_tools(super::with_routine_step_max_turns(builder, step))
                    .build()
                    .await?;
            // Use TaskType::Cron so the agent's cron_task template is selected.
            let inner_task = state
                .input
                .task_id
                .map(|_| build_task(state, state.input.description.clone()));
            let task = crate::types::TaskType::Cron {
                task: inner_task,
                project_id: state.input.project_id,
                schedule: crate::routines::types::CronSchedule::Interval(config.interval),
                start_at: None,
                timeout: config.timeout,
            };

            gate::execute_with_pass_verdict(
                &runner,
                task,
                state.input.project_id,
                step.id,
                step_run_id,
                events_tx,
            )
            .await?
        }
        CronMode::Lambda(lambda_id) => {
            anyhow::bail!("Cron lambda steps are no longer supported (lambda_id={lambda_id})")
        }
    };

    if let Some(passed) = gate::extract_pass_verdict(&cycle_result.messages) {
        let reasoning = gate::extract_pass_reasoning(&cycle_result.messages);
        let verdict_output = gate::extract_pass_output(&cycle_result.messages);
        let step_output = verdict_output
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                reasoning
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or(&cycle_result.text)
            .to_string();
        let data = serde_json::json!({
            "verdict": if passed { "pass" } else { "fail" },
            "reasoning": reasoning,
            "output": verdict_output,
        });
        return Ok(StepResult {
            passed,
            output: step_output,
            data,
            step_id: step.id,
            step_name: step.name.clone(),
            input_tokens: cycle_result.input_tokens,
            output_tokens: cycle_result.output_tokens,
            ..Default::default()
        });
    }

    bail!(
        "Cron step '{}' completed without required pass_verdict",
        step.name
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `Task` struct from the routine state.
fn build_task(state: &RoutineState, description: String) -> crate::types::Task {
    crate::types::Task {
        task_id: state.input.task_id.unwrap_or_else(Uuid::nil),
        title: state.input.title.clone(),
        description,
        acceptance_criteria: state.input.acceptance_criteria.clone(),
        tags: state.input.tags.clone(),
        source: state
            .input
            .source
            .clone()
            .unwrap_or_else(|| "routine".to_string()),
        project_id: state.input.project_id,
        status: state.input.status.clone().unwrap_or_default(),
        priority: state.input.priority.clone().unwrap_or_default(),
        task_type: state.input.task_type.clone().unwrap_or_default(),
        slug: state.input.slug.clone().unwrap_or_default(),
        complexity: state.input.complexity.clone().unwrap_or_default(),
        git: state.input.git.clone(),
    }
}

/// Build a TaskType::Task from the routine state and step config.
fn build_task_type(
    _step: &RoutineStepManifest,
    state: &RoutineState,
    description: String,
) -> TaskType {
    TaskType::Task(build_task(state, description))
}

/// Build a task description from step config and routine state.
fn build_task_description(step: &RoutineStepManifest, state: &RoutineState) -> String {
    // Use step config description if available, otherwise fall back to input description
    let base = step
        .config
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or(&state.input.description);

    let mut description = base.to_string();

    // Append gate feedback if available (from a previous failed gate)
    if let Some(ref feedback) = state.gate_feedback {
        description.push_str(&format!(
            "\n\n<gate_feedback>\nThe previous attempt was reviewed and rejected:\n{feedback}\n</gate_feedback>"
        ));
    }

    // Append previous step context
    if let Some(last) = state.step_results.values().last()
        && !last.output.is_empty()
    {
        let preview = if last.output.len() > 2000 {
            format!("{}...", &last.output[..2000])
        } else {
            last.output.clone()
        };
        description.push_str(&format!(
            "\n\n<previous_step>\nStep '{}' output:\n{preview}\n</previous_step>",
            last.step_name
        ));
    }

    description
}
