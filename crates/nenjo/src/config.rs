//! Agent configuration.

use serde::{Deserialize, Serialize};

/// Default upper bound for model turns in an agent execution.
pub const DEFAULT_AGENT_MAX_TURNS: usize = 100;

/// Per-agent configuration that controls turn loop behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub compact_context: bool,
    #[serde(default = "default_context_compaction_trigger_percent")]
    pub context_compaction_trigger_percent: u8,
    #[serde(default = "default_max_model_request_payload_bytes")]
    pub max_model_request_payload_bytes: usize,
    pub parallel_tools: bool,
    #[serde(default = "default_agent_max_turns")]
    pub max_turns: usize,
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

fn default_context_compaction_trigger_percent() -> u8 {
    60
}

fn default_max_model_request_payload_bytes() -> usize {
    8 * 1024 * 1024
}

fn default_agent_max_turns() -> usize {
    DEFAULT_AGENT_MAX_TURNS
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
            context_compaction_trigger_percent: default_context_compaction_trigger_percent(),
            max_model_request_payload_bytes: default_max_model_request_payload_bytes(),
            max_turns: default_agent_max_turns(),
            max_history_messages: default_agent_max_history_messages(),
            parallel_tools: true,
            tool_dispatcher: default_agent_tool_dispatcher(),
            max_delegation_depth: default_max_delegation_depth(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::TurnLoopConfig;

    #[test]
    fn agent_and_turn_loop_defaults_share_the_hundred_turn_limit() {
        assert_eq!(AgentConfig::default().max_turns, 100);
        assert_eq!(TurnLoopConfig::default().max_turns, 100);
        assert_eq!(DEFAULT_AGENT_MAX_TURNS, 100);
    }
}
