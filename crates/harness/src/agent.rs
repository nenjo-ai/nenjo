//! Agent type re-exports from the nenjo SDK.
//!
//! This module bridges the harness to the nenjo crate's agent types,
//! providing a single import path for all agent-related types.

// Core execution types
pub use nenjo::AgentBuilder;
pub use nenjo::AgentInstance;
pub use nenjo::AgentRunner;
pub use nenjo::ExecutionHandle;
pub use nenjo::TurnEvent;
pub use nenjo::TurnOutput;

// Task and step types
pub use nenjo::StepResult;
pub use nenjo::types::{
    ActiveDomain, DelegationContext, DomainArtifactConfig, DomainPromptConfig, DomainSessionConfig,
    DomainSessionManifest, DomainToolConfig, Task, TaskType, TurnOutcome,
};

// Prompt types
pub use nenjo::agents::prompts::{MemoryProfile, PromptConfig, PromptContext, PromptTemplates};

// Render context
pub use nenjo::context::RenderContextVars;

// Re-export the RenderContextExt trait from types so the harness can use it.
pub use nenjo::types::RenderContextExt;
