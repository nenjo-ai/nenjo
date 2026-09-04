//! Typed runtime context used to build session and turn context messages.

use std::collections::HashMap;

use crate::context::TaskContext;

use super::types::{GitContext, ProjectContext, RoutineContext};

/// Runtime-owned data kept separate from authored static prompt variables.
#[derive(Debug, Clone, Default)]
pub struct RenderContextVars {
    pub task: TaskContext,
    pub project: ProjectContext,
    pub routine: RoutineContext,
    pub git: GitContext,
    /// Static knowledge-pack summaries available while compiling instructions.
    pub knowledge_vars: HashMap<String, String>,
}
