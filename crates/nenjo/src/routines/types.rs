//! Routine execution types — inputs, state, step config.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
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
///     .with_tags(vec!["auth".into(), "security".into()]);
/// ```
#[derive(Clone)]
pub struct RoutineInput {
    pub project: Option<Slug>,
    pub title: String,
    pub description: String,
    pub task_id: Option<Uuid>,
    pub execution_run_id: Option<Uuid>,
    pub acceptance_criteria: Option<String>,
    pub tags: Vec<String>,
    pub slug: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub task_type: Option<String>,
    pub complexity: Option<String>,
    pub source: Option<String>,
    pub git: Option<crate::types::GitContext>,
    pub project_name: Option<String>,
    pub project_description: Option<String>,
    pub project_metadata: Option<String>,
    pub is_cron_trigger: bool,
    pub session_binding: Option<SessionBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBinding {
    pub session_id: Uuid,
    pub memory_namespace: Option<String>,
}

impl RoutineInput {
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            project: None,
            title: title.into(),
            description: description.into(),
            task_id: None,
            execution_run_id: None,
            acceptance_criteria: None,
            tags: Vec::new(),
            slug: None,
            status: None,
            priority: None,
            task_type: None,
            complexity: None,
            source: None,
            git: None,
            project_name: None,
            project_description: None,
            project_metadata: None,
            is_cron_trigger: false,
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

    pub fn with_acceptance_criteria(mut self, criteria: Option<String>) -> Self {
        self.acceptance_criteria = criteria;
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
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

    pub fn with_task_type(mut self, task_type: impl Into<String>) -> Self {
        self.task_type = Some(task_type.into());
        self
    }

    pub fn with_complexity(mut self, complexity: impl Into<String>) -> Self {
        self.complexity = Some(complexity.into());
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
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

    pub fn with_cron_trigger(mut self) -> Self {
        self.is_cron_trigger = true;
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
            RoutineRunKind::Cron(cron) => {
                let location = run.execution.project_location;
                let mut input = match cron.task {
                    Some(task) => RoutineInput::from_task_input(task),
                    None => {
                        let mut input = RoutineInput::new("Cron", "Cron-triggered routine");
                        input.project = cron.project;
                        input
                    }
                };
                input = input
                    .with_git(location.and_then(|location| location.git))
                    .with_cron_trigger()
                    .with_execution_run_id_opt(run.execution.execution_run_id);
                if let Some(binding) = run.execution.session_binding {
                    input = input.with_session_binding(binding);
                }
                input
            }
        }
    }

    fn from_task_input(task: TaskInput) -> Self {
        let mut input = RoutineInput::new(task.title, task.description)
            .with_tags(task.tags)
            .with_acceptance_criteria(task.acceptance_criteria)
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
        if let Some(task_type) = task.task_type {
            input = input.with_task_type(task_type);
        }
        if let Some(complexity) = task.complexity {
            input = input.with_complexity(complexity);
        }
        if let Some(source) = task.source {
            input = input.with_source(source);
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
    pub gate_feedback: Option<String>,
    pub step_instructions: Option<String>,
    pub step_metadata: Option<String>,
    pub metrics: RoutineMetrics,
}

impl RoutineState {
    pub fn new(input: RoutineInput) -> Self {
        let initial_input = input.description.clone();
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
            gate_feedback: None,
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

#[derive(Debug, Clone)]
pub(crate) struct RoutineHandoff {
    pub source_step: Slug,
    pub target_step: Slug,
    pub handoff: serde_json::Value,
    pub purpose: Option<String>,
    pub summary: Option<String>,
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

/// A cron schedule — either a fixed interval or a cron expression.
#[derive(Debug, Clone)]
pub enum CronSchedule {
    /// Fixed interval between cycles (e.g. "30s", "5m").
    Interval(Duration),
    /// Standard cron expression (e.g. "0 9 * * *").
    Expression {
        schedule: Box<cron::Schedule>,
        timezone: chrono_tz::Tz,
    },
}

impl CronSchedule {
    /// Compute the next fire time in UTC.
    pub fn next_fire_at(&self) -> chrono::DateTime<chrono::Utc> {
        match self {
            CronSchedule::Interval(d) => {
                chrono::Utc::now()
                    + chrono::Duration::from_std(*d)
                        .unwrap_or_else(|_| chrono::Duration::seconds(60))
            }
            CronSchedule::Expression { schedule, timezone } => schedule
                .upcoming(*timezone)
                .next()
                .map(|value| value.with_timezone(&chrono::Utc))
                .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::seconds(60)),
        }
    }

    /// Compute the duration to sleep until the next fire time.
    /// For fixed intervals this returns the interval directly.
    /// For cron expressions it computes the delay until the next upcoming time.
    pub fn next_delay(&self) -> Duration {
        let delta = self.next_fire_at() - chrono::Utc::now();
        delta.to_std().unwrap_or(Duration::from_secs(60))
    }
}

/// Parse a schedule string — either a cron expression ("0 9 * * *") or a
/// simple duration string ("30s", "5m", "1h", "2d").
///
/// Cron expressions are detected by the presence of spaces (at least 4
/// space-separated fields). Everything else is parsed as a duration.
pub fn parse_schedule(s: &str) -> Result<CronSchedule> {
    parse_schedule_in_timezone(s, None)
}

pub fn parse_schedule_in_timezone(s: &str, timezone: Option<&str>) -> Result<CronSchedule> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Empty schedule string");
    }

    // Cron expressions have at least 4 space-separated fields.
    if s.split_whitespace().count() >= 4 {
        // The `cron` crate expects a 7-field format (sec min hour dom month dow year).
        // Standard 5-field cron ("min hour dom month dow") needs sec + year padding.
        let expr = match s.split_whitespace().count() {
            5 => format!("0 {s} *"), // sec=0, append year=*
            6 => format!("0 {s}"),   // prepend sec=0
            _ => s.to_string(),      // already 7-field
        };
        let schedule: cron::Schedule = expr
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid cron expression '{}': {}", s, e))?;
        let timezone_name = timezone
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("UTC");
        let timezone = timezone_name
            .parse::<chrono_tz::Tz>()
            .map_err(|_| anyhow::anyhow!("Invalid timezone '{}'", timezone_name))?;
        Ok(CronSchedule::Expression {
            schedule: Box::new(schedule),
            timezone,
        })
    } else {
        parse_duration(s).map(CronSchedule::Interval)
    }
}

/// Parse a simple duration string: "30s", "5m", "1h", "2d".
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Empty duration string");
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let value: u64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid duration number: '{}'", num_str))?;
    if value == 0 {
        bail!("Schedule duration must be greater than zero");
    }

    let secs = match suffix {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        "d" => value * 86400,
        _ => bail!("Invalid duration suffix '{}', expected s/m/h/d", suffix),
    };

    Ok(Duration::from_secs(secs))
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
    fn parse_duration_all_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(172800));
    }

