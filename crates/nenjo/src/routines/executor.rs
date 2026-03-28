//! DAG executor — walks routine steps following conditional edges.

use std::collections::HashSet;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::manifest::{RoutineManifest, RoutineStepManifest};
use crate::provider::Provider;
use crate::routines::gate;
use crate::routines::types::StepResult;
use crate::types::TaskType;

use super::RoutineEvent;
use super::types::{
    CronMode, CronStepConfig, EdgeCondition, LambdaStepConfig, RoutineState, StepType,
};

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
    _cancel: &CancellationToken,
) -> Result<StepResult> {
    let steps = &routine.steps;
    let edges = &routine.edges;

    if steps.is_empty() {
        anyhow::bail!("Routine '{}' has no steps", routine.name);
    }

    // Find entry step
    let entry_step_ids: Vec<Uuid> = routine
        .metadata
        .get("entry_step_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                .collect()
        })
        .unwrap_or_default();

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
        debug!(
            iteration,
            step = %current_step.name,
            step_type = %current_step.step_type,
            "Executing routine step"
        );

        state.current_step_name = Some(current_step.name.clone());
        state.step_metadata = current_step.config.get("metadata").map(|v| v.to_string());

        // Emit step started
        let _ = events_tx.send(RoutineEvent::StepStarted {
            step_id: current_step.id,
            step_name: current_step.name.clone(),
            step_type: current_step.step_type.clone(),
        });

        // Execute the step
        let result = execute_step(provider, &current_step, state, events_tx).await;

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
                    result: step_result.clone(),
                });

                // Store gate feedback if this step failed (for on_fail edges).
                // For gate steps, prefer the structured reasoning from the
                // gate_verdict tool over the raw LLM output.
                if !step_result.passed {
                    let step_type = StepType::from_str_value(&current_step.step_type);
                    if matches!(
                        step_type,
                        StepType::Gate | StepType::Cron | StepType::Lambda | StepType::Council
                    ) {
                        let feedback = step_result
                            .data
                            .get("reasoning")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| step_result.output.clone());
                        state.gate_feedback = Some(feedback);
                    }
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
                    error: error_msg.clone(),
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

        // Check for terminal steps
        let step_type = StepType::from_str_value(&current_step.step_type);
        if matches!(step_type, StepType::Terminal | StepType::TerminalFail) {
            break;
        }

        // Follow the first matching edge
        let outgoing: Vec<_> = edges
            .iter()
            .filter(|e| e.source_step_id == current_step.id)
            .collect();

        let next_edge = outgoing.iter().find(|e| {
            let condition = EdgeCondition::from_str_value(&e.condition);
            condition.is_satisfied(last_result.passed)
        });

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
        result: last_result.clone(),
    });

    Ok(last_result)
}

