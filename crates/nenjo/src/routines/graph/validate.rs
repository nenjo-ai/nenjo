use std::collections::{HashMap, HashSet, VecDeque};

use crate::Slug;

use super::error::{RoutineValidationError, RoutineValidationIssue};
use super::types::{
    RoutineGraph, RoutineGraphEdge, RoutineGraphEdgeCondition, RoutineGraphStep,
    RoutineGraphStepType,
};
use super::utils::{edge_key, outgoing_edges, required_inbound_targets, steps_by_slug};

type ValidationResult = Result<(), RoutineValidationError>;

/// Validate the routine graph embedded in a runtime manifest.
///
/// This is harness/runtime validation. Platform callers should adapt their
/// persisted record types into [`RoutineGraph`] and call [`validate_routine_graph`]
/// before saving or dispatching a run.
pub fn validate_routine_manifest(routine: &crate::manifest::RoutineManifest) -> ValidationResult {
    validate_routine_graph(&RoutineGraph::from_manifest(routine))
}

/// Validate a routine graph against the canonical SDK contract.
///
/// Canonical semantics:
/// - one or more `entry_steps` start as parallel roots;
/// - a step with multiple activated inbound edges is an all-success join;
/// - agent fan-out is explicit and auditable through `route_next_steps`;
/// - `on_fail` edges are only gate verdict branches;
/// - gate retry exhaustion fails the routine directly;
/// - cycles are only allowed through gate `on_fail` retry loops.
pub fn validate_routine_graph(graph: &RoutineGraph) -> ValidationResult {
    validate_not_empty(&graph.steps)?;
    validate_unique_steps(&graph.steps)?;
    validate_step_resource_bindings(&graph.steps)?;
    validate_edges_reference_steps(&graph.steps, &graph.edges)?;
    validate_no_self_edges(&graph.edges)?;
    validate_no_duplicate_edges(&graph.edges)?;
    validate_gate_edges_are_verdict_routed(&graph.steps, &graph.edges)?;
    validate_on_fail_edges_originate_from_gates(&graph.steps, &graph.edges)?;
    validate_at_least_one_terminal(&graph.steps)?;
    validate_terminal_no_outgoing(&graph.steps, &graph.edges)?;
    validate_non_terminal_have_outgoing(&graph.steps, &graph.edges)?;
    validate_no_retry_exhaustion_branches(&graph.edges)?;
    validate_cycles_only_use_on_fail_edges(&graph.steps, &graph.edges)?;
    validate_entry_steps(&graph.steps, &graph.edges, &graph.entry_steps)?;
    validate_all_reachable(&graph.steps, &graph.edges, &graph.entry_steps)?;
    Ok(())
}

fn fail(message: impl Into<String>) -> RoutineValidationError {
    RoutineValidationError::single(RoutineValidationIssue::new(message))
}

fn validate_not_empty(steps: &[RoutineGraphStep]) -> ValidationResult {
    if steps.is_empty() {
        Err(fail("Routine graph must contain at least one step"))
    } else {
        Ok(())
    }
}

fn validate_unique_steps(steps: &[RoutineGraphStep]) -> ValidationResult {
    let mut seen = HashSet::new();
    for step in steps {
        if !seen.insert(step.slug.clone()) {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!("Duplicate routine step slug: {}", step.slug))
                    .step(step.slug.to_string()),
            ));
        }
    }
    Ok(())
}

