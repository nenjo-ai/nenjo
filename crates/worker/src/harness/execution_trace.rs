//! Local execution trace persistence for chat, task, and nested ability runs.
//!
//! These traces are worker-local inspection artifacts. They are not part of the
//! canonical chat history persisted to the platform and are intended for
//! debugging, auditability, and future agent-side inspection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use nenjo::TurnEvent;
use nenjo_models::ChatMessage;
use nenjo_sessions::SessionContentStore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::harness::preview::summarize_preview;

/// Controls whether traces are stored for a chat session or a task execution.
#[derive(Debug, Clone, Copy)]
pub enum TraceMode {
    Chat { session_id: Uuid },
    Task { enabled: bool },
}

/// A single persisted tool lifecycle event inside an execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TraceEvent {
    ToolStart {
        tool_name: String,
        tool_args: String,
        text_preview: Option<String>,
        started_at: String,
    },
    ToolEnd {
        tool_name: String,
        success: bool,
        output_preview: Option<String>,
        error_preview: Option<String>,
        completed_at: String,
    },
}

/// Top-level trace for an agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExecutionTrace {
    pub trace_type: String,
    pub agent_name: String,
    pub agent_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub final_output: Option<String>,
    pub events: Vec<TraceEvent>,
}

/// One invocation of an ability during a parent agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityInvocationTrace {
    pub started_at: String,
    pub completed_at: Option<String>,
    pub success: Option<bool>,
    pub task_input: String,
    pub caller_history_snapshot: Vec<ChatMessage>,
    pub final_output: Option<String>,
    pub events: Vec<TraceEvent>,
}

/// Aggregated trace file for a specific ability tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityExecutionTrace {
    pub trace_type: String,
    pub parent_agent_name: String,
    pub ability_tool_name: String,
    pub ability_name: String,
    pub invocations: Vec<AbilityInvocationTrace>,
}

#[derive(Debug, Clone)]
pub struct TaskTraceLocation<'a> {
    pub project_slug: &'a str,
    pub task_slug: &'a str,
    pub step_name: Option<&'a str>,
    pub step_id: Option<Uuid>,
}

#[derive(Clone)]
struct TraceInit {
    mode: TraceMode,
    project_slug: String,
    task_slug: Option<String>,
    step_name: Option<String>,
    step_id: Option<Uuid>,
    workspace_dir: PathBuf,
    agent_trace_path: PathBuf,
    agent_trace_key: Option<String>,
    content_store: Option<Arc<dyn SessionContentStore>>,
    agent_name: String,
    agent_id: Uuid,
}

/// Records local execution traces for one agent run and its nested abilities.
pub struct ExecutionTraceRecorder {
    mode: TraceMode,
    project_slug: String,
    task_slug: Option<String>,
    step_name: Option<String>,
    step_id: Option<Uuid>,
    workspace_dir: PathBuf,
    agent_trace_path: PathBuf,
    agent_trace_key: Option<String>,
    content_store: Option<Arc<dyn SessionContentStore>>,
    agent_trace: AgentExecutionTrace,
    ability_traces: HashMap<String, AbilityExecutionTrace>,
    active_ability_invocations: HashMap<String, usize>,
}

impl ExecutionTraceRecorder {
    /// Create a trace recorder for a chat session.
    pub fn for_chat(
        workspace_dir: &Path,
        project_slug: &str,
        agent_name: &str,
        agent_id: Uuid,
        session_id: Uuid,
    ) -> Self {
        Self::new(TraceInit::for_chat(
            workspace_dir,
            project_slug,
            agent_name,
            agent_id,
            session_id,
        ))
    }

    pub fn for_chat_with_store(
        workspace_dir: &Path,
        project_slug: &str,
        agent_name: &str,
        agent_id: Uuid,
        session_id: Uuid,
        content_store: Arc<dyn SessionContentStore>,
    ) -> Self {
        Self::new(
            TraceInit::for_chat(
                workspace_dir,
                project_slug,
                agent_name,
                agent_id,
                session_id,
            )
            .with_content_store(content_store),
        )
    }

