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
use super::{
    apply_session_binding_memory_scope, gate, with_agent_step_tools, with_routine_step_max_turns,
};
use crate::AgentBuilder;
use crate::Slug;
use crate::agents::runner::types::TurnOutput;
use crate::input::{AgentRun, ChatInput, ProjectLocation, TaskInput};
use crate::manifest::{
    CouncilDelegationStrategy, CouncilManifest, ProjectManifest, RoutineStepManifest,
};
use crate::provider::ProviderRuntime;
use crate::routines::types::{RoutineInput, RoutineState, SessionBinding, StepResult};

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

fn attach_location(mut run: AgentRun, state: &RoutineState) -> AgentRun {
    if let Some(git) = state.input.git.clone() {
        run.execution.project_location = Some(ProjectLocation::from_git(git));
    }
    run
}

/// Execute a council step, dispatching based on the council's delegation_strategy.
pub(crate) async fn execute_council<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let invocation = CouncilInvocation::Task;
    execute_council_with_invocation(provider, step, step_run_id, state, &invocation, events_tx)
        .await
}

/// Execute a council directly from a chat turn, reusing the same strategy
/// implementation used by routine council steps.
pub async fn execute_council_chat<P>(
    provider: &P,
    council: Slug,
    project: Option<Slug>,
    message: String,
    session_id: Uuid,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let step = RoutineStepManifest::builder()
        .with_slug(Slug::derive("council_chat"))
        .with_routine(Slug::derive("council_chat"))
        .with_name("Council Chat")
        .with_step_type(crate::manifest::RoutineStepType::Council)
        .with_council(council)
        .build()?;
    let mut input =
        RoutineInput::new("Council chat", message).with_session_binding(SessionBinding {
            session_id,
            memory_namespace: None,
        });
    if let Some(project_manifest) = project_manifest_for_slug(provider, project.as_ref()) {
        input = input.with_project_context(&project_manifest);
    }
    let state = RoutineState::new(input);
    let invocation = CouncilInvocation::Chat {
        history: Vec::new(),
    };
    let step_run_id = Uuid::new_v4();
    let _ = events_tx.send(RoutineEvent::StepStarted {
        step_id: step.id,
        step_run_id,
        step_name: step.name.clone(),
        step_type: "council".to_string(),
        agent_id: None,
    });
    let started = std::time::Instant::now();
    match execute_council_with_invocation(
        provider,
        &step,
        step_run_id,
        &state,
        &invocation,
        events_tx,
    )
    .await
    {
        Ok(result) => {
            let _ = events_tx.send(RoutineEvent::StepCompleted {
                step_id: step.id,
                step_run_id,
                result: result.clone(),
                duration_ms: started.elapsed().as_millis() as u64,
            });
            Ok(result)
        }
        Err(error) => {
            let _ = events_tx.send(RoutineEvent::StepFailed {
                step_id: step.id,
                step_run_id,
                error: error.to_string(),
                duration_ms: started.elapsed().as_millis() as u64,
            });
            Err(error)
        }
    }
}

fn project_manifest_for_slug<P>(provider: &P, project: Option<&Slug>) -> Option<ProjectManifest>
where
    P: ProviderRuntime,
{
    let project = project?;
    provider.find_project(project).cloned()
}

async fn execute_council_with_invocation<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    invocation: &CouncilInvocation,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let council_slug = step
        .council
        .as_ref()
        .context("Council step missing council")?;
    let manifest = provider.manifest_snapshot();

    let council = manifest
        .councils
        .iter()
        .find(|c| Slug::derive(&c.name) == *council_slug)
        .with_context(|| format!("Council {council_slug} not found in manifest"))?
        .clone();

    match council.delegation_strategy {
        CouncilDelegationStrategy::Dynamic => {
            execute_dynamic(
                provider,
                step,
                step_run_id,
                state,
                &council,
                invocation,
                events_tx,
            )
            .await
        }
        CouncilDelegationStrategy::Decompose => {
            execute_decompose(
                provider,
                step,
                step_run_id,
                state,
                &council,
                invocation,
                events_tx,
            )
            .await
        }
        CouncilDelegationStrategy::Broadcast => {
            execute_broadcast(
                provider,
                step,
                step_run_id,
                state,
                &council,
                invocation,
                events_tx,
            )
            .await
        }
        CouncilDelegationStrategy::RoundRobin => {
            execute_round_robin(
                provider,
                step,
                step_run_id,
                state,
                &council,
                invocation,
                events_tx,
            )
            .await
        }
        CouncilDelegationStrategy::Vote => {
            execute_vote(
                provider,
                step,
                step_run_id,
                state,
                &council,
                invocation,
                events_tx,
            )
            .await
        }
    }
}