fn validate_step_resource_bindings(steps: &[RoutineGraphStep]) -> ValidationResult {
    for step in steps {
        match step.step_type {
            RoutineGraphStepType::Agent | RoutineGraphStepType::Gate if !step.has_agent => {
                return Err(RoutineValidationError::single(
                    RoutineValidationIssue::new(format!(
                        "{} step '{}' must reference an agent",
                        step_type_label(step.step_type),
                        step.name
                    ))
                    .step(step.slug.to_string()),
                ));
            }
            RoutineGraphStepType::Council if !step.has_council => {
                return Err(RoutineValidationError::single(
                    RoutineValidationIssue::new(format!(
                        "Council step '{}' must reference a council",
                        step.name
                    ))
                    .step(step.slug.to_string()),
                ));
            }
            RoutineGraphStepType::Lambda if !step.has_lambda => {
                return Err(RoutineValidationError::single(
                    RoutineValidationIssue::new(format!(
                        "Lambda step '{}' must reference a lambda",
                        step.name
                    ))
                    .step(step.slug.to_string()),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_edges_reference_steps(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
) -> ValidationResult {
    let known = steps_by_slug(steps);
    for edge in edges {
        if !known.contains_key(&edge.source_step) {
            return Err(fail(format!(
                "Unknown edge source step slug: {}",
                edge.source_step
            )));
        }
        if !known.contains_key(&edge.target_step) {
            return Err(fail(format!(
                "Unknown edge target step slug: {}",
                edge.target_step
            )));
        }
    }
    Ok(())
}

fn validate_no_self_edges(edges: &[RoutineGraphEdge]) -> ValidationResult {
    for edge in edges {
        if edge.source_step == edge.target_step {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new("Source and target steps must be different")
                    .edge(edge_key(edge)),
            ));
        }
    }
    Ok(())
}

fn validate_no_duplicate_edges(edges: &[RoutineGraphEdge]) -> ValidationResult {
    let mut seen = HashSet::new();
    for edge in edges {
        let key = edge_key(edge);
        if !seen.insert(key.clone()) {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!("Duplicate routine edge: {key}")).edge(key),
            ));
        }
    }
    Ok(())
}

fn validate_gate_edges_are_verdict_routed(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
) -> ValidationResult {
    let step_map = steps_by_slug(steps);
    for edge in edges {
        if edge.condition == RoutineGraphEdgeCondition::Always
            && let Some(step) = step_map.get(&edge.source_step)
            && step.step_type == RoutineGraphStepType::Gate
        {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!(
                    "Gate step '{}' must use on_pass/on_fail edges, not always",
                    step.name
                ))
                .step(step.slug.to_string())
                .edge(edge_key(edge)),
            ));
        }
    }
    Ok(())
}

fn validate_on_fail_edges_originate_from_gates(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
) -> ValidationResult {
    let step_map = steps_by_slug(steps);
    for edge in edges {
        if edge.condition == RoutineGraphEdgeCondition::OnFail
            && let Some(step) = step_map.get(&edge.source_step)
            && step.step_type != RoutineGraphStepType::Gate
        {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!(
                    "on_fail edge from step '{}' is invalid: on_fail edges may only originate from gate steps",
                    step.name
                ))
                .step(step.slug.to_string())
                .edge(edge_key(edge)),
            ));
        }
    }
    Ok(())
}

fn validate_at_least_one_terminal(steps: &[RoutineGraphStep]) -> ValidationResult {
    if steps.iter().any(|step| step.step_type.is_terminal()) {
        Ok(())
    } else {
        Err(fail(
            "Routine graph must include at least one terminal or terminal_fail step",
        ))
    }
}

fn validate_terminal_no_outgoing(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
) -> ValidationResult {
    let step_map = steps_by_slug(steps);
    for edge in edges {
        if let Some(step) = step_map.get(&edge.source_step)
            && step.step_type.is_terminal()
        {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!(
                    "Terminal step '{}' must not have outgoing edges",
                    step.name
                ))
                .step(step.slug.to_string())
                .edge(edge_key(edge)),
            ));
        }
    }
    Ok(())
}

fn validate_non_terminal_have_outgoing(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
) -> ValidationResult {
    let outgoing = outgoing_edges(edges);
    for step in steps {
        if !step.step_type.is_terminal() && !outgoing.contains_key(&step.slug) {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!(
                    "Non-terminal step '{}' must have at least one outgoing edge",
                    step.name
                ))
                .step(step.slug.to_string()),
            ));
        }
    }
    Ok(())
}

fn validate_no_retry_exhaustion_branches(edges: &[RoutineGraphEdge]) -> ValidationResult {
    for edge in edges {
        if edge.metadata.get("on_exhausted").is_some()
            || edge.metadata.get("on_exhausted_step").is_some()
            || edge.metadata.get("on_exhausted_step_id").is_some()
        {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!(
                    "Edge {} must not define on_exhausted metadata; retry exhaustion fails the routine directly",
                    edge_key(edge)
                ))
                .edge(edge_key(edge)),
            ));
        }
    }
    Ok(())
}

fn validate_cycles_only_use_on_fail_edges(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
) -> ValidationResult {
    let mut indegree: HashMap<Slug, usize> = steps
        .iter()
        .map(|step| (step.slug.clone(), 0usize))
        .collect();
    let mut adjacency: HashMap<Slug, Vec<Slug>> = HashMap::new();

    for edge in edges
        .iter()
        .filter(|edge| edge.condition != RoutineGraphEdgeCondition::OnFail)
    {
        adjacency
            .entry(edge.source_step.clone())
            .or_default()
            .push(edge.target_step.clone());
        *indegree.entry(edge.target_step.clone()).or_default() += 1;
    }

    let mut queue: VecDeque<Slug> = indegree
        .iter()
        .filter_map(|(slug, degree)| (*degree == 0).then_some(slug.clone()))
        .collect();
    let mut visited = 0usize;

    while let Some(slug) = queue.pop_front() {
        visited += 1;
        if let Some(targets) = adjacency.get(&slug) {
            for target in targets {
                let degree = indegree
                    .get_mut(target)
                    .expect("target indegree should exist for known step");
                *degree -= 1;
                if *degree == 0 {
                    queue.push_back(target.clone());
                }
            }
        }
    }

    if visited == steps.len() {
        Ok(())
    } else {
        Err(fail(
            "Routine graph contains a cycle outside a gate on_fail retry loop",
        ))
    }
}