    /// Create a trace recorder for a task or routine-driven agent execution.
    pub fn for_task(
        workspace_dir: &Path,
        agent_name: &str,
        agent_id: Uuid,
        location: TaskTraceLocation<'_>,
        enabled: bool,
    ) -> Self {
        Self::new(TraceInit::for_task(
            workspace_dir,
            location,
            agent_name,
            agent_id,
            enabled,
        ))
    }

    pub fn for_task_with_store(
        workspace_dir: &Path,
        agent_name: &str,
        agent_id: Uuid,
        location: TaskTraceLocation<'_>,
        enabled: bool,
        content_store: Arc<dyn SessionContentStore>,
    ) -> Self {
        Self::new(
            TraceInit::for_task(workspace_dir, location, agent_name, agent_id, enabled)
                .with_content_store(content_store),
        )
    }

    fn new(init: TraceInit) -> Self {
        Self {
            mode: init.mode,
            project_slug: init.project_slug,
            task_slug: init.task_slug,
            step_name: init.step_name,
            step_id: init.step_id,
            workspace_dir: init.workspace_dir,
            agent_trace_path: init.agent_trace_path,
            agent_trace_key: init.agent_trace_key,
            content_store: init.content_store,
            agent_trace: AgentExecutionTrace {
                trace_type: "agent".into(),
                agent_name: init.agent_name,
                agent_id: init.agent_id.to_string(),
                started_at: Utc::now().to_rfc3339(),
                completed_at: None,
                final_output: None,
                events: Vec::new(),
            },
            ability_traces: HashMap::new(),
            active_ability_invocations: HashMap::new(),
        }
    }

