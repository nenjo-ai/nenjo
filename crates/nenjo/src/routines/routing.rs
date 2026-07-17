//! Routine terminal routing tool.
//!
//! Routable routine steps use `route_next_steps` as the canonical terminal contract.
//! The tool records the step verdict and the per-edge downstream handoff
//! payloads used for deterministic fan-out and audit trails.

use anyhow::{Context, Result, bail};
use nenjo_models::ChatMessage;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use super::RoutineEvent;
use super::handoff_schema::{compact_schema, edge_handoff_schema, validate_handoff_payload};
use crate::agents::runner::{AgentRunner, types::TurnOutput};
use crate::input::{AgentRun, FollowUpInput};
use crate::manifest::{RoutineEdgeCondition, RoutineEdgeManifest, RoutineStepManifest};
use crate::provider::ProviderRuntime;
use crate::tools::{Tool, ToolCategory, ToolResult};

pub const ROUTE_NEXT_STEPS_TOOL_NAME: &str = "route_next_steps";
const ROUTE_NEXT_STEPS_RETRY_LIMIT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingStepKind {
    Agent,
    Gate,
}

enum RouteRetryReason {
    InvalidRoute(String),
    MissingAfterProgress,
    MissingNoProgress,
}

impl RoutingStepKind {
    fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Gate => "gate",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouteOption {
    pub target_step: crate::Slug,
    pub target_name: String,
    pub condition: RoutineEdgeCondition,
    pub purpose: String,
    pub handoff_instructions: String,
    pub handoff_schema: Value,
}

impl RouteOption {
    fn from_edge(edge: &RoutineEdgeManifest, target: Option<&RoutineStepManifest>) -> Result<Self> {
        let handoff_schema = edge_handoff_schema(&edge.metadata).with_context(|| {
            format!(
                "edge {}:{} must define a valid metadata.handoff_schema",
                edge.source_step, edge.target_step
            )
        })?;
        Ok(Self {
            target_step: edge.target_step.clone(),
            target_name: target
                .map(|step| step.name.clone())
                .unwrap_or_else(|| edge.target_step.to_string()),
            condition: edge.condition,
            purpose: edge_purpose(edge),
            handoff_instructions: edge_handoff_instructions(edge),
            handoff_schema: handoff_schema.clone(),
        })
    }
}

pub struct RouteNextStepsTool {
    routes: Vec<RouteOption>,
    step_kind: RoutingStepKind,
    description: String,
}

impl RouteNextStepsTool {
    /// Build a step-scoped route tool whose schema only accepts this step's
    /// actual outgoing edge targets.
    pub fn new(
        edges: &[RoutineEdgeManifest],
        steps: &[RoutineStepManifest],
        step_kind: RoutingStepKind,
    ) -> Result<Self> {
        let routes = edges
            .iter()
            .map(|edge| {
                let target = steps.iter().find(|step| step.slug == edge.target_step);
                RouteOption::from_edge(edge, target)
            })
            .collect::<Result<Vec<_>>>()?;
        let description = build_description(step_kind, &routes);
        Ok(Self {
            routes,
            step_kind,
            description,
        })
    }
}

#[async_trait::async_trait]
impl Tool for RouteNextStepsTool {
    fn name(&self) -> &str {
        ROUTE_NEXT_STEPS_TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        let route_items = self
            .routes
            .iter()
            .map(|route| {
                serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "target_step": {
                            "type": "string",
                            "const": route.target_step.to_string(),
                            "description": "The downstream routine step slug for this decomposed task."
                        },
                        "handoff": route.handoff_schema.clone(),
                        "summary": {
                            "type": "string",
                            "description": "Optional short label for logs or UI."
                        }
                    },
                    "required": ["target_step", "handoff"]
                })
            })
            .collect::<Vec<_>>();
        let (min_routes, max_routes) = activated_route_count_range(self.step_kind, &self.routes);

        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "next_steps": {
                    "type": "array",
                    "minItems": min_routes,
                    "maxItems": max_routes,
                    "description": next_steps_schema_description(self.step_kind),
                    "items": {"oneOf": route_items}
                },
                "verdict": {
                    "type": "string",
                    "enum": ["pass", "fail"],
                    "description": verdict_schema_description(self.step_kind)
                },
                "reasoning": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Short explanation of why this step passed and was routed, or why it failed."
                },
                "output": {
                    "type": "string",
                    "description": "Final response text for this agent step."
                },
                "summary": {
                    "type": "string",
                    "description": "Brief explanation of how the work was decomposed across all outgoing edges."
                }
            },
            "required": ["verdict", "reasoning", "output"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn is_terminal(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        validate_route_args(self.step_kind, &self.routes, &args)?;
        let verdict = args
            .get("verdict")
            .and_then(|value| value.as_str())
            .unwrap_or("fail");
        Ok(ToolResult {
            success: true,
            output: match verdict {
                "pass" => format!(
                    "{} routing task(s) recorded for downstream routine steps.",
                    activated_routes(self.step_kind, &self.routes, verdict).len()
                ),
                _ => match activated_routes(self.step_kind, &self.routes, verdict).len() {
                    0 => format!(
                        "{} step failure verdict recorded; downstream routing will not run.",
                        self.step_kind.label()
                    ),
                    count => format!(
                        "{} routing task(s) recorded for downstream routine steps.",
                        count
                    ),
                },
            },
            error: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RouteNextStepsDecision {
    pub passed: bool,
    pub reasoning: Option<String>,
    pub output: Option<String>,
    pub next_steps: Option<Value>,
    pub arguments: Value,
}

/// Extract the raw `route_next_steps` tool arguments from assistant messages.
///
/// The most recent assistant message containing the tool must contain exactly
/// one call. Earlier invalid attempts may remain in history after a corrective
/// retry, but the terminal turn is always single-call.
pub fn extract_route_next_steps(messages: &[ChatMessage]) -> Result<Option<Value>> {
    for msg in messages.iter().rev() {
        if msg.role != "assistant" {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(&msg.content) else {
            continue;
        };
        let Some(tool_calls) = parsed.get("tool_calls").and_then(|value| value.as_array()) else {
            continue;
        };

        let matching = tool_calls
            .iter()
            .filter(|call| {
                call.get("name")
                    .and_then(|value| value.as_str())
                    .is_some_and(|name| name == ROUTE_NEXT_STEPS_TOOL_NAME)
            })
            .collect::<Vec<_>>();
        if matching.is_empty() {
            continue;
        }
        if matching.len() > 1 {
            bail!("Agent called {ROUTE_NEXT_STEPS_TOOL_NAME} more than once in the final turn");
        }
        let args = matching[0]
            .get("arguments")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("{ROUTE_NEXT_STEPS_TOOL_NAME} arguments must be a JSON string")
            })?;
        let value = serde_json::from_str::<Value>(args).with_context(|| {
            format!("{ROUTE_NEXT_STEPS_TOOL_NAME} arguments must parse as JSON")
        })?;
        return Ok(Some(value));
    }
    Ok(None)
}

/// Resolve the required agent-step routing decision from the transcript.
///
/// This parses the terminal `route_next_steps` call into the verdict fields used
/// by the task runtime and step result audit data.
pub fn resolve_route_next_steps(messages: &[ChatMessage]) -> Result<RouteNextStepsDecision> {
    let Some(arguments) = extract_route_next_steps(messages)? else {
        bail!("Routine step did not call required route_next_steps tool");
    };
    let Some(verdict) = arguments.get("verdict").and_then(|value| value.as_str()) else {
        bail!("route_next_steps did not include verdict");
    };
    let passed = match verdict {
        "pass" => true,
        "fail" => false,
        _ => bail!("route_next_steps verdict must be pass or fail"),
    };
    Ok(RouteNextStepsDecision {
        passed,
        reasoning: arguments
            .get("reasoning")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
        output: arguments
            .get("output")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
        next_steps: arguments.get("next_steps").cloned(),
        arguments,
    })
}

/// Choose the user-visible step output from a route decision.
pub fn route_next_steps_display_output(
    decision: &RouteNextStepsDecision,
    fallback: &str,
) -> String {
    decision
        .output
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            decision
                .reasoning
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Construct the runtime tool injected into an agent routine step.
pub fn route_next_steps_tool(
    edges: &[RoutineEdgeManifest],
    steps: &[RoutineStepManifest],
    step_kind: RoutingStepKind,
) -> Result<RouteNextStepsTool> {
    RouteNextStepsTool::new(edges, steps, step_kind)
}

pub fn validate_route_next_steps_call(
    edges: &[RoutineEdgeManifest],
    steps: &[RoutineStepManifest],
    step_kind: RoutingStepKind,
    args: &Value,
) -> Result<()> {
    let routes = edges
        .iter()
        .map(|edge| {
            let target = steps.iter().find(|step| step.slug == edge.target_step);
            RouteOption::from_edge(edge, target)
        })
        .collect::<Result<Vec<_>>>()?;
    validate_route_args(step_kind, &routes, args)
}

fn validate_route_args(
    step_kind: RoutingStepKind,
    routes: &[RouteOption],
    args: &Value,
) -> Result<()> {
    let Some(args_object) = args.as_object() else {
        bail!("route_next_steps arguments must be an object");
    };
    for key in args_object.keys() {
        if !matches!(
            key.as_str(),
            "next_steps" | "verdict" | "reasoning" | "output" | "summary"
        ) {
            bail!("route_next_steps argument '{key}' is not supported");
        }
    }

    let Some(verdict) = args.get("verdict").and_then(|value| value.as_str()) else {
        bail!("route_next_steps requires verdict");
    };
    if !matches!(verdict, "pass" | "fail") {
        bail!("route_next_steps verdict must be pass or fail");
    }
    let has_reasoning = args
        .get("reasoning")
        .and_then(|value| value.as_str())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !has_reasoning {
        bail!("route_next_steps requires reasoning");
    }
    if !args.get("output").is_some_and(Value::is_string) {
        bail!("route_next_steps requires output");
    }

    let activated = activated_routes(step_kind, routes, verdict);
    if activated.is_empty() {
        if args.get("next_steps").is_some() {
            bail!(
                "route_next_steps verdict '{verdict}' must not include next_steps for this {} step",
                step_kind.label()
            );
        }
        return Ok(());
    }

    let routes_by_target = activated
        .iter()
        .map(|route| (route.target_step.to_string(), route))
        .collect::<std::collections::HashMap<_, _>>();
    let Some(next_steps) = args.get("next_steps").and_then(|value| value.as_array()) else {
        bail!("route_next_steps requires next_steps");
    };
    if next_steps.len() != routes_by_target.len() {
        bail!(
            "route_next_steps requires exactly {} next_steps item(s)",
            routes_by_target.len()
        );
    }

    let mut seen = std::collections::HashSet::new();
    for next in next_steps {
        let Some(next_object) = next.as_object() else {
            bail!("route_next_steps next_steps item must be an object");
        };
        for key in next_object.keys() {
            if !matches!(key.as_str(), "target_step" | "handoff" | "summary") {
                bail!("route_next_steps next_steps item field '{key}' is not supported");
            }
        }
        let Some(target) = next.get("target_step").and_then(|value| value.as_str()) else {
            bail!("route_next_steps next_steps item is missing target_step");
        };
        let Some(route) = routes_by_target.get(target) else {
            if routes
                .iter()
                .any(|route| route.target_step.to_string() == target)
            {
                bail!(
                    "route_next_steps target_step '{target}' is not activated by verdict '{verdict}'"
                );
            }
            bail!("route_next_steps target_step '{target}' is not an outgoing routine edge");
        };
        if !seen.insert(target.to_string()) {
            bail!("route_next_steps target_step '{target}' was submitted more than once");
        }
        let Some(handoff) = next.get("handoff") else {
            bail!("route_next_steps target_step '{target}' is missing handoff");
        };
        validate_handoff_payload(&route.handoff_schema, handoff).with_context(|| {
            format!("route_next_steps target_step '{target}' handoff schema validation failed")
        })?;
    }

    Ok(())
}

fn activated_routes<'a>(
    step_kind: RoutingStepKind,
    routes: &'a [RouteOption],
    verdict: &str,
) -> Vec<&'a RouteOption> {
    routes
        .iter()
        .filter(|route| route_is_activated(step_kind, route, verdict))
        .collect()
}

fn route_is_activated(step_kind: RoutingStepKind, route: &RouteOption, verdict: &str) -> bool {
    match step_kind {
        RoutingStepKind::Agent => {
            verdict == "pass"
                && matches!(
                    route.condition,
                    RoutineEdgeCondition::Always | RoutineEdgeCondition::OnPass
                )
        }
        RoutingStepKind::Gate => match verdict {
            "pass" => route.condition == RoutineEdgeCondition::OnPass,
            "fail" => route.condition == RoutineEdgeCondition::OnFail,
            _ => false,
        },
    }
}

fn activated_route_count_range(
    step_kind: RoutingStepKind,
    routes: &[RouteOption],
) -> (usize, usize) {
    let counts = ["pass", "fail"]
        .into_iter()
        .map(|verdict| activated_routes(step_kind, routes, verdict).len())
        .filter(|count| *count > 0)
        .collect::<Vec<_>>();
    let min = counts.iter().copied().min().unwrap_or(0);
    let max = counts.iter().copied().max().unwrap_or(0);
    (min, max)
}

fn next_steps_schema_description(step_kind: RoutingStepKind) -> &'static str {
    match step_kind {
        RoutingStepKind::Agent => {
            "Required when verdict is pass. Include exactly one item for every activated outgoing routine edge, with each target exactly once."
        }
        RoutingStepKind::Gate => {
            "Required when the submitted verdict activates downstream routes. Include exactly one item for each edge matching the verdict, with each target exactly once."
        }
    }
}

fn verdict_schema_description(step_kind: RoutingStepKind) -> &'static str {
    match step_kind {
        RoutingStepKind::Agent => {
            "Final verdict for this agent step: pass to route downstream tasks, fail to stop the routine."
        }
        RoutingStepKind::Gate => {
            "Final verdict for this gate step: pass routes on_pass edges, fail routes on_fail edges."
        }
    }
}

