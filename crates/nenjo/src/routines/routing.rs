//! Routine agent-step terminal routing tool.
//!
//! Agent routine steps use `route_next_steps` instead of `pass_verdict`.
//! The tool records the agent verdict and, on pass, the per-edge downstream
//! task decomposition used for deterministic fan-out and audit trails.

use anyhow::{Result, bail};
use nenjo_models::ChatMessage;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::warn;
use uuid::Uuid;

use super::RoutineEvent;
use crate::agents::runner::{AgentRunner, types::TurnOutput};
use crate::input::{AgentRun, FollowUpInput};
use crate::manifest::{RoutineEdgeCondition, RoutineEdgeManifest, RoutineStepManifest};
use crate::provider::ProviderRuntime;
use crate::tools::{Tool, ToolCategory, ToolResult};

pub const ROUTE_NEXT_STEPS_TOOL_NAME: &str = "route_next_steps";
const ROUTE_NEXT_STEPS_RETRY_LIMIT: usize = 2;

#[derive(Debug, Clone)]
pub struct RouteOption {
    pub target_step: crate::Slug,
    pub target_name: String,
    pub condition: RoutineEdgeCondition,
    pub purpose: String,
}

impl RouteOption {
    fn from_edge(edge: &RoutineEdgeManifest, target: Option<&RoutineStepManifest>) -> Self {
        Self {
            target_step: edge.target_step.clone(),
            target_name: target
                .map(|step| step.name.clone())
                .unwrap_or_else(|| edge.target_step.to_string()),
            condition: edge.condition,
            purpose: edge_purpose(edge),
        }
    }
}

pub struct RouteNextStepsTool {
    routes: Vec<RouteOption>,
    description: String,
}

