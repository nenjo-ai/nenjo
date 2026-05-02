//! DelegateToTool — first-class agent-to-agent delegation.
//!
//! Allows an agent to delegate a subtask to another agent by name.
//! Uses [`DelegationContext`] for cycle detection and depth limiting.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info, warn};
use uuid::Uuid;

use nenjo_tools::{Tool, ToolCategory, ToolResult};

use super::runner::turn_loop;
use super::runner::types::TurnEvent;
use crate::config::AgentConfig;
use crate::manifest::Manifest;
use crate::memory::Memory;
use crate::provider::{ModelProviderFactory, Provider, ToolFactory};
use crate::types::DelegationContext;

/// Required parameters for constructing a [`DelegateToTool`].
pub(crate) struct DelegateToToolParams {
    pub manifest: Arc<Manifest>,
    pub model_factory: Arc<dyn ModelProviderFactory>,
    pub tool_factory: Arc<dyn ToolFactory>,
    pub memory: Option<Arc<dyn Memory>>,
    pub agent_config: AgentConfig,
    pub lambda_runner: Option<Arc<dyn crate::routines::LambdaRunner>>,
    pub caller_agent_id: Uuid,
    pub delegation_ctx: DelegationContext,
}

/// A tool that delegates a subtask to another agent by name.
///
/// Automatically injected into agents when other agents are available and
/// `max_delegation_depth > 0`. Uses [`DelegationContext`] to prevent cycles
/// and limit nesting depth.
///
/// The tool holds the Provider's factory Arcs directly so it can construct
/// target agents without needing an `Arc<Provider>`.
pub struct DelegateToTool {
    manifest: Arc<Manifest>,
    model_factory: Arc<dyn ModelProviderFactory>,
    tool_factory: Arc<dyn ToolFactory>,
    memory: Option<Arc<dyn Memory>>,
    agent_config: AgentConfig,
    lambda_runner: Option<Arc<dyn crate::routines::LambdaRunner>>,
    caller_agent_id: Uuid,
    delegation_ctx: DelegationContext,
}

impl DelegateToTool {
    /// Create a new DelegateToTool.
    ///
    /// Called by `AgentRunner::new()` when delegation is enabled.
    pub(crate) fn new(params: DelegateToToolParams) -> Self {
        Self {
            manifest: params.manifest,
            model_factory: params.model_factory,
            tool_factory: params.tool_factory,
            memory: params.memory,
            agent_config: params.agent_config,
            lambda_runner: params.lambda_runner,
            caller_agent_id: params.caller_agent_id,
            delegation_ctx: params.delegation_ctx,
        }
    }

    /// Build a temporary Provider from the stored Arcs to look up and run
    /// the target agent.
    fn build_provider(&self) -> Provider {
        Provider::from_manifest_raw(
            self.manifest.clone(),
            self.model_factory.clone(),
            self.tool_factory.clone(),
            self.memory.clone(),
            self.agent_config.clone(),
            self.lambda_runner.clone(),
        )
    }
}