fn build_description(step_kind: RoutingStepKind, routes: &[RouteOption]) -> String {
    let mut description = match step_kind {
        RoutingStepKind::Agent => format!(
            "Submit the final result for this agent routine step. Use verdict=\"fail\" when the step should fail and downstream routing must stop. Use verdict=\"pass\" when the step completed successfully; in that case, include exactly one next_steps item for every activated outgoing routine edge. Each next_steps.handoff must be the concrete payload for that downstream step: actual data, evidence, artifacts, decisions, or a specific work item. Do not put route instructions, summaries of what should happen, or a restatement of handoff_instructions in handoff. The runtime will fan out to downstream steps only after this tool records a pass verdict with all {} activated edge handoffs.",
            activated_routes(step_kind, routes, "pass").len()
        ),
        RoutingStepKind::Gate => {
            "Submit the final result for this gate routine step. Use verdict=\"pass\" when the previous result satisfies the gate criteria and route only the on_pass targets. Use verdict=\"fail\" when the previous result does not satisfy the criteria and route only the on_fail targets. For any verdict that activates routes, include exactly one next_steps item for every matching edge. Each next_steps.handoff must be the concrete payload for that downstream step: actual data, evidence, artifacts, decisions, or a specific work item. Do not put route instructions, summaries of what should happen, or a restatement of handoff_instructions in handoff.".to_string()
        }
    };
    for route in routes {
        description.push_str(&format!(
            "\n- target_step={} target_name=\"{}\" condition={} purpose=\"{}\" handoff_instructions=\"{}\" handoff_schema={}",
            route.target_step,
            route.target_name,
            condition_label(route.condition),
            route.purpose,
            route.handoff_instructions,
            compact_schema(&route.handoff_schema)
        ));
    }
    description
}