    #[test]
    fn parse_duration_invalid() {
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("abcs").is_err());
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn parse_schedule_interval() {
        let s = parse_schedule("30s").unwrap();
        assert!(matches!(s, CronSchedule::Interval(d) if d == Duration::from_secs(30)));
        let s = parse_schedule("5m").unwrap();
        assert!(matches!(s, CronSchedule::Interval(d) if d == Duration::from_secs(300)));
    }

    #[test]
    fn parse_schedule_rejects_zero_interval() {
        let error = parse_schedule("0s").expect_err("zero interval should be rejected");
        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn parse_schedule_cron_expression() {
        // Standard 5-field cron
        let s = parse_schedule("0 9 * * *").unwrap();
        assert!(matches!(s, CronSchedule::Expression { .. }));
        // Every 5 minutes
        let s = parse_schedule("*/5 * * * *").unwrap();
        assert!(matches!(s, CronSchedule::Expression { .. }));
        // Next delay should be positive and finite
        let delay = s.next_delay();
        assert!(delay.as_secs() > 0);
        assert!(delay.as_secs() <= 300);
    }

    #[test]
    fn parse_schedule_cron_expression_timezone() {
        let s = parse_schedule_in_timezone("0 9 * * *", Some("America/Chicago")).unwrap();
        assert!(
            matches!(s, CronSchedule::Expression { timezone, .. } if timezone == chrono_tz::America::Chicago)
        );
        assert!(parse_schedule_in_timezone("0 9 * * *", Some("Not/AZone")).is_err());
    }

    #[test]
    fn parse_schedule_invalid() {
        assert!(parse_schedule("").is_err());
        assert!(parse_schedule("not a schedule").is_err());
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
            .with_tags(vec!["a".into()])
            .with_cron_trigger();
        assert_eq!(
            input.project.as_ref().map(Slug::as_str),
            Some("demo_project")
        );
        assert_eq!(input.title, "Title");
        assert!(input.is_cron_trigger);
        assert_eq!(input.tags, vec!["a"]);
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