impl RouteNextStepsTool {
    /// Build a step-scoped route tool whose schema only accepts this step's
    /// actual outgoing edge targets.
    pub fn new(edges: &[RoutineEdgeManifest], steps: &[RoutineStepManifest]) -> Self {
        let routes = edges
            .iter()
            .map(|edge| {
                let target = steps.iter().find(|step| step.slug == edge.target_step);
                RouteOption::from_edge(edge, target)
            })
            .collect::<Vec<_>>();
        let description = build_description(&routes);
        Self {
            routes,
            description,
        }
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
        let targets = self
            .routes
            .iter()
            .map(|route| serde_json::json!(route.target_step.to_string()))
            .collect::<Vec<_>>();
        let route_count = self.routes.len();

        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "next_steps": {
                    "type": "array",
                    "minItems": route_count,
                    "maxItems": route_count,
                    "description": "Required when verdict is pass. One decomposition item for every outgoing routine edge. Include every target exactly once.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "target_step": {
                                "type": "string",
                                "enum": targets,
                                "description": "The downstream routine step slug for this decomposed task."
                            },
                            "task": {
                                "type": "string",
                                "minLength": 1,
                                "description": "The specific task, evidence, or handoff this downstream step should receive."
                            },
                            "purpose": {
                                "type": "string",
                                "description": "Why this branch exists and what the downstream step should optimize for."
                            }
                        },
                        "required": ["target_step", "task"]
                    }
                },
                "verdict": {
                    "type": "string",
                    "enum": ["pass", "fail"],
                    "description": "Final verdict for this agent step: pass to route all downstream tasks, fail to stop the routine."
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
        validate_route_args(&self.routes, &args)?;
        let verdict = args
            .get("verdict")
            .and_then(|value| value.as_str())
            .unwrap_or("fail");
        Ok(ToolResult {
            success: true,
            output: match verdict {
                "pass" => format!(
                    "{} routing task(s) recorded for downstream routine steps.",
                    self.routes.len()
                ),
                _ => "Agent step failure verdict recorded; downstream routing will not run."
                    .to_string(),
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
/// The last call wins, matching the pass-verdict extraction behavior.
pub fn extract_route_next_steps(messages: &[ChatMessage]) -> Option<Value> {
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

        for call in tool_calls {
            let name = call
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if name != ROUTE_NEXT_STEPS_TOOL_NAME {
                continue;
            }
            let args = call
                .get("arguments")
                .and_then(|value| value.as_str())
                .unwrap_or("{}");
            if let Ok(value) = serde_json::from_str::<Value>(args) {
                return Some(value);
            }
        }
    }
    None
}

/// Resolve the required agent-step routing decision from the transcript.
///
/// This parses the terminal `route_next_steps` call into the verdict fields used
/// by the routine scheduler and step result audit data.
pub fn resolve_route_next_steps(messages: &[ChatMessage]) -> Result<RouteNextStepsDecision> {
    let Some(arguments) = extract_route_next_steps(messages) else {
        bail!("Agent did not call required route_next_steps tool");
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
) -> RouteNextStepsTool {
    RouteNextStepsTool::new(edges, steps)
}

fn validate_route_args(routes: &[RouteOption], args: &Value) -> Result<()> {
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
    if verdict == "fail" {
        if args.get("next_steps").is_some() {
            bail!("route_next_steps fail verdict must not include next_steps");
        }
        return Ok(());
    }

    let expected = routes
        .iter()
        .map(|route| route.target_step.to_string())
        .collect::<std::collections::HashSet<_>>();
    let Some(next_steps) = args.get("next_steps").and_then(|value| value.as_array()) else {
        bail!("route_next_steps requires next_steps");
    };
    if next_steps.len() != expected.len() {
        bail!(
            "route_next_steps requires exactly {} next_steps item(s)",
            expected.len()
        );
    }

    let mut seen = std::collections::HashSet::new();
    for next in next_steps {
        let Some(target) = next.get("target_step").and_then(|value| value.as_str()) else {
            bail!("route_next_steps next_steps item is missing target_step");
        };
        if !expected.contains(target) {
            bail!("route_next_steps target_step '{target}' is not an outgoing routine edge");
        }
        if !seen.insert(target.to_string()) {
            bail!("route_next_steps target_step '{target}' was submitted more than once");
        }
        let has_task = next
            .get("task")
            .and_then(|value| value.as_str())
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        if !has_task {
            bail!("route_next_steps target_step '{target}' is missing task");
        }
    }

    Ok(())
}

fn build_description(routes: &[RouteOption]) -> String {
    let mut description = format!(
        "Submit the final result for this agent routine step. Use verdict=\"fail\" when the step should fail and downstream routing must stop. Use verdict=\"pass\" when the step completed successfully; in that case, decompose the completed work across all {} outgoing edges and include exactly one next_steps item for each listed target. The runtime will fan out to all downstream steps only after this tool records a pass verdict.",
        routes.len()
    );
    for route in routes {
        description.push_str(&format!(
            "\n- target_step={} target_name=\"{}\" condition={} purpose=\"{}\"",
            route.target_step,
            route.target_name,
            condition_label(route.condition),
            route.purpose
        ));
    }
    description
}

fn route_retry_prompt(previous_text: &str) -> String {
    if previous_text.trim().is_empty() {
        format!(
            "You did not call `{}`. Call `{}` exactly once now as your final action. \
             Use verdict=\"pass\" with next_steps for every downstream route, or verdict=\"fail\" with reasoning and output if the step failed.",
            ROUTE_NEXT_STEPS_TOOL_NAME, ROUTE_NEXT_STEPS_TOOL_NAME
        )
    } else {
        format!(
            "Your previous response did not call `{}`. Based on the work you already completed, \
             call `{}` exactly once now as your final action. Do not redo the task. Use verdict=\"pass\" with next_steps for every downstream route, or verdict=\"fail\" with reasoning and output if the step failed.\n\nPrevious response:\n{}",
            ROUTE_NEXT_STEPS_TOOL_NAME, ROUTE_NEXT_STEPS_TOOL_NAME, previous_text
        )
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
) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let mut handle = runner.run_stream(task).await?;
    while let Some(event) = handle.recv().await {
        let _ = events_tx.send(RoutineEvent::AgentEvent {
            step_slug: step_slug.clone(),
            step_run_id,
            event,
        });
    }
    handle.output().await
}

/// Run an agent and require a terminal `route_next_steps` call.
///
/// If the model omits the tool, the harness sends a corrective follow-up turn
/// rather than accepting free-form text as a routine step result.
pub async fn execute_with_route_next_steps<P>(
    runner: &AgentRunner<P>,
    task: AgentRun,
    project: Option<crate::Slug>,
    step_slug: crate::Slug,
    step_run_id: Uuid,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
) -> Result<TurnOutput>
where
    P: ProviderRuntime,
{
    let mut attempts = 0usize;
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
        )
        .await?;
        total_input_tokens += output.input_tokens;
        total_output_tokens += output.output_tokens;
        total_tool_calls += output.tool_calls;

        if extract_route_next_steps(&output.messages).is_some() {
            return Ok(TurnOutput {
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                tool_calls: total_tool_calls,
                ..output
            });
        }

        attempts += 1;
        if attempts > ROUTE_NEXT_STEPS_RETRY_LIMIT {
            bail!(
                "Agent did not call required {} tool after {} corrective attempt(s)",
                ROUTE_NEXT_STEPS_TOOL_NAME,
                ROUTE_NEXT_STEPS_RETRY_LIMIT
            );
        }

        warn!(
            attempt = attempts,
            "Agent omitted route_next_steps tool call, retrying with explicit instruction"
        );

        pending_task = AgentRun {
            kind: crate::input::AgentRunKind::FollowUp(FollowUpInput {
                message: route_retry_prompt(&output.text),
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
        .or_else(|| edge.metadata.get("task"))
        .or_else(|| edge.metadata.get("description"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Downstream branch from this routine step.")
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
        RoutineEdgeManifest {
            routine: crate::Slug::derive("routine"),
            source_step: crate::Slug::derive("source"),
            target_step: crate::Slug::derive(target),
            condition: RoutineEdgeCondition::Always,
            metadata: serde_json::json!({"purpose": format!("Send work to {target}")}),
        }
    }

    #[tokio::test]
    async fn route_tool_requires_all_targets_once() {
        let tool = RouteNextStepsTool::new(&[edge("a"), edge("b")], &[]);
        let error = tool
            .execute(serde_json::json!({
                "verdict": "pass",
                "reasoning": "complete",
                "output": "done",
                "next_steps": [
                    {"target_step": "a", "task": "do a"}
                ]
            }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exactly 2"));
    }

    #[tokio::test]
    async fn route_tool_accepts_failure_without_routes() {
        let tool = RouteNextStepsTool::new(&[edge("a"), edge("b")], &[]);
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
}