fn route_retry_prompt(
    step_kind: RoutingStepKind,
    previous_text: &str,
    reason: &RouteRetryReason,
) -> String {
    let previous_response = if previous_text.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nPrevious response:\n{previous_text}")
    };

    match reason {
        RouteRetryReason::InvalidRoute(error) => format!(
            "Your previous `{}` call was invalid: {} Correct the `{}` call exactly once now as your final action. {}{}",
            ROUTE_NEXT_STEPS_TOOL_NAME,
            error,
            ROUTE_NEXT_STEPS_TOOL_NAME,
            route_retry_instruction(step_kind),
            previous_response
        ),
        RouteRetryReason::MissingAfterProgress | RouteRetryReason::MissingNoProgress => format!(
            "Your previous response did not complete the routine step because it did not call `{}`. \
             If the step work is complete, call `{}` exactly once now as your final action. \
             If work remains, continue executing the step using the available tools. \
             Do not restate intent or provide preamble. {}{}",
            ROUTE_NEXT_STEPS_TOOL_NAME,
            ROUTE_NEXT_STEPS_TOOL_NAME,
            route_retry_instruction(step_kind),
            previous_response
        ),
    }
}

fn route_retry_instruction(step_kind: RoutingStepKind) -> &'static str {
    match step_kind {
        RoutingStepKind::Agent => {
            "Use verdict=\"pass\" with a handoff in next_steps for every downstream route, or verdict=\"fail\" with reasoning and output if the step failed."
        }
        RoutingStepKind::Gate => {
            "Use verdict=\"pass\" with handoffs for on_pass routes, or verdict=\"fail\" with handoffs for on_fail routes when they exist."
        }
    }
}