fn validate_entry_steps(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
    entry_steps: &[Slug],
) -> ValidationResult {
    if entry_steps.is_empty() {
        return Err(fail("Routine graph must include at least one entry step"));
    }

    let known = steps_by_slug(steps);
    let mut seen = HashSet::new();
    let required_inbound = required_inbound_targets(edges);
    for entry in entry_steps {
        if !known.contains_key(entry) {
            return Err(fail(format!("Unknown entry step slug: {entry}")));
        }
        if !seen.insert(entry.clone()) {
            return Err(fail(format!("Duplicate routine entry step slug: {entry}")));
        }
        if required_inbound.contains(entry) {
            return Err(fail(format!(
                "Entry step '{entry}' must not have incoming always/on_pass edges"
            )));
        }
    }
    Ok(())
}

fn validate_all_reachable(
    steps: &[RoutineGraphStep],
    edges: &[RoutineGraphEdge],
    entry_steps: &[Slug],
) -> ValidationResult {
    let mut adjacency: HashMap<Slug, Vec<Slug>> = HashMap::new();
    for edge in edges {
        adjacency
            .entry(edge.source_step.clone())
            .or_default()
            .push(edge.target_step.clone());
    }

    let mut visited: HashSet<Slug> = HashSet::new();
    let mut queue: VecDeque<Slug> = entry_steps.iter().cloned().collect();
    while let Some(slug) = queue.pop_front() {
        if !visited.insert(slug.clone()) {
            continue;
        }
        if let Some(targets) = adjacency.get(&slug) {
            for target in targets {
                queue.push_back(target.clone());
            }
        }
    }

    for step in steps {
        if !visited.contains(&step.slug) {
            return Err(RoutineValidationError::single(
                RoutineValidationIssue::new(format!(
                    "Step '{}' is not reachable from any entry step",
                    step.name
                ))
                .step(step.slug.to_string()),
            ));
        }
    }
    Ok(())
}

