//! Execution trace data model for platform harness runs.
//!
//! Hosts decide where these traces are stored. The worker currently persists
//! them to local files and optional session content blobs.

use std::collections::HashMap;

use chrono::Utc;
use nenjo::TurnEvent;
use nenjo_models::ChatMessage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::preview::summarize_preview;

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

/// One invocation of a delegated agent during a parent agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationInvocationTrace {
    pub started_at: String,
    pub completed_at: Option<String>,
    pub success: Option<bool>,
    pub target_agent_name: String,
    pub target_agent_id: Uuid,
    pub task_input: String,
    pub caller_history_snapshot: Vec<ChatMessage>,
    pub final_output: Option<String>,
    pub events: Vec<TraceEvent>,
}

/// Aggregated trace file for agent-to-agent delegations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationExecutionTrace {
    pub trace_type: String,
    pub parent_agent_name: String,
    pub delegate_tool_name: String,
    pub invocations: Vec<DelegationInvocationTrace>,
}

#[derive(Debug, Clone)]
pub struct TaskTraceLocation<'a> {
    pub project_slug: &'a str,
    pub task_slug: &'a str,
    pub step_name: Option<&'a str>,
    pub step_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceFlushTarget {
    Agent,
    Ability(String),
    Delegation(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceRecordUpdate {
    pub targets: Vec<TraceFlushTarget>,
}

impl TraceRecordUpdate {
    fn push(&mut self, target: TraceFlushTarget) {
        self.targets.push(target);
    }
}

/// Storage-neutral trace state machine.
///
/// This mutates trace documents from runtime turn events and reports which
/// documents changed. Hosts are responsible for persisting those changed
/// documents through their own storage sinks.
#[derive(Clone)]
pub struct TraceRecorderCore {
    agent_trace: AgentExecutionTrace,
    ability_traces: HashMap<String, AbilityExecutionTrace>,
    active_ability_invocations: HashMap<String, usize>,
    delegation_traces: HashMap<String, DelegationExecutionTrace>,
    active_delegation_invocations: HashMap<String, usize>,
}

impl TraceRecorderCore {
    pub fn new<AgentName>(agent_name: AgentName, agent_id: Uuid) -> Self
    where
        AgentName: Into<String>,
    {
        Self {
            agent_trace: AgentExecutionTrace {
                trace_type: "agent".into(),
                agent_name: agent_name.into(),
                agent_id: agent_id.to_string(),
                started_at: Utc::now().to_rfc3339(),
                completed_at: None,
                final_output: None,
                events: Vec::new(),
            },
            ability_traces: HashMap::new(),
            active_ability_invocations: HashMap::new(),
            delegation_traces: HashMap::new(),
            active_delegation_invocations: HashMap::new(),
        }
    }

    pub fn agent_trace(&self) -> &AgentExecutionTrace {
        &self.agent_trace
    }

    pub fn ability_trace(&self, ability_tool_name: &str) -> Option<&AbilityExecutionTrace> {
        self.ability_traces.get(ability_tool_name)
    }

    pub fn delegation_trace(&self, delegate_tool_name: &str) -> Option<&DelegationExecutionTrace> {
        self.delegation_traces.get(delegate_tool_name)
    }

    pub fn record(&mut self, event: &TurnEvent) -> TraceRecordUpdate {
        let mut update = TraceRecordUpdate::default();

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
                update.push(TraceFlushTarget::Ability(ability_tool_name.clone()));
            }
            TurnEvent::DelegationStarted {
                delegate_tool_name,
                target_agent_name,
                target_agent_id,
                task_input,
                caller_history,
            } => {
                let entry = self
                    .delegation_traces
                    .entry(delegate_tool_name.clone())
                    .or_insert_with(|| DelegationExecutionTrace {
                        trace_type: "delegation".into(),
                        parent_agent_name: self.agent_trace.agent_name.clone(),
                        delegate_tool_name: delegate_tool_name.clone(),
                        invocations: Vec::new(),
                    });
                entry.invocations.push(DelegationInvocationTrace {
                    started_at: Utc::now().to_rfc3339(),
                    completed_at: None,
                    success: None,
                    target_agent_name: target_agent_name.clone(),
                    target_agent_id: *target_agent_id,
                    task_input: task_input.clone(),
                    caller_history_snapshot: caller_history.clone(),
                    final_output: None,
                    events: Vec::new(),
                });
                self.active_delegation_invocations
                    .insert(delegate_tool_name.clone(), entry.invocations.len() - 1);
                update.push(TraceFlushTarget::Delegation(delegate_tool_name.clone()));
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
                    } else if let Some(invocation) = self.active_delegation_mut(parent) {
                        invocation
                            .events
                            .extend(calls.iter().map(|call| TraceEvent::ToolStart {
                                tool_name: call.tool_name.clone(),
                                tool_args: call.tool_args.clone(),
                                text_preview: call.text_preview.clone(),
                                started_at: now.clone(),
                            }));
                    }
                    update.push(TraceFlushTarget::Ability(parent.clone()));
                    update.push(TraceFlushTarget::Delegation(parent.clone()));
                } else {
                    self.agent_trace.events.extend(calls.iter().map(|call| {
                        TraceEvent::ToolStart {
                            tool_name: call.tool_name.clone(),
                            tool_args: call.tool_args.clone(),
                            text_preview: call.text_preview.clone(),
                            started_at: now.clone(),
                        }
                    }));
                    update.push(TraceFlushTarget::Agent);
                }
            }
            TurnEvent::ToolCallEnd {
                parent_tool_name,
                tool_name,
                tool_args: _,
                result,
                ..
            } => {
                let trace_event = TraceEvent::ToolEnd {
                    tool_name: tool_name.clone(),
                    success: result.success,
                    output_preview: summarize_preview(&result.output),
                    error_preview: result.error.as_deref().and_then(summarize_preview),
                    completed_at: Utc::now().to_rfc3339(),
                };
                if let Some(parent) = parent_tool_name {
                    if let Some(invocation) = self.active_invocation_mut(parent) {
                        invocation.events.push(trace_event);
                    } else if let Some(invocation) = self.active_delegation_mut(parent) {
                        invocation.events.push(trace_event);
                    }
                    update.push(TraceFlushTarget::Ability(parent.clone()));
                    update.push(TraceFlushTarget::Delegation(parent.clone()));
                } else {
                    self.agent_trace.events.push(trace_event);
                    update.push(TraceFlushTarget::Agent);
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
                self.active_ability_invocations.remove(ability_tool_name);
                update.push(TraceFlushTarget::Ability(ability_tool_name.clone()));
            }
            TurnEvent::DelegationCompleted {
                delegate_tool_name,
                success,
                final_output,
                ..
            } => {
                if let Some(invocation) = self.active_delegation_mut(delegate_tool_name) {
                    invocation.completed_at = Some(Utc::now().to_rfc3339());
                    invocation.success = Some(*success);
                    invocation.final_output = Some(final_output.clone());
                }
                self.active_delegation_invocations
                    .remove(delegate_tool_name);
                update.push(TraceFlushTarget::Delegation(delegate_tool_name.clone()));
            }
            TurnEvent::Done { output } => {
                self.agent_trace.completed_at = Some(Utc::now().to_rfc3339());
                self.agent_trace.final_output = Some(output.text.clone());
                update.push(TraceFlushTarget::Agent);
            }
            TurnEvent::TranscriptMessage { .. } => {}
            TurnEvent::MessageCompacted { .. } => {}
            TurnEvent::Paused | TurnEvent::Resumed => {}
        }

        update
    }

    pub fn finalize_with_error(&mut self, error: &str) -> TraceRecordUpdate {
        self.agent_trace.completed_at = Some(Utc::now().to_rfc3339());
        self.agent_trace.final_output = Some(error.to_string());
        TraceRecordUpdate {
            targets: vec![TraceFlushTarget::Agent],
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

    fn active_delegation_mut(
        &mut self,
        delegate_tool_name: &str,
    ) -> Option<&mut DelegationInvocationTrace> {
        let idx = *self.active_delegation_invocations.get(delegate_tool_name)?;
        self.delegation_traces
            .get_mut(delegate_tool_name)
            .and_then(|trace| trace.invocations.get_mut(idx))
    }
}

#[cfg(test)]
mod tests {
    use nenjo::TurnEvent;
    use nenjo_models::ChatMessage;
    use uuid::Uuid;

    use super::{AgentExecutionTrace, TraceEvent, TraceFlushTarget, TraceRecorderCore};

    #[test]
    fn trace_event_uses_tagged_shape() {
        let event = TraceEvent::ToolEnd {
            tool_name: "search".to_string(),
            success: true,
            output_preview: Some("ok".to_string()),
            error_preview: None,
            completed_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let value = serde_json::to_value(event).unwrap();

        assert_eq!(value["type"], "ToolEnd");
        assert_eq!(value["tool_name"], "search");
        assert_eq!(value["success"], true);
    }

    #[test]
    fn agent_trace_serializes_platform_harness_schema() {
        let trace = AgentExecutionTrace {
            trace_type: "agent".to_string(),
            agent_name: "agent".to_string(),
            agent_id: "agent-id".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            completed_at: None,
            final_output: None,
            events: Vec::new(),
        };

        let value = serde_json::to_value(trace).unwrap();

        assert_eq!(value["trace_type"], "agent");
        assert_eq!(value["agent_name"], "agent");
    }

    #[test]
    fn recorder_core_tracks_ability_lifecycle() {
        let mut core = TraceRecorderCore::new("agent", Uuid::new_v4());

        let update = core.record(&TurnEvent::AbilityStarted {
            ability_tool_name: "ability/review".to_string(),
            ability_name: "Review".to_string(),
            task_input: "check this".to_string(),
            caller_history: vec![ChatMessage::user("check this")],
        });

        assert_eq!(
            update.targets,
            vec![TraceFlushTarget::Ability("ability/review".to_string())]
        );
        assert_eq!(
            core.ability_trace("ability/review")
                .unwrap()
                .invocations
                .len(),
            1
        );
    }
}