fn chat_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .filter(|message| message.role != "system" && message.role != "developer")
        .cloned()
        .collect()
}

async fn stream_turn_output<P>(
    runner: &AgentRunner<P>,
    task: AgentRun,
    step_slug: crate::Slug,
    step_run_id: Uuid,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
    cancel: &CancellationToken,
) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let mut handle = runner.run_stream(task).await?;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                handle.cancel();
                anyhow::bail!("routine cancelled");
            }
            event = handle.recv() => {
                let Some(event) = event else {
                    break;
                };
                let _ = events_tx.send(RoutineEvent::AgentEvent {
                    step_slug: step_slug.clone(),
                    step_run_id,
                    event,
                });
            }
        }
    }
    handle.output().await
}

/// Run a routable step and require a terminal `route_next_steps` call.
///
/// If the model omits the tool, the harness sends a corrective follow-up turn
/// rather than accepting free-form text as a routine step result.
pub struct ExecuteRouteNextStepsParams<'a, P>
where
    P: ProviderRuntime,
{
    pub runner: &'a AgentRunner<P>,
    pub task: AgentRun,
    pub project: Option<crate::Slug>,
    pub step_slug: crate::Slug,
    pub step_run_id: Uuid,
    pub step_kind: RoutingStepKind,
    pub route_edges: &'a [RoutineEdgeManifest],
    pub routine_steps: &'a [RoutineStepManifest],
    pub events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    pub cancel: &'a CancellationToken,
}