/// Execute a single step based on its type.
async fn execute_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let step_type = StepType::from_str_value(&step.step_type);

    match step_type {
        StepType::Agent => execute_agent_step(provider, step, state, events_tx).await,
        StepType::Gate => execute_gate_step(provider, step, state, events_tx).await,
        StepType::Lambda => execute_lambda_step(provider, step, state).await,
        StepType::Council => {
            super::council::execute_council(provider, step, state, events_tx).await
        }
        StepType::Cron => execute_cron_step(provider, step, state, events_tx).await,
        StepType::Terminal => {
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
        StepType::TerminalFail => {
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
    state: &RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let agent_id = step
        .agent_id
        .or_else(|| resolve_agent_from_model(provider, step.model_id))
        .with_context(|| format!("No agent found for step '{}'", step.name))?;

    let mut builder = provider.agent_by_id(agent_id).await?;

    // Resolve project context from manifest so agent prompts can reference
    // {{ project.name }}, {{ project.description }}, etc.
    if !state.input.project_id.is_nil() {
        if let Some(project) = provider
            .manifest()
            .projects
            .iter()
            .find(|p| p.id == state.input.project_id)
        {
            builder = builder.with_project_context(project);
        }
    }

    // Inject routine context so agent prompts can reference
    // {{ routine.name }}, {{ routine.id }}, {{ routine.execution_id }}.
    let execution_id = state
        .input
        .execution_run_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    builder = builder.with_routine_context(
        state.routine_id,
        state.routine_name.as_deref().unwrap_or(""),
        &execution_id,
    );

    // Inject step metadata if available.
    if let Some(ref metadata) = state.step_metadata {
        builder = builder.with_step_metadata(metadata);
    }

    let runner = builder.build();

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
            interval: std::time::Duration::from_secs(0),
            timeout: std::time::Duration::from_secs(0),
        }
    } else {
        build_task_type(step, state, task_description)
    };

    let mut handle = runner.task_stream(task).await?;

    while let Some(event) = handle.recv().await {
        let _ = events_tx.send(RoutineEvent::AgentEvent {
            step_id: step.id,
            event,
        });
    }

    let output = handle.output().await?;

    Ok(StepResult {
        passed: true,
        output: output.text,
        step_id: step.id,
        step_name: step.name.clone(),
        input_tokens: output.input_tokens,
        output_tokens: output.output_tokens,
        tool_calls: output.tool_calls,
        messages: output.messages,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Gate step
// ---------------------------------------------------------------------------

async fn execute_gate_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    state: &RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let agent_id = step
        .agent_id
        .or_else(|| resolve_agent_from_model(provider, step.model_id))
        .with_context(|| format!("No agent found for gate step '{}'", step.name))?;

    let execution_id = state
        .input
        .execution_run_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    let runner = provider
        .agent_by_id(agent_id)
        .await?
        .with_tool(gate::GateVerdictTool::new())
        .with_routine_context(
            state.routine_id,
            state.routine_name.as_deref().unwrap_or(""),
            &execution_id,
        )
        .build();

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

    let mut handle = runner.task_stream(task).await?;

    while let Some(event) = handle.recv().await {
        let _ = events_tx.send(RoutineEvent::AgentEvent {
            step_id: step.id,
            event,
        });
    }

    let output = handle.output().await?;

    // Extract verdict from the gate_verdict tool call, falling back to
    // JSON parsing in the response text if the agent didn't call the tool.
    let verdict = gate::resolve_gate_verdict(&output.messages, &output.text);

    // Store verdict + reasoning in `data` so the event bus and gate_feedback
    // can surface structured information instead of raw LLM text.
    let data = serde_json::json!({
        "verdict": if verdict.passed { "pass" } else { "fail" },
        "reasoning": verdict.reasoning,
    });

    Ok(StepResult {
        passed: verdict.passed,
        output: output.text,
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
// Lambda step
// ---------------------------------------------------------------------------

async fn execute_lambda_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    state: &RoutineState,
) -> Result<StepResult> {
    let lambda_runner = provider
        .lambda_runner()
        .context("Lambda step requires a LambdaRunner (configure with .with_lambda_runner())")?;

    let config = LambdaStepConfig::from_config(&step.config, step.lambda_id)?;

    let lambda = provider
        .manifest()
        .lambdas
        .iter()
        .find(|l| l.id == config.lambda_id)
        .with_context(|| format!("Lambda {} not found in manifest", config.lambda_id))?;

    let interpreter = config.interpreter.as_deref().unwrap_or(&lambda.interpreter);

    // Build env vars with context
    let mut env = std::collections::HashMap::new();
    env.insert("ROUTINE_ID".to_string(), state.routine_id.to_string());
    env.insert("STEP_ID".to_string(), step.id.to_string());
    env.insert("STEP_NAME".to_string(), step.name.clone());
    env.insert("PROJECT_ID".to_string(), state.input.project_id.to_string());
    if let Some(ref run_id) = state.input.execution_run_id {
        env.insert("EXECUTION_RUN_ID".to_string(), run_id.to_string());
    }

    // Add previous step output
    if let Some(last) = state.step_results.values().last() {
        env.insert("PREVIOUS_OUTPUT".to_string(), last.output.clone());
        env.insert("PREVIOUS_PASSED".to_string(), last.passed.to_string());
    }

    let script_path = std::path::PathBuf::from(&lambda.path);

    let result = lambda_runner
        .run_script(&script_path, interpreter, env, config.timeout)
        .await?;

    let passed = result.exit_code == 0;
    let output = if passed {
        result.stdout
    } else {
        format!(
            "Lambda exited with code {}.\nStdout: {}\nStderr: {}",
            result.exit_code, result.stdout, result.stderr
        )
    };

    Ok(StepResult {
        passed,
        output,
        step_id: step.id,
        step_name: step.name.clone(),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Cron step
// ---------------------------------------------------------------------------

async fn execute_cron_step(
    provider: &Provider,
    step: &RoutineStepManifest,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let config = CronStepConfig::from_config(&step.config, step.agent_id, step.lambda_id)?;

    let deadline = tokio::time::Instant::now() + config.timeout;
    let mut cycle = 0u32;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;

    loop {
        cycle += 1;
        debug!(cycle, step = %step.name, "Cron cycle");

        let cycle_result = match &config.mode {
            CronMode::Agent(agent_id) => {
                let cron_exec_id = state
                    .input
                    .execution_run_id
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                let runner = provider
                    .agent_by_id(*agent_id)
                    .await?
                    .with_tool(gate::GateVerdictTool::new())
                    .with_routine_context(
                        state.routine_id,
                        state.routine_name.as_deref().unwrap_or(""),
                        &cron_exec_id,
                    )
                    .build();
                // Use TaskType::Cron so the agent's cron_task template is selected.
                let inner_task = state
                    .input
                    .task_id
                    .map(|_| build_task(state, state.input.description.clone()));
                let task = crate::types::TaskType::Cron {
                    task: inner_task,
                    project_id: state.input.project_id,
                    interval: config.interval,
                    timeout: config.timeout,
                };

                let mut handle = runner.task_stream(task).await?;
                while let Some(event) = handle.recv().await {
                    let _ = events_tx.send(RoutineEvent::AgentEvent {
                        step_id: step.id,
                        event,
                    });
                }
                handle.output().await?
            }
            CronMode::Lambda(lambda_id) => {
                let lambda_runner = provider
                    .lambda_runner()
                    .context("Cron step in lambda mode requires a LambdaRunner")?;

                let lambda = provider
                    .manifest()
                    .lambdas
                    .iter()
                    .find(|l| l.id == *lambda_id)
                    .with_context(|| format!("Lambda {lambda_id} not found"))?;

                let mut env = std::collections::HashMap::new();
                env.insert("CYCLE".to_string(), cycle.to_string());
                env.insert("STEP_NAME".to_string(), step.name.clone());

                let script_path = std::path::PathBuf::from(&lambda.path);
                let lambda_output = lambda_runner
                    .run_script(&script_path, &lambda.interpreter, env, config.interval)
                    .await?;

                crate::agents::runner::types::TurnOutput {
                    text: lambda_output.stdout,
                    input_tokens: 0,
                    output_tokens: 0,
                    tool_calls: 0,
                    messages: Vec::new(),
                }
            }
        };

        total_input_tokens += cycle_result.input_tokens;
        total_output_tokens += cycle_result.output_tokens;

        // Check for gate_verdict tool call — the deterministic completion
        // signal. If the agent called gate_verdict, the cycle is done.
        if let Some(passed) = gate::extract_gate_verdict(&cycle_result.messages) {
            let reasoning = gate::extract_gate_reasoning(&cycle_result.messages);
            let data = serde_json::json!({
                "verdict": if passed { "pass" } else { "fail" },
                "reasoning": reasoning,
            });
            return Ok(StepResult {
                passed,
                output: cycle_result.text,
                data,
                step_id: step.id,
                step_name: step.name.clone(),
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                ..Default::default()
            });
        }

        // No verdict — agent wants another cycle.

        // Check timeout
        if tokio::time::Instant::now() >= deadline {
            return Ok(StepResult {
                passed: false,
                output: format!("Cron step '{}' timed out after {} cycles", step.name, cycle),
                step_id: step.id,
                step_name: step.name.clone(),
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                ..Default::default()
            });
        }

        // Sleep for interval
        tokio::time::sleep(config.interval).await;
    }
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

/// Resolve an agent ID from a model assignment.
fn resolve_agent_from_model(provider: &Provider, model_id: Option<Uuid>) -> Option<Uuid> {
    let model_id = model_id?;
    provider
        .manifest()
        .agents
        .iter()
        .find(|a| a.model_id == Some(model_id))
        .map(|a| a.id)
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
    if let Some(last) = state.step_results.values().last() {
        if !last.output.is_empty() {
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
    }

    description
}
