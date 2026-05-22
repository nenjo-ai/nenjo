use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum SubAgentError {
    #[error("invalid sub-agent slug: {0}")]
    InvalidSlug(String),
    #[error("invalid result field name: {0}")]
    InvalidResultFieldName(String),
    #[error("agent '{0}' not found")]
    AgentNotFound(String),
    #[error("cannot spawn '{0}': would create a sub-agent cycle")]
    Cycle(String),
    #[error("cannot spawn '{0}': maximum sub-agent depth reached")]
    DepthLimit(String),
}
