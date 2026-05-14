//! Error types for agent construction and execution.

/// Errors returned by agent construction and execution.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The builder was asked to build before an agent manifest was provided.
    #[error("agent manifest is required")]
    MissingAgentManifest,

    /// The builder was asked to build before model metadata was provided.
    #[error("model manifest is required")]
    MissingModelManifest,

    /// The builder was asked to build without an explicit model provider and
    /// the backing Provider could not create one.
    #[error("model provider is required")]
    MissingModelProvider,

    /// Agent has abilities but no manifest was provided to resolve them.
    #[error("agent '{0}' has abilities but no delegation support (manifest required)")]
    MissingManifest(String),

    /// An error from the backing Provider.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),

    /// An error from the underlying execution (turn loop, etc.).
    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}
