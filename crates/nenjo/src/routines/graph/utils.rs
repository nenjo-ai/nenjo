use std::collections::{HashMap, HashSet};

use crate::Slug;

use super::types::{RoutineGraphEdge, RoutineGraphEdgeCondition, RoutineGraphStep};

pub fn steps_by_slug(steps: &[RoutineGraphStep]) -> HashMap<Slug, &RoutineGraphStep> {
    steps.iter().map(|step| (step.slug.clone(), step)).collect()
}

pub fn outgoing_edges(edges: &[RoutineGraphEdge]) -> HashMap<Slug, Vec<&RoutineGraphEdge>> {
    let mut outgoing: HashMap<Slug, Vec<&RoutineGraphEdge>> = HashMap::new();
    for edge in edges {
        outgoing
            .entry(edge.source_step.clone())
            .or_default()
            .push(edge);
    }
    outgoing
}

pub fn required_inbound_targets(edges: &[RoutineGraphEdge]) -> HashSet<Slug> {
    edges
        .iter()
        .filter(|edge| edge.condition != RoutineGraphEdgeCondition::OnFail)
        .map(|edge| edge.target_step.clone())
        .collect()
}

pub fn edge_key(edge: &RoutineGraphEdge) -> String {
    let condition = match edge.condition {
        RoutineGraphEdgeCondition::Always => "always",
        RoutineGraphEdgeCondition::OnPass => "on_pass",
        RoutineGraphEdgeCondition::OnFail => "on_fail",
    };
    format!("{}:{}:{}", edge.source_step, condition, edge.target_step)
}
