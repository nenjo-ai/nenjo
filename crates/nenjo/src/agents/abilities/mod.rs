//! Assigned ability discovery and isolated child execution.
//!
//! `broker` exposes the stable discovery/invocation tools, `instance` assembles
//! the scoped child, `execution` owns its lifecycle, and `events` records and
//! forwards child activity. Generic operation controls live in `async_ops`.

mod broker;
mod child_tools;
mod events;
mod execution;
mod instance;
mod registry;

pub use broker::{ListAssignedAbilitiesTool, UseAbilityTool};
pub(crate) use broker::{build_ability_tools, is_ability_tool};

use crate::tools::ToolResult;

/// Tool name used to discover the current agent's assigned abilities.
pub const LIST_ASSIGNED_ABILITIES_TOOL_NAME: &str = "list_assigned_abilities";
/// Tool name used to start an assigned ability with a self-contained task.
pub const USE_ABILITY_TOOL_NAME: &str = "use_ability";
/// Required terminal tool for an ability's structured result.
pub(crate) const FINISH_ABILITY_TOOL_NAME: &str = "finish";

const ABILITY_COMPLETION_GUIDANCE: &str = "Complete this ability execution through the `finish` tool. Ordinary assistant prose does not end an ability. Use status `completed` only after the requested work and its required verification are complete. Use status `failed` when the work cannot be completed, and explain the concrete blocker. Use `ask_parent_agent` instead when parent input could unblock the work.";

/// Encode a successful harness tool result without changing the wire format.
fn json_tool(value: serde_json::Value) -> ToolResult {
    ToolResult {
        success: true,
        output: value.to_string().into(),
        error: None,
    }
}

#[cfg(test)]
mod tests;
