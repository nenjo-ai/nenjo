//! Routine execution types — inputs, state, step config.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::Slug;
use crate::input::{RoutineRun, RoutineRunKind, TaskInput};
use crate::manifest::ProjectManifest;

/// Outcome of a routine step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Uuid>,
    pub passed: bool,
    pub output: String,
    pub data: serde_json::Value,
    pub step_slug: Slug,
    pub step_name: String,
    /// Total input tokens consumed across all LLM calls in this step.
    pub input_tokens: u64,
    /// Total output tokens consumed across all LLM calls in this step.
    pub output_tokens: u64,
    /// Number of tool calls executed during this step.
    pub tool_calls: u32,
    /// Full conversation messages (excluding system/developer) for chat history
    /// persistence. Only populated for chat tasks.
    pub messages: Vec<nenjo_models::ChatMessage>,
}

impl Default for StepResult {
    fn default() -> Self {
        Self {
            task_id: None,
            passed: false,
            output: String::new(),
            data: serde_json::Value::Null,
            step_slug: Slug::derive("unknown_step"),
            step_name: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            tool_calls: 0,
            messages: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// RoutineInput — caller-provided context for a routine execution
// ---------------------------------------------------------------------------

/// Input context for a routine execution.
///
/// ```ignore
/// let input = RoutineInput::new("Implement auth", "Add JWT authentication")
///     .with_task_id(task_id)
///     .with_execution_run_id(run_id)
///     .with_labels(vec!["auth".into(), "security".into()]);
/// ```
#[derive(Clone)]
pub struct RoutineInput {
    pub project: Option<Slug>,
    pub title: String,
    pub instructions: String,
    pub task_id: Option<Uuid>,
    pub execution_run_id: Option<Uuid>,
    pub labels: Vec<String>,
    pub slug: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub git: Option<crate::types::GitContext>,
    pub project_name: Option<String>,
    pub project_description: Option<String>,
    pub project_metadata: Option<String>,
    pub session_binding: Option<SessionBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBinding {
    pub session_id: Uuid,
    pub memory_namespace: Option<String>,
}

impl RoutineInput {
    pub fn new(title: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            project: None,
            title: title.into(),
            instructions: instructions.into(),
            task_id: None,
            execution_run_id: None,
            labels: Vec::new(),
            slug: None,
            status: None,
            priority: None,
            git: None,
            project_name: None,
            project_description: None,
            project_metadata: None,
            session_binding: None,
        }
    }

    pub fn with_task_id(mut self, id: Uuid) -> Self {
        self.task_id = Some(id);
        self
    }

    pub fn with_execution_run_id(mut self, id: Uuid) -> Self {
        self.execution_run_id = Some(id);
        self
    }

    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }

    pub fn with_slug(mut self, slug: impl Into<String>) -> Self {
        self.slug = Some(slug.into());
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_priority(mut self, priority: impl Into<String>) -> Self {
        self.priority = Some(priority.into());
        self
    }

    pub fn with_git(mut self, git: Option<crate::types::GitContext>) -> Self {
        self.git = git;
        self
    }

    pub fn with_project_context(mut self, project: &ProjectManifest) -> Self {
        self.project = Some(project.slug.clone());
        self.project_name = Some(project.name.clone());
        self.project_description = Some(project.description.clone().unwrap_or_default());
        let metadata = nenjo_xml::types::metadata_json_to_xml(&project.settings);
        if !metadata.is_empty() {
            self.project_metadata = Some(metadata);
        }
        self
    }

    pub fn with_session_binding(mut self, binding: SessionBinding) -> Self {
        self.session_binding = Some(binding);
        self
    }

    pub(crate) fn from_routine_run(run: RoutineRun) -> Self {
        match run.kind {
            RoutineRunKind::Task(task) => {
                let location = run.execution.project_location;
                let mut input = RoutineInput::from_task_input(task)
                    .with_git(location.and_then(|location| location.git))
                    .with_execution_run_id_opt(run.execution.execution_run_id);
                if let Some(binding) = run.execution.session_binding {
                    input = input.with_session_binding(binding);
                }
                input
            }
        }
    }

    fn from_task_input(task: TaskInput) -> Self {
        let mut input = RoutineInput::new(task.title, task.instructions)
            .with_labels(task.labels)
            .with_task_id(task.task_id);
        input.project = task.project;
        if let Some(slug) = task.slug {
            input = input.with_slug(slug);
        }
        if let Some(status) = task.status {
            input = input.with_status(status);
        }
        if let Some(priority) = task.priority {
            input = input.with_priority(priority);
        }
        input
    }

    fn with_execution_run_id_opt(mut self, id: Option<Uuid>) -> Self {
        self.execution_run_id = id;
        self
    }
}

// ---------------------------------------------------------------------------
// RoutineState — internal accumulator during execution
// ---------------------------------------------------------------------------

/// Internal execution state, accumulated as steps run.
#[derive(Clone)]
pub(crate) struct RoutineState {
    pub step_results: HashMap<Slug, StepResult>,
    pub handoffs: HashMap<Slug, Vec<RoutineHandoff>>,
    pub step_run_ids: HashMap<Slug, Uuid>,
    pub completed_steps: Vec<Slug>,
    pub initial_input: String,
    pub input: RoutineInput,
    pub routine_name: Option<String>,
    pub current_step_name: Option<String>,
    pub current_step_type: Option<String>,
    pub step_instructions: Option<String>,
    pub step_metadata: Option<String>,
    pub metrics: RoutineMetrics,
}

impl RoutineState {
    pub fn new(input: RoutineInput) -> Self {
        let initial_input = input.instructions.clone();
        Self {
            step_results: HashMap::new(),
            handoffs: HashMap::new(),
            step_run_ids: HashMap::new(),
            completed_steps: Vec::new(),
            initial_input,
            input,
            routine_name: None,
            current_step_name: None,
            current_step_type: None,
            step_instructions: None,
            step_metadata: None,
            metrics: RoutineMetrics::new(),
        }
    }

    pub(crate) fn record_step_result(&mut self, step_slug: Slug, result: StepResult) {
        self.completed_steps.push(step_slug.clone());
        self.step_results.insert(step_slug, result);
    }

    pub(crate) fn record_handoff(&mut self, handoff: RoutineHandoff) {
        self.handoffs
            .entry(handoff.target_step.clone())
            .or_default()
            .push(handoff);
    }

    pub(crate) fn handoffs_for(&self, step_slug: &Slug) -> &[RoutineHandoff] {
        self.handoffs
            .get(step_slug)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn step_run_id_for(&mut self, step_slug: &Slug) -> Uuid {
        *self
            .step_run_ids
            .entry(step_slug.clone())
            .or_insert_with(Uuid::new_v4)
    }

    pub(crate) fn last_step_result(&self) -> Option<&StepResult> {
        self.completed_steps
            .iter()
            .rev()
            .find_map(|slug| self.step_results.get(slug))
    }
}

/// One activated routine edge and the structured handoff it delivered.
#[derive(Debug, Clone)]
pub struct RoutineHandoff {
    pub source_step: Slug,
    pub target_step: Slug,
    pub handoff: serde_json::Value,
    pub purpose: Option<String>,
    pub summary: Option<String>,
    pub edge_condition: EdgeCondition,
}

// ---------------------------------------------------------------------------
// EdgeCondition — conditional routing on DAG edges
// ---------------------------------------------------------------------------

pub use crate::manifest::RoutineEdgeCondition as EdgeCondition;

// ---------------------------------------------------------------------------
// StepType — the type of a routine step
// ---------------------------------------------------------------------------

/// Type of routine step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepType {
    Agent,
    Council,
    Gate,
    Terminal,
    TerminalFail,
}

impl StepType {
    pub fn from_str_value(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "council" => Self::Council,
            "gate" => Self::Gate,
            "terminal" => Self::Terminal,
            "terminal_fail" => Self::TerminalFail,
            _ => Self::Agent,
        }
    }
}

// ---------------------------------------------------------------------------
// RoutineMetrics
// ---------------------------------------------------------------------------

/// Accumulated metrics for a single routine step.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StepMetrics {
    pub execution_count: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl StepMetrics {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// Accumulator for all step metrics within a routine execution.
#[derive(Debug, Clone, Default)]
pub struct RoutineMetrics {
    steps: HashMap<Slug, StepMetrics>,
}

impl RoutineMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_step(&mut self, step_slug: &Slug, input_tokens: u64, output_tokens: u64) {
        let entry = self.steps.entry(step_slug.clone()).or_default();
        entry.execution_count += 1;
        entry.input_tokens += input_tokens;
        entry.output_tokens += output_tokens;
    }

    pub fn get(&self, step_slug: &Slug) -> Option<&StepMetrics> {
        self.steps.get(step_slug)
    }

    /// Total input tokens across all steps.
    pub fn total_input_tokens(&self) -> u64 {
        self.steps.values().map(|s| s.input_tokens).sum()
    }

    /// Total output tokens across all steps.
    pub fn total_output_tokens(&self) -> u64 {
        self.steps.values().map(|s| s.output_tokens).sum()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edge_condition_parsing() {
        assert_eq!(
            EdgeCondition::from_str_value("on_pass"),
            EdgeCondition::OnPass
        );
        assert_eq!(
            EdgeCondition::from_str_value("on_fail"),
            EdgeCondition::OnFail
        );
        assert_eq!(
            EdgeCondition::from_str_value("always"),
            EdgeCondition::Always
        );
        assert_eq!(
            EdgeCondition::from_str_value("unknown"),
            EdgeCondition::Always
        );
    }

    #[test]
    fn edge_condition_satisfaction() {
        assert!(EdgeCondition::Always.is_satisfied(true));
        assert!(EdgeCondition::Always.is_satisfied(false));
        assert!(EdgeCondition::OnPass.is_satisfied(true));
        assert!(!EdgeCondition::OnPass.is_satisfied(false));
        assert!(!EdgeCondition::OnFail.is_satisfied(true));
        assert!(EdgeCondition::OnFail.is_satisfied(false));
    }

    #[test]
    fn step_type_parsing() {
        assert_eq!(StepType::from_str_value("agent"), StepType::Agent);
        assert_eq!(StepType::from_str_value("council"), StepType::Council);
        assert_eq!(StepType::from_str_value("gate"), StepType::Gate);
        assert_eq!(StepType::from_str_value("terminal"), StepType::Terminal);
        assert_eq!(
            StepType::from_str_value("terminal_fail"),
            StepType::TerminalFail
        );
        assert_eq!(StepType::from_str_value("unknown"), StepType::Agent);
    }

    #[test]
    fn routine_input_builder() {
        let project = ProjectManifest {
            name: "Demo Project".to_string(),
            slug: Slug::derive("demo_project"),
            description: Some("Project description".to_string()),
            settings: serde_json::json!({}),
        };
        let input = RoutineInput::new("Title", "Desc")
            .with_project_context(&project)
            .with_labels(vec!["a".into()]);
        assert_eq!(
            input.project.as_ref().map(Slug::as_str),
            Some("demo_project")
        );
        assert_eq!(input.title, "Title");
        assert_eq!(input.labels, vec!["a"]);
    }

    #[test]
    fn routine_input_builder_uses_project_context_for_project_data() {
        let project = ProjectManifest {
            name: "Demo Project".to_string(),
            slug: Slug::derive("demo_project"),
            description: Some("Project description".to_string()),
            settings: serde_json::json!({
                "context": "Use Postgres",
                "metadata": {
                    "owner": "platform"
                }
            }),
        };

        let input = RoutineInput::new("Title", "Desc").with_project_context(&project);

        assert_eq!(
            input.project.as_ref().map(Slug::as_str),
            Some("demo_project")
        );
        assert_eq!(input.project_name.as_deref(), Some("Demo Project"));
        assert_eq!(
            input.project_description.as_deref(),
            Some("Project description")
        );
        assert!(
            input
                .project_metadata
                .as_deref()
                .is_some_and(|metadata| !metadata.is_empty())
        );
    }

    #[test]
    fn routine_state_reuses_step_run_id_for_logical_step() {
        let mut state = RoutineState::new(RoutineInput::new("Title", "Desc"));
        let step = Slug::derive("agent_step");
        let other_step = Slug::derive("other_step");

        let first = state.step_run_id_for(&step);
        let second = state.step_run_id_for(&step);
        let other = state.step_run_id_for(&other_step);

        assert_eq!(first, second);
        assert_ne!(first, other);
    }

    #[test]
    fn metrics_accumulate() {
        let mut metrics = RoutineMetrics::new();
        let slug = Slug::derive("step");
        metrics.record_step(&slug, 100, 50);
        metrics.record_step(&slug, 200, 75);
        let step = metrics.get(&slug).unwrap();
        assert_eq!(step.execution_count, 2);
        assert_eq!(step.total_tokens(), 425);
    }
}
