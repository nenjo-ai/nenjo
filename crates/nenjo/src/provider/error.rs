//! Error types for [`Provider`](super::Provider) operations.

/// Errors returned by [`Provider`](super::Provider) operations.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The requested agent was not found in the manifest.
    #[error("agent not found: {0}")]
    AgentNotFound(String),

    /// The requested routine was not found in the manifest.
    #[error("routine not found: {0}")]
    RoutineNotFound(String),

    /// The agent's assigned model was not found in the manifest.
    #[error("model not found: {0}")]
    ModelNotFound(String),

    /// The model provider factory failed to create an LLM provider.
    #[error("model provider factory failed: {0}")]
    FactoryFailed(#[source] anyhow::Error),

    /// An error from the underlying execution (turn loop, routine DAG, etc.).
    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}