#[derive(Debug, Clone)]
enum CouncilInvocation {
    Chat {
        history: Vec<nenjo_models::ChatMessage>,
    },
    Task,
}

impl CouncilInvocation {
    fn requires_pass_verdict(&self) -> bool {
        match self {
            CouncilInvocation::Chat { .. } => false,
            CouncilInvocation::Task => true,
        }
    }

    fn run_for_instruction(
        &self,
        state: &RoutineState,
        instruction: impl Into<String>,
    ) -> AgentRun {
        match self {
            CouncilInvocation::Chat { history } => AgentRun::chat(ChatInput {
                message: instruction.into(),
                history: history.clone(),
                project: state.input.project.clone(),
            }),
            CouncilInvocation::Task => {
                AgentRun::task(task_input_for_instruction(state, instruction.into()))
            }
        }
    }
}

#[derive(Debug, Clone)]
struct CouncilAssignment {
    agent: Slug,
    instruction: String,
}

fn task_input_for_instruction(state: &RoutineState, description: String) -> TaskInput {
    TaskInput {
        task_id: state.input.task_id.unwrap_or_else(Uuid::nil),
        title: state.input.title.clone(),
        description,
        acceptance_criteria: state.input.acceptance_criteria.clone(),
        tags: state.input.tags.clone(),
        source: state
            .input
            .source
            .clone()
            .or_else(|| Some("routine".to_string())),
        project: state.input.project.clone(),
        status: state.input.status.clone(),
        priority: state.input.priority.clone(),
        task_type: state.input.task_type.clone(),
        slug: state.input.slug.clone(),
        complexity: state.input.complexity.clone(),
    }
}

struct StreamedTaskParams<'a, P> {
    provider: &'a P,
    agent: Slug,
    state: &'a RoutineState,
    step: &'a RoutineStepManifest,
    invocation: &'a CouncilInvocation,
    task: AgentRun,
    step_run_id: Uuid,
    events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
}

async fn run_streamed_task<P>(params: StreamedTaskParams<'_, P>) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let StreamedTaskParams {
        provider,
        agent,
        state,
        step,
        invocation,
        task,
        step_run_id,
        events_tx,
    } = params;

    let mut builder = apply_session_binding_memory_scope(
        provider.agent(&agent).await?,
        state.input.session_binding.as_ref(),
    );
    if let Some(project_manifest) =
        project_manifest_for_slug(provider, state.input.project.as_ref())
    {
        builder = builder.with_project_context(&project_manifest);
    }
    let builder = scope_tools_to_work_dir(builder, state);
    let builder = with_routine_step_max_turns(builder, step);
    let builder = if invocation.requires_pass_verdict() {
        with_agent_step_tools(builder)
    } else {
        builder
    };
    let runner = builder.build().await?;

    if invocation.requires_pass_verdict() {
        gate::execute_with_pass_verdict(
            &runner,
            task,
            state.input.project.clone(),
            step.id,
            step_run_id,
            events_tx,
        )
        .await
    } else {
        let mut handle = runner.run_stream(task).await?;
        while let Some(event) = handle.recv().await {
            let _ = events_tx.send(RoutineEvent::AgentEvent {
                step_id: step.id,
                step_run_id,
                event,
            });
        }
        handle.output().await
    }
}

fn member_agents(council: &CouncilManifest) -> Result<Vec<Slug>> {
    let agents: Vec<Slug> = council.members.iter().map(|m| m.agent.clone()).collect();
    if agents.is_empty() {
        bail!("Council '{}' has no members configured", council.name);
    }
    Ok(agents)
}

