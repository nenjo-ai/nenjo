//! Routine execution types — inputs, state, step config.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::input::{RoutineRun, RoutineRunKind, TaskInput};

/// Outcome of a routine step execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepResult {
    pub passed: bool,
    pub output: String,
    pub data: serde_json::Value,
    pub step_id: Uuid,
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

// ---------------------------------------------------------------------------
// RoutineInput — caller-provided context for a routine execution
// ---------------------------------------------------------------------------

/// Input context for a routine execution.
///
/// ```ignore
/// let input = RoutineInput::new(project_id, "Implement auth", "Add JWT authentication")
///     .with_task_id(task_id)
///     .with_execution_run_id(run_id)
///     .with_tags(vec!["auth".into(), "security".into()]);
/// ```
pub struct RoutineInput {
    pub project_id: Uuid,
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
    pub fn new(project_id: Uuid, title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            project_id,
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

    pub fn with_project_context(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.project_name = Some(name.into());
        self.project_description = Some(description.into());
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
                    None => RoutineInput::new(
                        cron.project_id.unwrap_or_else(Uuid::nil),
                        "Cron",
                        "Cron-triggered routine",
                    ),
                }
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
        let mut input = RoutineInput::new(task.project_id, task.title, task.description)
            .with_tags(task.tags)
            .with_acceptance_criteria(task.acceptance_criteria)
            .with_task_id(task.task_id);
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
pub(crate) struct RoutineState {
    pub routine_id: Uuid,
    pub step_results: HashMap<Uuid, StepResult>,
    pub initial_input: String,
    pub input: RoutineInput,
    pub routine_name: Option<String>,
    pub current_step_name: Option<String>,
    pub current_step_type: Option<String>,
    pub current_agent_id: Option<Uuid>,
    pub gate_feedback: Option<String>,
    pub step_instructions: Option<String>,
    pub step_metadata: Option<String>,
    pub metrics: RoutineMetrics,
}

impl RoutineState {
    pub fn new(routine_id: Uuid, input: RoutineInput) -> Self {
        let initial_input = input.description.clone();
        Self {
            routine_id,
            step_results: HashMap::new(),
            initial_input,
            input,
            routine_name: None,
            current_step_name: None,
            current_step_type: None,
            current_agent_id: None,
            gate_feedback: None,
            step_instructions: None,
            step_metadata: None,
            metrics: RoutineMetrics::new(),
        }
    }
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
    Cron,
    Gate,
    Lambda,
    Terminal,
    TerminalFail,
}

impl StepType {
    pub fn from_str_value(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "council" => Self::Council,
            "cron" => Self::Cron,
            "gate" => Self::Gate,
            "lambda" => Self::Lambda,
            "terminal" => Self::Terminal,
            "terminal_fail" => Self::TerminalFail,
            _ => Self::Agent,
        }
    }
}

// ---------------------------------------------------------------------------
// CronStepConfig / LambdaStepConfig
// ---------------------------------------------------------------------------

/// Execution mode for a cron step.
#[derive(Debug, Clone)]
pub enum CronMode {
    Agent(Uuid),
    Lambda(Uuid),
}

/// Configuration for a cron-type routine step.
pub struct CronStepConfig {
    pub interval: Duration,
    pub timeout: Duration,
    pub mode: CronMode,
}

impl CronStepConfig {
    pub fn from_config(
        config: &serde_json::Value,
        agent_id: Option<Uuid>,
        lambda_id: Option<Uuid>,
    ) -> Result<Self> {
        let interval = config
            .get("interval")
            .and_then(|v| v.as_str())
            .map(parse_duration)
            .transpose()?
            .unwrap_or(Duration::from_secs(60));

        let timeout = config
            .get("timeout")
            .and_then(|v| v.as_str())
            .map(parse_duration)
            .transpose()?
            .unwrap_or(Duration::from_secs(24 * 3600));

        let resolved_lambda = lambda_id.or_else(|| {
            config
                .get("lambda_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        });

        let resolved_agent = agent_id.or_else(|| {
            config
                .get("agent_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        });

        let mode = if let Some(lid) = resolved_lambda {
            CronMode::Lambda(lid)
        } else if let Some(aid) = resolved_agent {
            CronMode::Agent(aid)
        } else {
            bail!("Cron step requires either an agent_id or a lambda_id");
        };

        Ok(Self {
            interval,
            timeout,
            mode,
        })
    }
}

/// Configuration for a lambda-type routine step.
pub struct LambdaStepConfig {
    pub lambda_id: Uuid,
    pub interpreter: Option<String>,
    pub timeout: Duration,
}

impl LambdaStepConfig {
    pub fn from_config(config: &serde_json::Value, lambda_id: Option<Uuid>) -> Result<Self> {
        let lambda_id = lambda_id
            .or_else(|| {
                config
                    .get("lambda_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .ok_or_else(|| anyhow::anyhow!("Lambda step requires a lambda_id"))?;

        let interpreter = config
            .get("interpreter_override")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let timeout = config
            .get("timeout")
            .and_then(|v| v.as_str())
            .map(parse_duration)
            .transpose()?
            .unwrap_or(Duration::from_secs(300));

        Ok(Self {
            lambda_id,
            interpreter,
            timeout,
        })
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
#[derive(Debug, Default)]
pub struct RoutineMetrics {
    steps: HashMap<Uuid, StepMetrics>,
}

impl RoutineMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_step(&mut self, step_id: Uuid, input_tokens: u64, output_tokens: u64) {
        let entry = self.steps.entry(step_id).or_default();
        entry.execution_count += 1;
        entry.input_tokens += input_tokens;
        entry.output_tokens += output_tokens;
    }

    pub fn get(&self, step_id: &Uuid) -> Option<&StepMetrics> {
        self.steps.get(step_id)
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
        assert_eq!(StepType::from_str_value("lambda"), StepType::Lambda);
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
    fn cron_config_defaults() {
        let id = Uuid::new_v4();
        let config = serde_json::json!({});
        let cron = CronStepConfig::from_config(&config, Some(id), None).unwrap();
        assert_eq!(cron.interval, Duration::from_secs(60));
        assert_eq!(cron.timeout, Duration::from_secs(86400));
        assert!(matches!(cron.mode, CronMode::Agent(aid) if aid == id));
    }

    #[test]
    fn cron_config_lambda_precedence() {
        let agent_id = Uuid::new_v4();
        let lambda_id = Uuid::new_v4();
        let config = serde_json::json!({});
        let cron = CronStepConfig::from_config(&config, Some(agent_id), Some(lambda_id)).unwrap();
        assert!(matches!(cron.mode, CronMode::Lambda(lid) if lid == lambda_id));
    }

    #[test]
    fn cron_config_missing_both() {
        let config = serde_json::json!({});
        assert!(CronStepConfig::from_config(&config, None, None).is_err());
    }

    #[test]
    fn routine_input_builder() {
        let pid = Uuid::new_v4();
        let input = RoutineInput::new(pid, "Title", "Desc")
            .with_tags(vec!["a".into()])
            .with_cron_trigger();
        assert_eq!(input.project_id, pid);
        assert_eq!(input.title, "Title");
        assert!(input.is_cron_trigger);
        assert_eq!(input.tags, vec!["a"]);
    }

    #[test]
    fn metrics_accumulate() {
        let mut metrics = RoutineMetrics::new();
        let id = Uuid::new_v4();
        metrics.record_step(id, 100, 50);
        metrics.record_step(id, 200, 75);
        let step = metrics.get(&id).unwrap();
        assert_eq!(step.execution_count, 2);
        assert_eq!(step.total_tokens(), 425);
    }
}