pub async fn execute_with_route_next_steps<P>(
    params: ExecuteRouteNextStepsParams<'_, P>,
) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let ExecuteRouteNextStepsParams {
        runner,
        task,
        project,
        step_slug,
        step_run_id,
        step_kind,
        route_edges,
        routine_steps,
        events_tx,
        cancel,
    } = params;
    let mut invalid_route_attempts = 0usize;
    let mut no_progress_attempts = 0usize;
    let mut pending_task = task;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_tool_calls = 0u32;

    loop {
        let output = stream_turn_output(
            runner,
            pending_task,
            step_slug.clone(),
            step_run_id,
            events_tx,
            cancel,
        )
        .await?;
        total_input_tokens += output.input_tokens;
        total_output_tokens += output.output_tokens;
        total_tool_calls += output.tool_calls;

        let retry_reason = match extract_route_next_steps(&output.messages) {
            Ok(Some(args)) => {
                match validate_route_next_steps_call(route_edges, routine_steps, step_kind, &args) {
                    Ok(()) => {
                        return Ok(TurnOutput {
                            input_tokens: total_input_tokens,
                            output_tokens: total_output_tokens,
                            tool_calls: total_tool_calls,
                            ..output
                        });
                    }
                    Err(error) => {
                        invalid_route_attempts += 1;
                        if invalid_route_attempts > ROUTE_NEXT_STEPS_RETRY_LIMIT {
                            bail!(
                                "Routine step called invalid {} tool after {} corrective attempt(s): {}",
                                ROUTE_NEXT_STEPS_TOOL_NAME,
                                ROUTE_NEXT_STEPS_RETRY_LIMIT,
                                error
                            );
                        }
                        RouteRetryReason::InvalidRoute(error.to_string())
                    }
                }
            }
            Ok(None) if output.tool_calls > 0 => {
                no_progress_attempts = 0;
                RouteRetryReason::MissingAfterProgress
            }
            Ok(None) => {
                no_progress_attempts += 1;
                if no_progress_attempts > ROUTE_NEXT_STEPS_RETRY_LIMIT {
                    bail!(
                        "Routine step did not call required {} tool after {} no-progress corrective attempt(s)",
                        ROUTE_NEXT_STEPS_TOOL_NAME,
                        ROUTE_NEXT_STEPS_RETRY_LIMIT
                    );
                }
                RouteRetryReason::MissingNoProgress
            }
            Err(error) => {
                invalid_route_attempts += 1;
                if invalid_route_attempts > ROUTE_NEXT_STEPS_RETRY_LIMIT {
                    bail!(
                        "Routine step called invalid {} tool after {} corrective attempt(s): {}",
                        ROUTE_NEXT_STEPS_TOOL_NAME,
                        ROUTE_NEXT_STEPS_RETRY_LIMIT,
                        error
                    );
                }
                RouteRetryReason::InvalidRoute(error.to_string())
            }
        };

        match &retry_reason {
            RouteRetryReason::InvalidRoute(error) => {
                warn!(
                    attempt = invalid_route_attempts,
                    error = %error,
                    "Routine step called invalid route_next_steps tool, retrying with explicit instruction"
                );
            }
            RouteRetryReason::MissingAfterProgress => {
                warn!(
                    "Routine step made tool progress without route_next_steps, retrying with explicit instruction"
                );
            }
            RouteRetryReason::MissingNoProgress => {
                warn!(
                    attempt = no_progress_attempts,
                    "Routine step omitted route_next_steps tool call without tool progress, retrying with explicit instruction"
                );
            }
        }

        pending_task = AgentRun {
            kind: crate::input::AgentRunKind::FollowUp(FollowUpInput {
                message: route_retry_prompt(step_kind, &output.text, &retry_reason),
                history: chat_history(&output.messages),
                project: project.clone(),
            }),
            execution: Default::default(),
        };
    }
}

