//! Shared runtime domain types used by agent execution.

use std::collections::HashSet;

use crate::Slug;
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
    /// Agent slugs already visited in this delegation chain.
    pub ancestor_agent_slugs: HashSet<Slug>,
}

impl DelegationContext {
    /// Create a new root delegation context with the given max depth.
    pub fn new(max_depth: u32) -> Self {
        Self {
            current_depth: 0,
            max_depth,
            ancestor_agent_slugs: HashSet::new(),
        }
    }

    /// Create a child context for a delegated agent. Returns `None` if max depth reached.
    pub fn child(&self, parent_slug: &Slug) -> Option<Self> {
        let next_depth = self.current_depth + 1;
        if next_depth >= self.max_depth {
            return None;
        }
        let mut ancestors = self.ancestor_agent_slugs.clone();
        ancestors.insert(parent_slug.clone());
        Some(Self {
            current_depth: next_depth,
            max_depth: self.max_depth,
            ancestor_agent_slugs: ancestors,
        })
    }

    /// Check if delegating to the target would create a cycle.
    pub fn would_cycle(&self, target_slug: &Slug) -> bool {
        self.ancestor_agent_slugs.contains(target_slug)
    }
}

/// Active domain state carried across turns within a domain session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveDomain {
    /// Unique ID for the active domain session.
    pub session_id: Uuid,
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
        assert!(ctx.ancestor_agent_slugs.is_empty());
    }

    #[test]
    fn delegation_context_child_increments_depth() {
        let ctx = DelegationContext::new(3);
        let parent_slug = crate::Slug::derive("parent");
        let child = ctx.child(&parent_slug).unwrap();
        assert_eq!(child.current_depth, 1);
        assert!(child.ancestor_agent_slugs.contains(&parent_slug));
    }

    #[test]
    fn delegation_context_max_depth_blocks() {
        let ctx = DelegationContext::new(2);
        let slug1 = crate::Slug::derive("agent_1");
        let child = ctx.child(&slug1).unwrap();
        assert_eq!(child.current_depth, 1);
        // depth 1 + 1 = 2 >= max_depth 2, so child returns None
        let slug2 = crate::Slug::derive("agent_2");
        assert!(child.child(&slug2).is_none());
    }

    #[test]
    fn delegation_context_cycle_detection() {
        let ctx = DelegationContext::new(5);
        let slug1 = crate::Slug::derive("agent_1");
        let child = ctx.child(&slug1).unwrap();
        assert!(child.would_cycle(&slug1));
        assert!(!child.would_cycle(&crate::Slug::derive("agent_2")));
    }
}
