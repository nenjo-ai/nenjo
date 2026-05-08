//! Council execution — multi-agent delegation strategies.
//!
//! A council is a group of agents with a leader and members. Supported strategies:
//! - **dynamic**: leader gets free reign to delegate via tool calls
//! - **decompose**: leader splits task → members execute → leader aggregates
//! - **broadcast**: members independently respond to the same task → leader aggregates
//! - **round_robin**: members contribute sequentially, each seeing prior outputs → leader aggregates
//! - **vote**: members cast votes/recommendations → leader tallies and submits final verdict

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::RoutineEvent;
use super::types::RoutineState;
use super::{
    apply_session_binding_memory_scope, gate, with_agent_step_tools, with_routine_step_max_turns,
};
use crate::AgentBuilder;
use crate::agents::runner::types::TurnOutput;
use crate::manifest::{CouncilDelegationStrategy, CouncilManifest, RoutineStepManifest};
use crate::provider::Provider;
use crate::routines::types::StepResult;
use crate::types::TaskType;

fn scope_tools_to_work_dir(mut builder: AgentBuilder, state: &RoutineState) -> AgentBuilder {
    if let Some(ref git) = state.input.git
        && !git.work_dir.is_empty()
    {
        builder = builder.with_work_dir(&git.work_dir);
    }
    builder
}

/// Execute a council step, dispatching based on the council's delegation_strategy.
pub(crate) async fn execute_council(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
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

    match council.delegation_strategy {
        CouncilDelegationStrategy::Dynamic => {
            execute_dynamic(provider, step, step_run_id, state, &council, events_tx).await
        }
        CouncilDelegationStrategy::Decompose => {
            execute_decompose(provider, step, step_run_id, state, &council, events_tx).await
        }
        CouncilDelegationStrategy::Broadcast => {
            execute_broadcast(provider, step, step_run_id, state, &council, events_tx).await
        }
        CouncilDelegationStrategy::RoundRobin => {
            execute_round_robin(provider, step, step_run_id, state, &council, events_tx).await
        }
        CouncilDelegationStrategy::Vote => {
            execute_vote(provider, step, step_run_id, state, &council, events_tx).await
        }
    }
}

