use serde_json::Value;

use crate::Slug;
use crate::manifest::{
    RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest, RoutineStepManifest,
    RoutineStepType,
};

/// Step type understood by the shared routine graph validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoutineGraphStepType {
    Agent,
    Council,
    Gate,
    Lambda,
    Terminal,
    TerminalFail,
}

impl RoutineGraphStepType {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "council" => Some(Self::Council),
            "gate" => Some(Self::Gate),
            "lambda" => Some(Self::Lambda),
            "terminal" => Some(Self::Terminal),
            "terminal_fail" => Some(Self::TerminalFail),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal | Self::TerminalFail)
    }
}

impl From<RoutineStepType> for RoutineGraphStepType {
    fn from(value: RoutineStepType) -> Self {
        match value {
            RoutineStepType::Agent => Self::Agent,
            RoutineStepType::Council => Self::Council,
            RoutineStepType::Gate => Self::Gate,
            RoutineStepType::Terminal => Self::Terminal,
            RoutineStepType::TerminalFail => Self::TerminalFail,
        }
    }
}

/// Edge condition understood by the shared routine graph validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoutineGraphEdgeCondition {
    Always,
    OnPass,
    OnFail,
}

impl RoutineGraphEdgeCondition {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "always" => Some(Self::Always),
            "on_pass" => Some(Self::OnPass),
            "on_fail" => Some(Self::OnFail),
            _ => None,
        }
    }
}

impl From<RoutineEdgeCondition> for RoutineGraphEdgeCondition {
    fn from(value: RoutineEdgeCondition) -> Self {
        match value {
            RoutineEdgeCondition::Always => Self::Always,
            RoutineEdgeCondition::OnPass => Self::OnPass,
            RoutineEdgeCondition::OnFail => Self::OnFail,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutineGraphStep {
    pub slug: Slug,
    pub name: String,
    pub step_type: RoutineGraphStepType,
    pub has_agent: bool,
    pub has_council: bool,
    pub has_lambda: bool,
}

impl RoutineGraphStep {
    pub fn from_manifest(step: &RoutineStepManifest) -> Self {
        Self {
            slug: step.slug.clone(),
            name: step.name.clone(),
            step_type: step.step_type.into(),
            has_agent: step.agent.is_some(),
            has_council: step.council.is_some(),
            has_lambda: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutineGraphEdge {
    pub source_step: Slug,
    pub target_step: Slug,
    pub condition: RoutineGraphEdgeCondition,
    pub metadata: Value,
}

impl RoutineGraphEdge {
    pub fn from_manifest(edge: &RoutineEdgeManifest) -> Self {
        Self {
            source_step: edge.source_step.clone(),
            target_step: edge.target_step.clone(),
            condition: edge.condition.into(),
            metadata: edge.metadata.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutineGraph {
    pub entry_steps: Vec<Slug>,
    pub steps: Vec<RoutineGraphStep>,
    pub edges: Vec<RoutineGraphEdge>,
}

impl RoutineGraph {
    pub fn from_manifest(routine: &RoutineManifest) -> Self {
        Self {
            entry_steps: routine.metadata.entry_steps.clone(),
            steps: routine
                .steps
                .iter()
                .map(RoutineGraphStep::from_manifest)
                .collect(),
            edges: routine
                .edges
                .iter()
                .map(RoutineGraphEdge::from_manifest)
                .collect(),
        }
    }
}
