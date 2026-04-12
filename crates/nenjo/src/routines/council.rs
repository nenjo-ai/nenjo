//! Council execution — multi-agent delegation strategies.
//!
//! A council is a group of agents with a leader and members. Two strategies:
//! - **dynamic**: leader gets free reign to delegate via tool calls
//! - **decompose**: leader splits task → members execute → leader aggregates

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::RoutineEvent;
use super::types::RoutineState;
use super::{apply_session_binding_memory_scope, gate};
use crate::manifest::{CouncilManifest, RoutineStepManifest};
use crate::provider::Provider;
use crate::routines::types::StepResult;
use crate::types::TaskType;

/// Execute a council step, dispatching based on the council's delegation_strategy.
pub(crate) async fn execute_council(
    provider: &Provider,
    step: &RoutineStepManifest,
    state: &RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let council_id = step.council_id.context("Council step missing council_id")?;

    let council = provider
        .manifest()
        .councils
        .iter()
        .find(|c| c.id == council_id)
        .with_context(|| format!("Council {council_id} not found in manifest"))?
        .clone();

    match council.delegation_strategy.as_str() {
        "dynamic" => execute_dynamic(provider, step, state, &council, events_tx).await,
        _ => execute_decompose(provider, step, state, &council, events_tx).await,
    }
}

/// Dynamic: leader gets free reign to work (with delegation tools if configured).
async fn execute_dynamic(
    provider: &Provider,
    step: &RoutineStepManifest,
    state: &RoutineState,
    council: &CouncilManifest,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let leader_agent_id = council.leader_agent_id;

    info!(
        step_name = %step.name,
        leader = %leader_agent_id,
        strategy = "dynamic",
        "Starting dynamic council execution"
    );

    let runner_builder = provider
        .agent_by_id(leader_agent_id)
        .await?
        .with_tool(gate::GateVerdictTool::new());
    let runner =
        apply_session_binding_memory_scope(runner_builder, state.input.session_binding.as_ref())
            .build()
            .await?;

    let task = TaskType::Chat {
        user_message: state.initial_input.clone(),
        history: Vec::new(),
        project_id: state.input.project_id,
    };

    let mut handle = runner.task_stream(task).await?;

    while let Some(event) = handle.recv().await {
        let _ = events_tx.send(RoutineEvent::AgentEvent {
            step_id: step.id,
            event,
        });
    }

    let output = handle.output().await?;

    info!(
        step_name = %step.name,
        "Dynamic council execution complete"
    );

    let verdict = gate::resolve_gate_verdict(&output.messages, &output.text);
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
        messages: output.messages,
    })
}