async fn run_streamed_task(
    provider: &Provider,
    agent_id: Uuid,
    state: &RoutineState,
    step: &RoutineStepManifest,
    task: TaskType,
    step_id: Uuid,
    step_run_id: Uuid,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<TurnOutput> {
    let builder = apply_session_binding_memory_scope(
        provider.agent_by_id(agent_id).await?,
        state.input.session_binding.as_ref(),
    );
    let builder = scope_tools_to_work_dir(builder, state);
    let runner = with_agent_step_tools(with_routine_step_max_turns(builder, step))
        .build()
        .await?;

    gate::execute_with_pass_verdict(
        &runner,
        task,
        state.input.project_id,
        step_id,
        step_run_id,
        events_tx,
    )
    .await
}

fn member_agent_ids(council: &CouncilManifest) -> Result<Vec<Uuid>> {
    let ids: Vec<Uuid> = council.members.iter().map(|m| m.agent_id).collect();
    if ids.is_empty() {
        bail!("Council '{}' has no members configured", council.name);
    }
    Ok(ids)
}

async fn run_member_tasks(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    members: &[(Uuid, String)],
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<Vec<StepResult>> {
    let mut member_results = Vec::new();

    for (i, (agent_id, task_text)) in members.iter().enumerate() {
        debug!(
            member_index = i,
            agent_id = %agent_id,
            task = %task_text,
            "Executing council member task"
        );

        let task = TaskType::CouncilSubtask {
            parent_task: state.initial_input.clone(),
            subtask_description: task_text.clone(),
            subtask_index: i,
            project_id: state.input.project_id,
        };

        match run_streamed_task(
            provider,
            *agent_id,
            state,
            step,
            task,
            step.id,
            step_run_id,
            events_tx,
        )
        .await
        {
            Ok(output) => member_results.push(StepResult {
                passed: gate::resolve_pass_verdict(&output.messages)?.passed,
                output: output.text,
                step_name: format!("member-{}", i + 1),
                input_tokens: output.input_tokens,
                output_tokens: output.output_tokens,
                tool_calls: output.tool_calls,
                ..Default::default()
            }),
            Err(e) => {
                warn!(member_index = i, error = %e, "Council member task failed");
                member_results.push(StepResult {
                    passed: false,
                    output: format!("Member execution failed: {e}"),
                    step_name: format!("member-{}", i + 1),
                    ..Default::default()
                });
            }
        }
    }

    Ok(member_results)
}

struct AggregateMemberResultsParams<'a> {
    provider: &'a Provider,
    step: &'a RoutineStepManifest,
    step_run_id: Uuid,
    state: &'a RoutineState,
    council: &'a CouncilManifest,
    events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    header: &'a str,
    member_results: &'a [StepResult],
    extra_data: serde_json::Value,
}

async fn aggregate_member_results(params: AggregateMemberResultsParams<'_>) -> Result<StepResult> {
    let AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        events_tx,
        header,
        member_results,
        extra_data,
    } = params;
    let mut prompt = format!(
        "{header}\n\nOriginal task: {}\n\nMember results:\n",
        state.initial_input
    );

    for (i, result) in member_results.iter().enumerate() {
        prompt.push_str(&format!(
            "\n--- Member {} ({}) ---\nStatus: {}\nOutput:\n{}\n",
            i + 1,
            result.step_name,
            if result.passed { "PASS" } else { "FAIL" },
            result.output
        ));
    }

    prompt.push_str(
        "\nSynthesize the final result and submit a pass_verdict that reflects the council outcome.",
    );

    let aggregate_result = run_streamed_task(
        provider,
        council.leader_agent_id,
        state,
        step,
        TaskType::Chat {
            user_message: prompt,
            history: Vec::new(),
            project_id: state.input.project_id,
        },
        step.id,
        step_run_id,
        events_tx,
    )
    .await?;

    let verdict = gate::resolve_pass_verdict(&aggregate_result.messages)?;

    let total_input =
        member_results.iter().map(|r| r.input_tokens).sum::<u64>() + aggregate_result.input_tokens;
    let total_output = member_results.iter().map(|r| r.output_tokens).sum::<u64>()
        + aggregate_result.output_tokens;
    let total_tool_calls =
        member_results.iter().map(|r| r.tool_calls).sum::<u32>() + aggregate_result.tool_calls;
    let output = gate::pass_verdict_display_output(&verdict, &aggregate_result.text);

    Ok(StepResult {
        passed: verdict.passed,
        output,
        data: serde_json::json!({
            "verdict": if verdict.passed { "pass" } else { "fail" },
            "reasoning": verdict.reasoning,
            "output": verdict.output,
            "member_results": member_results.iter().map(|r| serde_json::json!({
                "step_name": r.step_name,
                "passed": r.passed,
                "output_preview": if r.output.len() > 200 {
                    format!("{}...", &r.output[..200])
                } else {
                    r.output.clone()
                }
            })).collect::<Vec<_>>(),
            "strategy_data": extra_data,
        }),
        step_id: step.id,
        step_name: step.name.clone(),
        input_tokens: total_input,
        output_tokens: total_output,
        tool_calls: total_tool_calls,
        messages: aggregate_result.messages,
    })
}