    pub fn record(&mut self, event: &TurnEvent) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }

        match event {
            TurnEvent::AbilityStarted {
                ability_tool_name,
                ability_name,
                task_input,
                caller_history,
            } => {
                let entry = self
                    .ability_traces
                    .entry(ability_tool_name.clone())
                    .or_insert_with(|| AbilityExecutionTrace {
                        trace_type: "ability".into(),
                        parent_agent_name: self.agent_trace.agent_name.clone(),
                        ability_tool_name: ability_tool_name.clone(),
                        ability_name: ability_name.clone(),
                        invocations: Vec::new(),
                    });
                entry.invocations.push(AbilityInvocationTrace {
                    started_at: Utc::now().to_rfc3339(),
                    completed_at: None,
                    success: None,
                    task_input: task_input.clone(),
                    caller_history_snapshot: caller_history.clone(),
                    final_output: None,
                    events: Vec::new(),
                });
                self.active_ability_invocations
                    .insert(ability_tool_name.clone(), entry.invocations.len() - 1);
                self.flush_ability(ability_tool_name)?;
            }
            TurnEvent::ToolCallStart {
                parent_tool_name,
                calls,
            } => {
                let now = Utc::now().to_rfc3339();
                if let Some(parent) = parent_tool_name {
                    if let Some(invocation) = self.active_invocation_mut(parent) {
                        invocation
                            .events
                            .extend(calls.iter().map(|call| TraceEvent::ToolStart {
                                tool_name: call.tool_name.clone(),
                                tool_args: call.tool_args.clone(),
                                text_preview: call.text_preview.clone(),
                                started_at: now.clone(),
                            }));
                    }
                    self.flush_ability(parent)?;
                } else {
                    self.agent_trace.events.extend(calls.iter().map(|call| {
                        TraceEvent::ToolStart {
                            tool_name: call.tool_name.clone(),
                            tool_args: call.tool_args.clone(),
                            text_preview: call.text_preview.clone(),
                            started_at: now.clone(),
                        }
                    }));
                    self.flush_agent()?;
                }
            }
            TurnEvent::ToolCallEnd {
                parent_tool_name,
                tool_name,
                result,
            } => {
                let trace_event = TraceEvent::ToolEnd {
                    tool_name: tool_name.clone(),
                    success: result.success,
                    output_preview: first_preview(&result.output),
                    error_preview: result.error.as_deref().and_then(first_preview_str),
                    completed_at: Utc::now().to_rfc3339(),
                };
                if let Some(parent) = parent_tool_name {
                    if let Some(invocation) = self.active_invocation_mut(parent) {
                        invocation.events.push(trace_event);
                    }
                    self.flush_ability(parent)?;
                } else {
                    self.agent_trace.events.push(trace_event);
                    self.flush_agent()?;
                }
            }
            TurnEvent::AbilityCompleted {
                ability_tool_name,
                success,
                final_output,
                ..
            } => {
                if let Some(invocation) = self.active_invocation_mut(ability_tool_name) {
                    invocation.completed_at = Some(Utc::now().to_rfc3339());
                    invocation.success = Some(*success);
                    invocation.final_output = Some(final_output.clone());
                }
                self.flush_ability(ability_tool_name)?;
                self.active_ability_invocations.remove(ability_tool_name);
            }
            TurnEvent::Done { output } => {
                self.agent_trace.completed_at = Some(Utc::now().to_rfc3339());
                self.agent_trace.final_output = Some(output.text.clone());
                self.flush_agent()?;
            }
            TurnEvent::MessageCompacted { .. } => {}
            TurnEvent::Paused | TurnEvent::Resumed => {}
        }
        Ok(())
    }

    /// Mark the agent trace as failed and persist the final state.
    pub fn finalize_with_error(&mut self, error: &str) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        self.agent_trace.completed_at = Some(Utc::now().to_rfc3339());
        self.agent_trace.final_output = Some(error.to_string());
        self.flush_agent()
    }

    fn enabled(&self) -> bool {
        match self.mode {
            TraceMode::Chat { .. } => true,
            TraceMode::Task { enabled } => enabled,
        }
    }

    fn active_invocation_mut(
        &mut self,
        ability_tool_name: &str,
    ) -> Option<&mut AbilityInvocationTrace> {
        let idx = *self.active_ability_invocations.get(ability_tool_name)?;
        self.ability_traces
            .get_mut(ability_tool_name)
            .and_then(|trace| trace.invocations.get_mut(idx))
    }

    fn flush_agent(&self) -> Result<()> {
        write_json(
            &self.agent_trace_path,
            self.agent_trace_key.as_deref(),
            self.content_store.as_deref(),
            &self.agent_trace,
        )?;
        Ok(())
    }

    fn flush_ability(&self, ability_tool_name: &str) -> Result<()> {
        if let Some(trace) = self.ability_traces.get(ability_tool_name) {
            let path = self.ability_trace_path(ability_tool_name);
            let key = self.ability_trace_key(ability_tool_name);
            write_json(&path, key.as_deref(), self.content_store.as_deref(), trace)?;
        }
        Ok(())
    }

    fn ability_trace_key(&self, ability_tool_name: &str) -> Option<String> {
        let safe_agent = sanitize(&self.agent_trace.agent_name);
        let safe_ability = sanitize(ability_tool_name.trim_start_matches("ability/"));
        let step_suffix = match (&self.step_name, self.step_id) {
            (Some(name), Some(id)) => format!("_{}_{}", sanitize(name), id),
            _ => String::new(),
        };
        match self.mode {
            TraceMode::Chat { session_id } => Some(if self.project_slug.is_empty() {
                format!("chat_history/traces/{safe_agent}_{session_id}_{safe_ability}.json")
            } else {
                format!(
                    "{}/chat_history/traces/{safe_agent}_{session_id}_{safe_ability}.json",
                    self.project_slug
                )
            }),
            TraceMode::Task { .. } => Some(format!(
                "{}/execution_traces/{}/{}_{}{}.json",
                self.project_slug,
                self.task_slug.as_deref().unwrap_or("task"),
                safe_agent,
                safe_ability,
                step_suffix
            )),
        }
    }

    fn ability_trace_path(&self, ability_tool_name: &str) -> PathBuf {
        let safe_agent = sanitize(&self.agent_trace.agent_name);
        let safe_ability = sanitize(ability_tool_name.trim_start_matches("ability/"));
        let step_suffix = match (&self.step_name, self.step_id) {
            (Some(name), Some(id)) => format!("_{}_{}", sanitize(name), id),
            _ => String::new(),
        };
        match self.mode {
            TraceMode::Chat { session_id } => {
                if self.project_slug.is_empty() {
                    self.workspace_dir
                        .join("chat_history")
                        .join("traces")
                        .join(format!("{safe_agent}_{session_id}_{safe_ability}.json"))
                } else {
                    self.workspace_dir
                        .join(&self.project_slug)
                        .join("chat_history")
                        .join("traces")
                        .join(format!("{safe_agent}_{session_id}_{safe_ability}.json"))
                }
            }
            TraceMode::Task { .. } => self
                .workspace_dir
                .join(&self.project_slug)
                .join("execution_traces")
                .join(self.task_slug.as_deref().unwrap_or("task"))
                .join(format!("{safe_agent}_{safe_ability}{step_suffix}.json")),
        }
    }
}

