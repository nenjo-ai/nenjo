//! Shared runtime domain types used by agent execution.

use std::collections::HashSet;

use crate::manifest::DomainManifest;
pub use crate::manifest::{
    AbilityPromptConfig, DomainManifest as DomainSessionManifest, DomainPromptConfig,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Re-export RenderContext from the agents context module.
pub use crate::context::RenderContextVars;

/// Git context for a task execution — set by the harness when the project
/// has a synced repository. Provides the agent with branch and worktree info.
#[derive(Debug, Clone, Default)]
pub struct GitContext {
    /// Branch name for this task (e.g. `agent/run-id/fix-auth`).
    pub branch: String,
    /// Target branch for PRs/merges (e.g. `main`).
    pub target_branch: String,
    /// Absolute path to the worktree directory.
    pub work_dir: String,
    /// Remote clone URL for the repository.
    pub repo_url: String,
}

/// Tracks delegation depth and prevents cycles in agent-to-agent delegation.
#[derive(Debug, Clone)]
pub struct DelegationContext {
    /// Current nesting depth for the active delegation chain.
    pub current_depth: u32,
    /// Maximum allowed nesting depth for delegation.
    pub max_depth: u32,
    /// Agent IDs already visited in this delegation chain.
    pub ancestor_agent_ids: HashSet<Uuid>,
}

impl DelegationContext {
    /// Create a new root delegation context with the given max depth.
    pub fn new(max_depth: u32) -> Self {
        Self {
            current_depth: 0,
            max_depth,
            ancestor_agent_ids: HashSet::new(),
        }
    }

    /// Create a child context for a delegated agent. Returns `None` if max depth reached.
    pub fn child(&self, parent_id: Uuid) -> Option<Self> {
        let next_depth = self.current_depth + 1;
        if next_depth >= self.max_depth {
            return None;
        }
        let mut ancestors = self.ancestor_agent_ids.clone();
        ancestors.insert(parent_id);
        Some(Self {
            current_depth: next_depth,
            max_depth: self.max_depth,
            ancestor_agent_ids: ancestors,
        })
    }

    /// Check if delegating to the target would create a cycle.
    pub fn would_cycle(&self, target_id: Uuid) -> bool {
        self.ancestor_agent_ids.contains(&target_id)
    }
}

/// Active domain state carried across turns within a domain session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveDomain {
    /// Unique ID for the active domain session.
    pub session_id: Uuid,
    /// ID of the domain manifest being used for this session.
    pub domain_id: Uuid,
    /// Human-readable domain name.
    pub domain_name: String,
    /// Domain manifest applied to the active session.
    pub manifest: DomainManifest,
}

/// Outcome of a single turn in the agent loop.
#[derive(Debug)]
pub enum TurnOutcome {
    /// The LLM returned tool calls that need execution.
    ToolCalls,
    /// The LLM returned a final text response (no tool calls).
    Final(String),
    /// The loop hit the max iteration limit.
    MaxIterations(String),
}

#[cfg(test)]
mod tests {
    use crate::routines::types::StepResult;

    use super::*;

    #[test]
    fn step_result_serde_roundtrip() {
        let result = StepResult {
            passed: true,
            output: "done".into(),
            data: serde_json::json!({"key": "value"}),
            step_name: "step-1".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: StepResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.passed);
        assert_eq!(parsed.output, "done");
    }

    #[test]
    fn delegation_context_new() {
        let ctx = DelegationContext::new(3);
        assert_eq!(ctx.current_depth, 0);
        assert_eq!(ctx.max_depth, 3);
        assert!(ctx.ancestor_agent_ids.is_empty());
    }

    #[test]
    fn delegation_context_child_increments_depth() {
        let ctx = DelegationContext::new(3);
        let parent_id = Uuid::new_v4();
        let child = ctx.child(parent_id).unwrap();
        assert_eq!(child.current_depth, 1);
        assert!(child.ancestor_agent_ids.contains(&parent_id));
    }

    #[test]
    fn delegation_context_max_depth_blocks() {
        let ctx = DelegationContext::new(2);
        let id1 = Uuid::new_v4();
        let child = ctx.child(id1).unwrap();
        assert_eq!(child.current_depth, 1);
        // depth 1 + 1 = 2 >= max_depth 2, so child returns None
        let id2 = Uuid::new_v4();
        assert!(child.child(id2).is_none());
    }

    #[test]
    fn delegation_context_cycle_detection() {
        let ctx = DelegationContext::new(5);
        let id1 = Uuid::new_v4();
        let child = ctx.child(id1).unwrap();
        assert!(child.would_cycle(id1));
        assert!(!child.would_cycle(Uuid::new_v4()));
    }
}