/// Dynamic: leader gets free reign to work (with delegation tools if configured).
async fn execute_dynamic(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
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

    let output = run_streamed_task(
        provider,
        leader_agent_id,
        state,
        step,
        TaskType::Chat {
            user_message: state.initial_input.clone(),
            history: Vec::new(),
            project_id: state.input.project_id,
        },
        step.id,
        step_run_id,
        events_tx,
    )
    .await?;

    info!(
        step_name = %step.name,
        "Dynamic council execution complete"
    );

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

/// Decompose: leader splits → members execute → leader aggregates.
async fn execute_decompose(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let leader_agent_id = council.leader_agent_id;
    let member_agent_ids = member_agent_ids(council)?;

    debug!(
        step_name = %step.name,
        leader = %leader_agent_id,
        members = member_agent_ids.len(),
        strategy = "decompose",
        "Starting decompose council execution"
    );

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

    let decompose_result = run_streamed_task(
        provider,
        leader_agent_id,
        state,
        step,
        TaskType::Chat {
            user_message: decompose_message,
            history: Vec::new(),
            project_id: state.input.project_id,
        },
        step.id,
        step_run_id,
        events_tx,
    )
    .await?;
    let subtasks = parse_subtasks(&decompose_result.text, member_agent_ids.len());

    debug!(parsed_subtasks = subtasks.len(), "Leader decomposed task");

    let members: Vec<(Uuid, String)> = member_agent_ids
        .iter()
        .zip(subtasks.iter())
        .map(|(agent_id, subtask)| (*agent_id, subtask.clone()))
        .collect();
    let member_results =
        run_member_tasks(provider, step, step_run_id, state, &members, events_tx).await?;

    let mut result = aggregate_member_results(AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        events_tx,
        header:
            "You are the leader. Your team completed their subtasks. Synthesize into a final output.",
        member_results: &member_results,
        extra_data: serde_json::json!({
            "decomposition": decompose_result.text,
            "strategy": "decompose",
        }),
    })
    .await?;
    result.input_tokens += decompose_result.input_tokens;
    result.output_tokens += decompose_result.output_tokens;
    result.tool_calls += decompose_result.tool_calls;
    debug!(step_name = %step.name, passed = result.passed, "Council execution complete");
    Ok(result)
}

async fn execute_broadcast(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let member_agent_ids = member_agent_ids(council)?;
    let members: Vec<(Uuid, String)> = member_agent_ids
        .iter()
        .map(|agent_id| {
            (
                *agent_id,
                format!(
                    "Provide your independent assessment of the full task.\n\nTask: {}",
                    state.initial_input
                ),
            )
        })
        .collect();
    let member_results =
        run_member_tasks(provider, step, step_run_id, state, &members, events_tx).await?;
    aggregate_member_results(AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        events_tx,
        header:
            "You are the leader. Your team independently assessed the same task. Compare the responses and synthesize the best final outcome.",
        member_results: &member_results,
        extra_data: serde_json::json!({ "strategy": "broadcast" }),
    })
    .await
}

async fn execute_round_robin(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let member_agent_ids = member_agent_ids(council)?;
    let mut running_context = String::new();
    let mut members = Vec::new();
    for (index, agent_id) in member_agent_ids.iter().enumerate() {
        let task = if running_context.is_empty() {
            format!(
                "You are contributor {} in a round-robin council. Provide the first contribution toward this task.\n\nTask: {}",
                index + 1,
                state.initial_input
            )
        } else {
            format!(
                "You are contributor {} in a round-robin council. Build on the prior council contributions without repeating them.\n\nTask: {}\n\nPrior contributions:\n{}",
                index + 1,
                state.initial_input,
                running_context
            )
        };
        let single_member = vec![(*agent_id, task)];
        let result = run_member_tasks(
            provider,
            step,
            step_run_id,
            state,
            &single_member,
            events_tx,
        )
        .await?
        .into_iter()
        .next()
        .unwrap_or_default();
        running_context.push_str(&format!(
            "\n--- Contribution {} ---\n{}\n",
            index + 1,
            result.output
        ));
        members.push(result);
    }

    aggregate_member_results(AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        events_tx,
        header:
            "You are the leader. Your team contributed in round-robin sequence. Merge their cumulative work into the final result.",
        member_results: &members,
        extra_data: serde_json::json!({
            "strategy": "round_robin",
            "contribution_chain": running_context,
        }),
    })
    .await
}

async fn execute_vote(
    provider: &Provider,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult> {
    let member_agent_ids = member_agent_ids(council)?;
    let members: Vec<(Uuid, String)> = member_agent_ids
        .iter()
        .map(|agent_id| {
            (
                *agent_id,
                format!(
                    "Review the task and cast your vote with a recommendation. State your preferred outcome, whether you believe the task should pass or fail, and your reasoning.\n\nTask: {}",
                    state.initial_input
                ),
            )
        })
        .collect();
    let member_results =
        run_member_tasks(provider, step, step_run_id, state, &members, events_tx).await?;
    aggregate_member_results(AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        events_tx,
        header:
            "You are the leader. Your team cast votes and recommendations. Tally the votes, resolve disagreement, and produce the final council decision.",
        member_results: &member_results,
        extra_data: serde_json::json!({ "strategy": "vote" }),
    })
    .await
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