fn step_type_label(step_type: RoutineGraphStepType) -> &'static str {
    match step_type {
        RoutineGraphStepType::Agent => "Agent",
        RoutineGraphStepType::Gate => "Gate",
        RoutineGraphStepType::Council => "Council",
        RoutineGraphStepType::Lambda => "Lambda",
        RoutineGraphStepType::Terminal => "Terminal",
        RoutineGraphStepType::TerminalFail => "Terminal fail",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(slug: &str, step_type: RoutineGraphStepType) -> RoutineGraphStep {
        RoutineGraphStep {
            slug: Slug::derive(slug),
            name: slug.to_string(),
            step_type,
            has_agent: matches!(
                step_type,
                RoutineGraphStepType::Agent | RoutineGraphStepType::Gate
            ),
            has_council: step_type == RoutineGraphStepType::Council,
            has_lambda: step_type == RoutineGraphStepType::Lambda,
        }
    }

    fn edge(source: &str, target: &str, condition: RoutineGraphEdgeCondition) -> RoutineGraphEdge {
        RoutineGraphEdge {
            source_step: Slug::derive(source),
            target_step: Slug::derive(target),
            condition,
            metadata: serde_json::json!({}),
        }
    }

    fn valid_linear_graph() -> RoutineGraph {
        RoutineGraph {
            entry_steps: vec![Slug::derive("start")],
            steps: vec![
                step("start", RoutineGraphStepType::Agent),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![edge("start", "done", RoutineGraphEdgeCondition::Always)],
        }
    }

    fn assert_invalid(graph: RoutineGraph, expected: &str) {
        let error = validate_routine_graph(&graph).expect_err("graph should fail");
        assert!(
            error.to_string().contains(expected),
            "expected error containing {expected:?}, got {error}"
        );
    }

    #[test]
    fn accepts_parallel_entries_and_and_join_shape() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("research"), Slug::derive("review")],
            steps: vec![
                step("research", RoutineGraphStepType::Agent),
                step("review", RoutineGraphStepType::Agent),
                step("synthesize", RoutineGraphStepType::Agent),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![
                edge("research", "synthesize", RoutineGraphEdgeCondition::Always),
                edge("review", "synthesize", RoutineGraphEdgeCondition::Always),
                edge("synthesize", "done", RoutineGraphEdgeCondition::Always),
            ],
        };

        validate_routine_graph(&graph).expect("graph should validate");
    }

    #[test]
    fn rejects_entry_with_required_inbound_edge() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("join")],
            steps: vec![
                step("start", RoutineGraphStepType::Agent),
                step("join", RoutineGraphStepType::Agent),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![
                edge("start", "join", RoutineGraphEdgeCondition::Always),
                edge("join", "done", RoutineGraphEdgeCondition::Always),
            ],
        };

        let error = validate_routine_graph(&graph).expect_err("graph should fail");
        assert!(
            error
                .to_string()
                .contains("must not have incoming always/on_pass")
        );
    }

    #[test]
    fn rejects_duplicate_entry_step() {
        let mut graph = valid_linear_graph();
        graph.entry_steps.push(Slug::derive("start"));

        assert_invalid(graph, "Duplicate routine entry step slug");
    }

    #[test]
    fn rejects_unknown_entry_step() {
        let mut graph = valid_linear_graph();
        graph.entry_steps = vec![Slug::derive("missing")];

        assert_invalid(graph, "Unknown entry step slug");
    }

    #[test]
    fn rejects_duplicate_edge() {
        let mut graph = valid_linear_graph();
        graph
            .edges
            .push(edge("start", "done", RoutineGraphEdgeCondition::Always));

        assert_invalid(graph, "Duplicate routine edge");
    }

    #[test]
    fn rejects_terminal_outgoing_edge() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("done")],
            steps: vec![
                step("done", RoutineGraphStepType::Terminal),
                step("next", RoutineGraphStepType::Terminal),
            ],
            edges: vec![edge("done", "next", RoutineGraphEdgeCondition::Always)],
        };

        assert_invalid(graph, "must not have outgoing edges");
    }

    #[test]
    fn rejects_non_terminal_without_outgoing_edge() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("start")],
            steps: vec![
                step("start", RoutineGraphStepType::Agent),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![],
        };

        assert_invalid(graph, "must have at least one outgoing edge");
    }

    #[test]
    fn rejects_gate_always_edge() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("gate")],
            steps: vec![
                step("gate", RoutineGraphStepType::Gate),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![edge("gate", "done", RoutineGraphEdgeCondition::Always)],
        };

        assert_invalid(graph, "must use on_pass/on_fail");
    }

    #[test]
    fn rejects_on_fail_from_non_gate() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("start")],
            steps: vec![
                step("start", RoutineGraphStepType::Agent),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![edge("start", "done", RoutineGraphEdgeCondition::OnFail)],
        };

        assert_invalid(graph, "on_fail edges may only originate from gate steps");
    }

    #[test]
    fn rejects_non_fail_cycle() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("a")],
            steps: vec![
                step("a", RoutineGraphStepType::Agent),
                step("b", RoutineGraphStepType::Agent),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![
                edge("a", "b", RoutineGraphEdgeCondition::Always),
                edge("b", "a", RoutineGraphEdgeCondition::Always),
                edge("b", "done", RoutineGraphEdgeCondition::Always),
            ],
        };

        assert_invalid(graph, "contains a cycle");
    }

    #[test]
    fn accepts_gate_on_fail_retry_cycle() {
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("work")],
            steps: vec![
                step("work", RoutineGraphStepType::Agent),
                step("gate", RoutineGraphStepType::Gate),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![
                edge("work", "gate", RoutineGraphEdgeCondition::Always),
                edge("gate", "done", RoutineGraphEdgeCondition::OnPass),
                edge("gate", "work", RoutineGraphEdgeCondition::OnFail),
            ],
        };

        validate_routine_graph(&graph).expect("retry graph should validate");
    }

    #[test]
    fn rejects_retry_exhaustion_branch_metadata() {
        let mut retry = edge("gate", "work", RoutineGraphEdgeCondition::OnFail);
        retry.metadata = serde_json::json!({"on_exhausted": "missing"});
        let graph = RoutineGraph {
            entry_steps: vec![Slug::derive("work")],
            steps: vec![
                step("work", RoutineGraphStepType::Agent),
                step("gate", RoutineGraphStepType::Gate),
                step("done", RoutineGraphStepType::Terminal),
            ],
            edges: vec![
                edge("work", "gate", RoutineGraphEdgeCondition::Always),
                edge("gate", "done", RoutineGraphEdgeCondition::OnPass),
                retry,
            ],
        };

        assert_invalid(graph, "must not define on_exhausted metadata");
    }

    #[test]
    fn rejects_agent_without_agent_binding() {
        let mut graph = valid_linear_graph();
        graph.steps[0].has_agent = false;

        assert_invalid(graph, "must reference an agent");
    }
}
