//! Agent configuration.

use serde::{Deserialize, Serialize};

/// Per-agent configuration that controls turn loop behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub compact_context: bool,
    pub parallel_tools: bool,
    #[serde(default = "default_agent_max_tool_iterations")]
    pub max_tool_iterations: usize,
    #[serde(default = "default_agent_max_history_messages")]
    pub max_history_messages: usize,
    #[serde(default = "default_agent_tool_dispatcher")]
    pub tool_dispatcher: String,
    #[serde(default = "default_max_delegation_depth")]
    pub max_delegation_depth: u32,
}

fn default_max_delegation_depth() -> u32 {
    3
}

fn default_agent_max_tool_iterations() -> usize {
    100
}

fn default_agent_max_history_messages() -> usize {
    50
}

fn default_agent_tool_dispatcher() -> String {
    "auto".into()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            compact_context: false,
            max_tool_iterations: default_agent_max_tool_iterations(),
            max_history_messages: default_agent_max_history_messages(),
            parallel_tools: true,
            tool_dispatcher: default_agent_tool_dispatcher(),
            max_delegation_depth: default_max_delegation_depth(),
        }
    }
}
