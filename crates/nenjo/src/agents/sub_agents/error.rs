use serde_json::{Value, json};
use thiserror::Error;

use crate::agents::async_ops::AsyncOpStartError;

#[derive(Debug, Error)]
pub(crate) enum SubAgentError {
    #[error("invalid result field name: {0}")]
    InvalidResultFieldName(String),
    #[error("cannot spawn '{0}': maximum sub-agent depth reached")]
    DepthLimit(String),
    #[error("cannot spawn {requested} sub-agents in one call; the limit is {limit}")]
    BatchLimit { requested: usize, limit: usize },
    #[error(transparent)]
    Capacity(#[from] AsyncOpStartError),
    #[error("cannot reserve sub-agent slug for '{0}': all suffixes are in use")]
    SlugExhausted(String),
    #[error("cannot build ephemeral sub-agent manifest for '{agent}': {reason}")]
    ManifestBuild { agent: String, reason: String },
}

impl SubAgentError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::InvalidResultFieldName(_) => "invalid_result_format",
            Self::DepthLimit(_) => "sub_agent_depth_limit",
            Self::BatchLimit { .. } => "sub_agent_batch_limit",
            Self::Capacity(error) => error.code(),
            Self::SlugExhausted(_) => "sub_agent_slug_exhausted",
            Self::ManifestBuild { .. } => "sub_agent_manifest_invalid",
        }
    }

    pub(crate) fn retryable(&self) -> bool {
        match self {
            Self::Capacity(error) => error.retryable(),
            Self::InvalidResultFieldName(_)
            | Self::DepthLimit(_)
            | Self::BatchLimit { .. }
            | Self::SlugExhausted(_)
            | Self::ManifestBuild { .. } => false,
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        let mut value = json!({
            "code": self.code(),
            "message": self.to_string(),
            "retryable": self.retryable(),
        });
        if let Self::Capacity(error) = self {
            value["limit"] = json!(error.limit());
        }
        value
    }
}
