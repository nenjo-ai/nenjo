//! Error types for agent construction and execution.

/// Errors returned by agent construction and execution.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Agent has abilities but no manifest was provided to resolve them.
    #[error("agent '{0}' has abilities but no delegation support (manifest required)")]
    MissingManifest(String),

    /// An error from the underlying execution (turn loop, etc.).
    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}