fn edge_purpose(edge: &RoutineEdgeManifest) -> String {
    edge.metadata
        .get("purpose")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Downstream branch from this routine step.")
        .to_string()
}

fn edge_handoff_instructions(edge: &RoutineEdgeManifest) -> String {
    edge.metadata
        .get("handoff_instructions")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Pass the information this downstream step needs to continue.")
        .to_string()
}

fn condition_label(condition: RoutineEdgeCondition) -> &'static str {
    match condition {
        RoutineEdgeCondition::Always => "always",
        RoutineEdgeCondition::OnPass => "on_pass",
        RoutineEdgeCondition::OnFail => "on_fail",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(target: &str) -> RoutineEdgeManifest {
        edge_with_condition(target, RoutineEdgeCondition::Always)
    }

    fn edge_with_condition(target: &str, condition: RoutineEdgeCondition) -> RoutineEdgeManifest {
        RoutineEdgeManifest {
            routine: crate::Slug::derive("routine"),
            source_step: crate::Slug::derive("source"),
            target_step: crate::Slug::derive(target),
            condition,
            metadata: serde_json::json!({
                "purpose": format!("Send work to {target}"),
                "handoff_schema": {
                    "type": "object",
                    "required": ["work"],
                    "properties": {
                        "work": {"type": "string", "minLength": 1}
                    },
                    "additionalProperties": false
                }
            }),
        }
    }

    #[tokio::test]
    async fn route_tool_requires_all_targets_once() {
        let tool =
            RouteNextStepsTool::new(&[edge("a"), edge("b")], &[], RoutingStepKind::Agent).unwrap();
        let error = tool
            .execute(serde_json::json!({
                "verdict": "pass",
                "reasoning": "complete",
                "output": "done",
                "next_steps": [
                    {"target_step": "a", "handoff": {"work": "do a"}}
                ]
            }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exactly 2"));
    }

    #[test]
    fn extracted_route_call_validation_uses_step_edges() {
        let edges = vec![edge("a"), edge("b")];
        let error = validate_route_next_steps_call(
            &edges,
            &[],
            RoutingStepKind::Agent,
            &serde_json::json!({
                "verdict": "pass",
                "reasoning": "complete",
                "output": "done",
                "next_steps": [
                    {"target_step": "a", "handoff": {"work": "do a"}}
                ]
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exactly 2"));

        validate_route_next_steps_call(
            &edges,
            &[],
            RoutingStepKind::Agent,
            &serde_json::json!({
                "verdict": "pass",
                "reasoning": "complete",
                "output": "done",
                "next_steps": [
                    {"target_step": "a", "handoff": {"work": "do a"}},
                    {"target_step": "b", "handoff": {"work": "do b"}}
                ]
            }),
        )
        .expect("all outgoing targets should validate");
    }

    #[tokio::test]
    async fn route_tool_accepts_failure_without_routes() {
        let tool =
            RouteNextStepsTool::new(&[edge("a"), edge("b")], &[], RoutingStepKind::Agent).unwrap();
        let result = tool
            .execute(serde_json::json!({
                "verdict": "fail",
                "reasoning": "blocked",
                "output": "cannot continue"
            }))
            .await
            .expect("failure verdict should not require routes");

        assert!(result.success);
    }

    #[test]
    fn agent_route_validation_rejects_failure_next_steps() {
        let edges = vec![edge("a")];
        let error = validate_route_next_steps_call(
            &edges,
            &[],
            RoutingStepKind::Agent,
            &serde_json::json!({
                "verdict": "fail",
                "reasoning": "blocked",
                "output": "cannot continue",
                "next_steps": []
            }),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must not include next_steps for this agent step")
        );
    }

    #[test]
    fn gate_route_validation_uses_verdict_branch() {
        let edges = vec![
            edge_with_condition("done", RoutineEdgeCondition::OnPass),
            edge_with_condition("retry", RoutineEdgeCondition::OnFail),
        ];

        validate_route_next_steps_call(
            &edges,
            &[],
            RoutingStepKind::Gate,
            &serde_json::json!({
                "verdict": "fail",
                "reasoning": "needs revision",
                "output": "retry required",
                "next_steps": [
                    {"target_step": "retry", "handoff": {"work": "revise"}}
                ]
            }),
        )
        .expect("gate fail should route on_fail edge");

        let error = validate_route_next_steps_call(
            &edges,
            &[],
            RoutingStepKind::Gate,
            &serde_json::json!({
                "verdict": "fail",
                "reasoning": "needs revision",
                "output": "retry required",
                "next_steps": [
                    {"target_step": "done", "handoff": {"work": "finish"}}
                ]
            }),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("target_step 'done' is not activated by verdict 'fail'")
        );
    }
}