/// Decompose: leader splits → members execute → leader aggregates.
async fn execute_decompose(
    provider: &Provider,
    step: &RoutineStepManifest,
    state: &RoutineState,
    council: &CouncilManifest,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let leader_agent_id = council.leader_agent_id;
    let member_agent_ids: Vec<Uuid> = council.members.iter().map(|m| m.agent_id).collect();

    if member_agent_ids.is_empty() {
        anyhow::bail!("Council '{}' has no members configured", council.name);
    }

    debug!(
        step_name = %step.name,
        leader = %leader_agent_id,
        members = member_agent_ids.len(),
        strategy = "decompose",
        "Starting decompose council execution"
    );

    // Step 1: Leader decomposes
    let leader = apply_session_binding_memory_scope(
        provider.agent_by_id(leader_agent_id).await?,
        state.input.session_binding.as_ref(),
    )
    .build()
    .await?;

    let decompose_message = format!(
        "You are the leader of a team of {} agents. Decompose the following work \
         into exactly {} subtasks, one for each team member.\n\n\
         Task: {}\n\n\
         Respond with a numbered list:\n\
         1. [subtask description]\n\
         2. [subtask description]\n\
         ...",
        member_agent_ids.len(),
        member_agent_ids.len(),
        state.initial_input
    );

    let decompose_task = TaskType::Chat {
        user_message: decompose_message,
        history: Vec::new(),
        project_id: state.input.project_id,
    };

    let decompose_result = leader.task(decompose_task).await?;
    let subtasks = parse_subtasks(&decompose_result.text, member_agent_ids.len());

    debug!(parsed_subtasks = subtasks.len(), "Leader decomposed task");

    // Step 2: Members execute subtasks
    let mut member_results: Vec<StepResult> = Vec::new();

    for (i, (agent_id, subtask_desc)) in member_agent_ids.iter().zip(subtasks.iter()).enumerate() {
        debug!(
            member_index = i,
            agent_id = %agent_id,
            subtask = %subtask_desc,
            "Executing member subtask"
        );

        let member_runner = match provider.agent_by_id(*agent_id).await {
            Ok(builder) => {
                apply_session_binding_memory_scope(builder, state.input.session_binding.as_ref())
                    .build()
                    .await?
            }
            Err(e) => {
                warn!(agent_id = %agent_id, error = %e, "Failed to build member agent");
                member_results.push(StepResult {
                    passed: false,
                    output: format!("Failed to build agent: {e}"),
                    step_name: format!("member-{}", i + 1),
                    ..Default::default()
                });
                continue;
            }
        };

        let task = TaskType::CouncilSubtask {
            parent_task: state.initial_input.clone(),
            subtask_description: subtask_desc.clone(),
            subtask_index: i,
            project_id: state.input.project_id,
        };

        // Stream member events
        match member_runner.task_stream(task).await {
            Ok(mut handle) => {
                while let Some(event) = handle.recv().await {
                    let _ = events_tx.send(RoutineEvent::AgentEvent {
                        step_id: step.id,
                        event,
                    });
                }
                match handle.output().await {
                    Ok(output) => {
                        member_results.push(StepResult {
                            passed: true,
                            output: output.text,
                            step_name: format!("member-{}", i + 1),
                            input_tokens: output.input_tokens,
                            output_tokens: output.output_tokens,
                            tool_calls: output.tool_calls,
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        warn!(member_index = i, error = %e, "Member subtask failed");
                        member_results.push(StepResult {
                            passed: false,
                            output: format!("Subtask execution failed: {e}"),
                            step_name: format!("member-{}", i + 1),
                            ..Default::default()
                        });
                    }
                }
            }
            Err(e) => {
                warn!(member_index = i, error = %e, "Failed to start member subtask");
                member_results.push(StepResult {
                    passed: false,
                    output: format!("Failed to start subtask: {e}"),
                    step_name: format!("member-{}", i + 1),
                    ..Default::default()
                });
            }
        }
    }

    // Step 3: Leader aggregates
    let mut aggregation_prompt = format!(
        "You are the leader. Your team completed their subtasks. Synthesize into a final output.\n\n\
         Original task: {}\n\nMember results:\n",
        state.initial_input
    );

    for (i, result) in member_results.iter().enumerate() {
        aggregation_prompt.push_str(&format!(
            "\n--- Member {} ({}) ---\nStatus: {}\nOutput:\n{}\n",
            i + 1,
            result.step_name,
            if result.passed { "PASS" } else { "FAIL" },
            result.output
        ));
    }

    aggregation_prompt
        .push_str("\nProvide the final aggregated result. Note any gaps from failed members.");

    // Rebuild the leader with gate_verdict tool for the aggregation phase
    // so it can submit a structured verdict.
    let aggregation_leader = provider
        .agent_by_id(leader_agent_id)
        .await?
        .with_tool(gate::GateVerdictTool::new())
        .build()
        .await?;

    let aggregate_task = TaskType::Chat {
        user_message: aggregation_prompt,
        history: Vec::new(),
        project_id: state.input.project_id,
    };

    let aggregate_result = aggregation_leader.task(aggregate_task).await?;

    let verdict = gate::resolve_gate_verdict(&aggregate_result.messages, &aggregate_result.text);

    let total_input = decompose_result.input_tokens
        + member_results.iter().map(|r| r.input_tokens).sum::<u64>()
        + aggregate_result.input_tokens;
    let total_output = decompose_result.output_tokens
        + member_results.iter().map(|r| r.output_tokens).sum::<u64>()
        + aggregate_result.output_tokens;

    debug!(step_name = %step.name, passed = verdict.passed, "Council execution complete");

    Ok(StepResult {
        passed: verdict.passed,
        output: aggregate_result.text,
        data: serde_json::json!({
            "verdict": if verdict.passed { "pass" } else { "fail" },
            "reasoning": verdict.reasoning,
            "member_results": member_results.iter().map(|r| serde_json::json!({
                "step_name": r.step_name,
                "passed": r.passed,
                "output_preview": if r.output.len() > 200 {
                    format!("{}...", &r.output[..200])
                } else {
                    r.output.clone()
                }
            })).collect::<Vec<_>>()
        }),
        step_id: step.id,
        step_name: step.name.clone(),
        input_tokens: total_input,
        output_tokens: total_output,
        ..Default::default()
    })
}

/// Parse numbered subtasks from the leader's decomposition output.
fn parse_subtasks(output: &str, expected_count: usize) -> Vec<String> {
    let mut subtasks: Vec<String> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let stripped = trimmed
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches('.')
            .trim_start_matches(')')
            .trim_start_matches('-')
            .trim_start_matches('*')
            .trim();

        if !stripped.is_empty() && stripped != trimmed {
            subtasks.push(stripped.to_string());
        }
    }

    if subtasks.len() < expected_count {
        let paragraphs: Vec<String> = output
            .split("\n\n")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();

        if paragraphs.len() >= expected_count {
            return paragraphs;
        }

        if subtasks.is_empty() {
            subtasks.push(output.to_string());
        }
        while subtasks.len() < expected_count {
            subtasks.push(subtasks.last().cloned().unwrap_or_default());
        }
    }

    subtasks.truncate(expected_count);
    subtasks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numbered() {
        let output = "1. Research the API\n2. Implement endpoint\n3. Write tests";
        let subtasks = parse_subtasks(output, 3);
        assert_eq!(subtasks.len(), 3);
        assert_eq!(subtasks[0], "Research the API");
    }

    #[test]
    fn parse_fewer_than_expected() {
        let output = "1. Only one task";
        let subtasks = parse_subtasks(output, 3);
        assert_eq!(subtasks.len(), 3);
    }

    #[test]
    fn parse_truncates() {
        let output = "1. A\n2. B\n3. C\n4. D";
        let subtasks = parse_subtasks(output, 2);
        assert_eq!(subtasks.len(), 2);
    }
}