async fn run_member_tasks<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    invocation: &CouncilInvocation,
    assignments: &[CouncilAssignment],
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<Vec<StepResult>>
where
    P: ProviderRuntime,
{
    let mut member_results = Vec::new();

    for (i, assignment) in assignments.iter().enumerate() {
        debug!(
            member_index = i,
            agent = %assignment.agent,
            task = %assignment.instruction,
            "Executing council member task"
        );

        let task = attach_location(
            invocation.run_for_instruction(state, assignment.instruction.clone()),
            state,
        );

        match run_streamed_task(StreamedTaskParams {
            provider,
            agent: assignment.agent.clone(),
            state,
            step,
            invocation,
            task,
            step_run_id,
            events_tx,
        })
        .await
        {
            Ok(output) if invocation.requires_pass_verdict() => {
                let verdict = gate::resolve_pass_verdict(&output.messages)?;
                member_results.push(StepResult {
                    task_id: output.task_id.or(state.input.task_id),
                    passed: verdict.passed,
                    output: gate::pass_verdict_display_output(&verdict, &output.text),
                    step_name: format!("member-{}", i + 1),
                    input_tokens: output.input_tokens,
                    output_tokens: output.output_tokens,
                    tool_calls: output.tool_calls,
                    ..Default::default()
                })
            }
            Ok(output) => member_results.push(StepResult {
                task_id: output.task_id.or(state.input.task_id),
                passed: true,
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

struct AggregateMemberResultsParams<'a, P> {
    provider: &'a P,
    step: &'a RoutineStepManifest,
    step_run_id: Uuid,
    state: &'a RoutineState,
    council: &'a CouncilManifest,
    invocation: &'a CouncilInvocation,
    events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    header: &'a str,
    member_results: &'a [StepResult],
    extra_data: serde_json::Value,
}

async fn aggregate_member_results<P>(
    params: AggregateMemberResultsParams<'_, P>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        invocation,
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

    if invocation.requires_pass_verdict() {
        prompt.push_str(
            "\nSynthesize the final result and submit a pass_verdict that reflects the council outcome.",
        );
    } else {
        prompt.push_str("\nSynthesize the final response for the user.");
    }

    let aggregate_result = run_streamed_task(StreamedTaskParams {
        provider,
        agent: council.leader_agent.clone(),
        state,
        step,
        invocation,
        task: attach_location(invocation.run_for_instruction(state, prompt), state),
        step_run_id,
        events_tx,
    })
    .await?;

    let total_input =
        member_results.iter().map(|r| r.input_tokens).sum::<u64>() + aggregate_result.input_tokens;
    let total_output = member_results.iter().map(|r| r.output_tokens).sum::<u64>()
        + aggregate_result.output_tokens;
    let total_tool_calls =
        member_results.iter().map(|r| r.tool_calls).sum::<u32>() + aggregate_result.tool_calls;
    if !invocation.requires_pass_verdict() {
        return Ok(StepResult {
            task_id: aggregate_result.task_id.or(state.input.task_id),
            passed: true,
            output: aggregate_result.text,
            data: serde_json::json!({
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
        });
    }

    let verdict = gate::resolve_pass_verdict(&aggregate_result.messages)?;
    let output = gate::pass_verdict_display_output(&verdict, &aggregate_result.text);

    Ok(StepResult {
        task_id: aggregate_result.task_id.or(state.input.task_id),
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
async fn execute_dynamic<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    invocation: &CouncilInvocation,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let leader_agent = council.leader_agent.clone();

    info!(
        step_name = %step.name,
        leader = %leader_agent,
        strategy = "dynamic",
        "Starting dynamic council execution"
    );

    let output = run_streamed_task(StreamedTaskParams {
        provider,
        agent: leader_agent,
        state,
        step,
        invocation,
        task: attach_location(
            invocation.run_for_instruction(state, state.initial_input.clone()),
            state,
        ),
        step_run_id,
        events_tx,
    })
    .await?;

    info!(
        step_name = %step.name,
        "Dynamic council execution complete"
    );

    if !invocation.requires_pass_verdict() {
        return Ok(StepResult {
            task_id: output.task_id.or(state.input.task_id),
            passed: true,
            output: output.text,
            data: serde_json::json!({ "strategy": "dynamic" }),
            step_id: step.id,
            step_name: step.name.clone(),
            input_tokens: output.input_tokens,
            output_tokens: output.output_tokens,
            tool_calls: output.tool_calls,
            messages: output.messages,
        });
    }

    let verdict = gate::resolve_pass_verdict(&output.messages)?;
    let step_output = gate::pass_verdict_display_output(&verdict, &output.text);
    let data = serde_json::json!({
        "verdict": if verdict.passed { "pass" } else { "fail" },
        "reasoning": verdict.reasoning,
        "output": verdict.output,
        "strategy": "dynamic",
    });

    Ok(StepResult {
        task_id: output.task_id.or(state.input.task_id),
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
async fn execute_decompose<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    invocation: &CouncilInvocation,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let leader_agent = council.leader_agent.clone();
    let member_agents = member_agents(council)?;

    debug!(
        step_name = %step.name,
        leader = %leader_agent,
        members = member_agents.len(),
        strategy = "decompose",
        "Starting decompose council execution"
    );

    let decompose_message = format!(
        "You are the leader of a team of {} agents. Decompose the following work \
         into exactly {} assignments, one for each team member.\n\n\
         Task: {}\n\n\
         Respond with a numbered list:\n\
         1. [assignment description]\n\
         2. [assignment description]\n\
         ...",
        member_agents.len(),
        member_agents.len(),
        state.initial_input
    );

    let decompose_result = run_streamed_task(StreamedTaskParams {
        provider,
        agent: leader_agent,
        state,
        step,
        invocation,
        task: attach_location(
            invocation.run_for_instruction(state, decompose_message),
            state,
        ),
        step_run_id,
        events_tx,
    })
    .await?;
    let parsed_assignments = parse_assignments(&decompose_result.text, member_agents.len());

    debug!(
        parsed_assignments = parsed_assignments.len(),
        "Leader decomposed task"
    );

    let assignments: Vec<CouncilAssignment> = member_agents
        .iter()
        .zip(parsed_assignments.iter())
        .map(|(agent, parsed_assignment)| CouncilAssignment {
            agent: agent.clone(),
            instruction: parsed_assignment.clone(),
        })
        .collect();
    let member_results = run_member_tasks(
        provider,
        step,
        step_run_id,
        state,
        invocation,
        &assignments,
        events_tx,
    )
    .await?;

    let mut result = aggregate_member_results(AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        invocation,
        events_tx,
        header:
            "You are the leader. Your team completed their assignments. Synthesize into a final output.",
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

async fn execute_broadcast<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    invocation: &CouncilInvocation,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let member_agents = member_agents(council)?;
    let assignments: Vec<CouncilAssignment> = member_agents
        .iter()
        .map(|agent| CouncilAssignment {
            agent: agent.clone(),
            instruction: format!(
                "Provide your independent assessment of the full task.\n\nTask: {}",
                state.initial_input
            ),
        })
        .collect();
    let member_results = run_member_tasks(
        provider,
        step,
        step_run_id,
        state,
        invocation,
        &assignments,
        events_tx,
    )
    .await?;
    aggregate_member_results(AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        invocation,
        events_tx,
        header:
            "You are the leader. Your team independently assessed the same task. Compare the responses and synthesize the best final outcome.",
        member_results: &member_results,
        extra_data: serde_json::json!({ "strategy": "broadcast" }),
    })
    .await
}

async fn execute_round_robin<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    invocation: &CouncilInvocation,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let member_agents = member_agents(council)?;
    let mut running_context = String::new();
    let mut members = Vec::new();
    for (index, agent) in member_agents.iter().enumerate() {
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
        let single_member = vec![CouncilAssignment {
            agent: agent.clone(),
            instruction: task,
        }];
        let result = run_member_tasks(
            provider,
            step,
            step_run_id,
            state,
            invocation,
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
        invocation,
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

async fn execute_vote<P>(
    provider: &P,
    step: &RoutineStepManifest,
    step_run_id: Uuid,
    state: &RoutineState,
    council: &CouncilManifest,
    invocation: &CouncilInvocation,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let member_agents = member_agents(council)?;
    let assignments: Vec<CouncilAssignment> = member_agents
        .iter()
        .map(|agent| CouncilAssignment {
            agent: agent.clone(),
            instruction: format!(
                "Review the task and cast your vote with a recommendation. State your preferred outcome, whether you believe the task should pass or fail, and your reasoning.\n\nTask: {}",
                state.initial_input
            ),
        })
        .collect();
    let member_results = run_member_tasks(
        provider,
        step,
        step_run_id,
        state,
        invocation,
        &assignments,
        events_tx,
    )
    .await?;
    aggregate_member_results(AggregateMemberResultsParams {
        provider,
        step,
        step_run_id,
        state,
        council,
        invocation,
        events_tx,
        header:
            "You are the leader. Your team cast votes and recommendations. Tally the votes, resolve disagreement, and produce the final council decision.",
        member_results: &member_results,
        extra_data: serde_json::json!({ "strategy": "vote" }),
    })
    .await
}

/// Parse numbered assignments from the leader's decomposition output.
fn parse_assignments(output: &str, expected_count: usize) -> Vec<String> {
    let mut assignments: Vec<String> = Vec::new();

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
            assignments.push(stripped.to_string());
        }
    }

    if assignments.len() < expected_count {
        let paragraphs: Vec<String> = output
            .split("\n\n")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();

        if paragraphs.len() >= expected_count {
            return paragraphs;
        }

        if assignments.is_empty() {
            assignments.push(output.to_string());
        }
        while assignments.len() < expected_count {
            assignments.push(assignments.last().cloned().unwrap_or_default());
        }
    }

    assignments.truncate(expected_count);
    assignments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::AgentRunKind;

    fn test_state() -> RoutineState {
        RoutineState::new(
            RoutineInput::new("Parent task", "Parent description")
                .with_task_id(Uuid::nil())
                .with_acceptance_criteria(Some("Meets criteria".to_string()))
                .with_tags(vec!["tag".to_string()])
                .with_slug("parent-task")
                .with_status("todo")
                .with_priority("high")
                .with_task_type("feature")
                .with_complexity("medium")
                .with_source("test"),
        )
    }

    #[test]
    fn chat_invocation_builds_chat_run() {
        let state = test_state();
        let invocation = CouncilInvocation::Chat {
            history: Vec::new(),
        };

        let run = invocation.run_for_instruction(&state, "Answer the user");

        match run.kind {
            AgentRunKind::Chat(chat) => {
                assert_eq!(chat.message, "Answer the user");
                assert_eq!(chat.project, state.input.project);
            }
            other => panic!("expected chat run, got {other:?}"),
        }
    }

    #[test]
    fn task_invocation_builds_task_run() {
        let state = test_state();
        let invocation = CouncilInvocation::Task;

        let run = invocation.run_for_instruction(&state, "Complete assignment");

        match run.kind {
            AgentRunKind::Task(task) => {
                assert_eq!(task.description, "Complete assignment");
                assert_eq!(task.title, "Parent task");
                assert_eq!(task.acceptance_criteria.as_deref(), Some("Meets criteria"));
            }
            other => panic!("expected task run, got {other:?}"),
        }
    }

    #[test]
    fn parse_numbered() {
        let output = "1. Research the API\n2. Implement endpoint\n3. Write tests";
        let assignments = parse_assignments(output, 3);
        assert_eq!(assignments.len(), 3);
        assert_eq!(assignments[0], "Research the API");
    }

    #[test]
    fn parse_fewer_than_expected() {
        let output = "1. Only one task";
        let assignments = parse_assignments(output, 3);
        assert_eq!(assignments.len(), 3);
    }

    #[test]
    fn parse_truncates() {
        let output = "1. A\n2. B\n3. C\n4. D";
        let assignments = parse_assignments(output, 2);
        assert_eq!(assignments.len(), 2);
    }
}