impl TraceInit {
    fn with_content_store(mut self, content_store: Arc<dyn SessionContentStore>) -> Self {
        self.content_store = Some(content_store);
        self
    }

    fn for_chat(
        workspace_dir: &Path,
        project_slug: &str,
        agent_name: &str,
        agent_id: Uuid,
        session_id: Uuid,
    ) -> Self {
        let safe_agent = sanitize(agent_name);
        let agent_trace_path = if project_slug.is_empty() {
            workspace_dir
                .join("chat_history")
                .join("traces")
                .join(format!("{safe_agent}_{session_id}.trace.json"))
        } else {
            workspace_dir
                .join(project_slug)
                .join("chat_history")
                .join("traces")
                .join(format!("{safe_agent}_{session_id}.trace.json"))
        };
        Self {
            mode: TraceMode::Chat { session_id },
            project_slug: project_slug.to_string(),
            task_slug: None,
            step_name: None,
            step_id: None,
            workspace_dir: workspace_dir.to_path_buf(),
            agent_trace_path,
            agent_trace_key: Some(if project_slug.is_empty() {
                format!("chat_history/traces/{safe_agent}_{session_id}.trace.json")
            } else {
                format!("{project_slug}/chat_history/traces/{safe_agent}_{session_id}.trace.json")
            }),
            content_store: None,
            agent_name: agent_name.to_string(),
            agent_id,
        }
    }

    fn for_task(
        workspace_dir: &Path,
        location: TaskTraceLocation<'_>,
        agent_name: &str,
        agent_id: Uuid,
        enabled: bool,
    ) -> Self {
        let safe_agent = sanitize(agent_name);
        let step_suffix = match (location.step_name, location.step_id) {
            (Some(name), Some(id)) => format!("_{}_{}", sanitize(name), id),
            _ => String::new(),
        };
        let agent_trace_path = workspace_dir
            .join(location.project_slug)
            .join("execution_traces")
            .join(location.task_slug)
            .join(format!("{safe_agent}_{agent_id}{step_suffix}.json"));
        Self {
            mode: TraceMode::Task { enabled },
            project_slug: location.project_slug.to_string(),
            task_slug: Some(location.task_slug.to_string()),
            step_name: location.step_name.map(str::to_string),
            step_id: location.step_id,
            workspace_dir: workspace_dir.to_path_buf(),
            agent_trace_path,
            agent_trace_key: Some(format!(
                "{}/execution_traces/{}/{}_{}{}.json",
                location.project_slug, location.task_slug, safe_agent, agent_id, step_suffix
            )),
            content_store: None,
            agent_name: agent_name.to_string(),
            agent_id,
        }
    }
}

fn write_json(
    path: &Path,
    key: Option<&str>,
    content_store: Option<&dyn SessionContentStore>,
    value: &impl Serialize,
) -> Result<()> {
    if let (Some(key), Some(store)) = (key, content_store) {
        let json = serde_json::to_vec_pretty(value)?;
        store.write_blob(key, &json)?;
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn first_preview(s: &str) -> Option<String> {
    summarize_preview(s)
}

fn first_preview_str(s: &str) -> Option<String> {
    summarize_preview(s)
}