#[async_trait]
impl Tool for DelegateToTool {
    fn name(&self) -> &str {
        "delegate_to"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to another agent. The target agent will execute the task \
         independently and return its result. Use this when the task requires a different \
         agent's expertise or capabilities."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn parameters_schema(&self) -> serde_json::Value {
        // Build the list of available agent names for the description.
        let agent_names: Vec<&str> = self
            .manifest
            .agents
            .iter()
            .filter(|a| a.id != self.caller_agent_id)
            .map(|a| a.name.as_str())
            .collect();

        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": format!(
                        "Name of the agent to delegate to. Available agents: {}",
                        agent_names.join(", ")
                    )
                },
                "task": {
                    "type": "string",
                    "description": "Clear description of the subtask for the delegate agent to execute"
                }
            },
            "required": ["agent_name", "task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let agent_name = match args.get("agent_name").and_then(|v| v.as_str()) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("agent_name is required and must be non-empty".into()),
                });
            }
        };

        let task = match args.get("task").and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("task is required and must be non-empty".into()),
                });
            }
        };

        // Look up the target agent.
        let target_agent = match self.manifest.agents.iter().find(|a| a.name == agent_name) {
            Some(a) => a,
            None => {
                let available: Vec<&str> = self
                    .manifest
                    .agents
                    .iter()
                    .filter(|a| a.id != self.caller_agent_id)
                    .map(|a| a.name.as_str())
                    .collect();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Agent '{}' not found. Available: {}",
                        agent_name,
                        available.join(", ")
                    )),
                });
            }
        };

        let target_id = target_agent.id;

        // Cycle detection: would delegating to this agent create a cycle?
        if self.delegation_ctx.would_cycle(target_id) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Cannot delegate to '{}': would create a delegation cycle",
                    agent_name
                )),
            });
        }

        // Depth limiting: can we go one level deeper?
        let child_ctx = match self.delegation_ctx.child(self.caller_agent_id) {
            Some(ctx) => ctx,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Cannot delegate to '{}': maximum delegation depth ({}) reached",
                        agent_name, self.delegation_ctx.max_depth
                    )),
                });
            }
        };

        info!(
            caller = %self.caller_agent_id,
            target = %agent_name,
            depth = child_ctx.current_depth,
            "Delegating subtask to agent"
        );

        // Build a temporary Provider and run the target agent.
        // Pass the child delegation context so the sub-agent's DelegateToTool
        // has decremented depth (preventing infinite delegation chains).
        let provider = self.build_provider();

        let mut builder = match provider.agent_by_name(&agent_name).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build agent '{}': {}", agent_name, e)),
                });
            }
        };

        // Override the delegation context with the child context so depth
        // decrements correctly across nested delegations.
        builder = builder.with_child_delegation_ctx(child_ctx);

        let runner = builder.build().await?;

        let tool_specs = runner.instance().tool_specs();
        let tool_names: Vec<&str> = tool_specs.iter().map(|t| t.name.as_str()).collect();
        debug!(
            target_agent = %agent_name,
            caller = %self.caller_agent_id,
            tool_count = tool_specs.len(),
            tools = ?tool_names,
            "Delegated agent prompt"
        );
        debug!(
            "{}",
            runner
                .instance()
                .build_prompts(&crate::types::TaskType::Chat {
                    user_message: task.clone(),
                    history: vec![],
                    project_id: uuid::Uuid::nil(),
                })
        );

        let delegate_tool_name = self.name().to_string();
        let caller_history_snapshot = turn_loop::current_chat_history().unwrap_or_default();
        if let Some(parent_tx) = turn_loop::current_events_tx() {
            let _ = parent_tx.send(TurnEvent::DelegationStarted {
                delegate_tool_name: delegate_tool_name.clone(),
                target_agent_name: agent_name.clone(),
                target_agent_id: target_id,
                task_input: task.clone(),
                caller_history: caller_history_snapshot,
            });
        }

        let mut handle = match runner.chat_stream(&task).await {
            Ok(handle) => handle,
            Err(e) => {
                let error = format!("Delegation to '{}' failed: {}", agent_name, e);
                if let Some(parent_tx) = turn_loop::current_events_tx() {
                    let _ = parent_tx.send(TurnEvent::DelegationCompleted {
                        delegate_tool_name,
                        target_agent_name: agent_name.clone(),
                        target_agent_id: target_id,
                        success: false,
                        final_output: error.clone(),
                    });
                }
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error),
                });
            }
        };

        let parent_events_tx = turn_loop::current_events_tx();
        while let Some(event) = handle.recv().await {
            let Some(parent_tx) = parent_events_tx.as_ref() else {
                continue;
            };
            match event {
                TurnEvent::ToolCallStart {
                    parent_tool_name,
                    calls,
                } => {
                    let _ = parent_tx.send(TurnEvent::ToolCallStart {
                        parent_tool_name: parent_tool_name
                            .or_else(|| Some(delegate_tool_name.clone())),
                        calls,
                    });
                }
                TurnEvent::ToolCallEnd {
                    parent_tool_name,
                    tool_name,
                    result,
                } => {
                    let _ = parent_tx.send(TurnEvent::ToolCallEnd {
                        parent_tool_name: parent_tool_name
                            .or_else(|| Some(delegate_tool_name.clone())),
                        tool_name,
                        result,
                    });
                }
                TurnEvent::AbilityStarted { .. }
                | TurnEvent::AbilityCompleted { .. }
                | TurnEvent::DelegationStarted { .. }
                | TurnEvent::DelegationCompleted { .. }
                | TurnEvent::MessageCompacted { .. } => {
                    let _ = parent_tx.send(event);
                }
                TurnEvent::TranscriptMessage { .. }
                | TurnEvent::Paused
                | TurnEvent::Resumed
                | TurnEvent::Done { .. } => {}
            }
        }

        match handle.output().await {
            Ok(output) => {
                debug!(
                    target = %agent_name,
                    tokens_in = output.input_tokens,
                    tokens_out = output.output_tokens,
                    "Delegation completed"
                );
                if let Some(parent_tx) = turn_loop::current_events_tx() {
                    let _ = parent_tx.send(TurnEvent::DelegationCompleted {
                        delegate_tool_name,
                        target_agent_name: agent_name.clone(),
                        target_agent_id: target_id,
                        success: true,
                        final_output: output.text.clone(),
                    });
                }
                Ok(ToolResult {
                    success: true,
                    output: output.text,
                    error: None,
                })
            }
            Err(e) => {
                warn!(target = %agent_name, error = %e, "Delegation failed");
                let error = format!("Delegation to '{}' failed: {}", agent_name, e);
                if let Some(parent_tx) = turn_loop::current_events_tx() {
                    let _ = parent_tx.send(TurnEvent::DelegationCompleted {
                        delegate_tool_name,
                        target_agent_name: agent_name.clone(),
                        target_agent_id: target_id,
                        success: false,
                        final_output: error.clone(),
                    });
                }
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error),
                })
            }
        }
    }
}
