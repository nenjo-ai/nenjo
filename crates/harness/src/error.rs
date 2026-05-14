/// Harness API result type.
pub type Result<T> = std::result::Result<T, HarnessError>;

/// Errors returned by the public harness API.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness manifest services are not configured")]
    ManifestServicesNotConfigured,

    #[error("invalid harness command: {0}")]
    InvalidCommand(String),

    #[error("session runtime error: {source}")]
    SessionRuntime {
        #[source]
        source: anyhow::Error,
    },

    #[error("manifest runtime error: {source}")]
    ManifestRuntime {
        #[source]
        source: anyhow::Error,
    },

    #[error("response transport error: {source}")]
    ResponseTransport {
        #[source]
        source: anyhow::Error,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl HarnessError {
    pub fn session_runtime<Source>(source: Source) -> Self
    where
        Source: Into<anyhow::Error>,
    {
        Self::SessionRuntime {
            source: source.into(),
        }
    }

    pub fn manifest_runtime<Source>(source: Source) -> Self
    where
        Source: Into<anyhow::Error>,
    {
        Self::ManifestRuntime {
            source: source.into(),
        }
    }

    pub fn response_transport<Source>(source: Source) -> Self
    where
        Source: Into<anyhow::Error>,
    {
        Self::ResponseTransport {
            source: source.into(),
        }
    }
}
