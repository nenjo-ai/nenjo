use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum SubAgentError {
    #[error("invalid sub-agent slug: {0}")]
    InvalidSlug(String),
    #[error("invalid result field name: {0}")]
    InvalidResultFieldName(String),
    #[error("cannot spawn '{0}': maximum sub-agent depth reached")]
    DepthLimit(String),
    #[error("cannot reserve sub-agent slug for '{0}': all suffixes are in use")]
    SlugExhausted(String),
    #[error("cannot build ephemeral sub-agent manifest for '{agent}': {reason}")]
    ManifestBuild { agent: String, reason: String },
}
