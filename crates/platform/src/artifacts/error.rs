use uuid::Uuid;

/// Structured failures while resolving or materializing immutable artifacts.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactMaterializationError {
    #[error("artifact {artifact_id} is not ready")]
    NotReady { artifact_id: Uuid },
    #[error("artifact {artifact_id} metadata is invalid: {reason}")]
    InvalidMetadata {
        artifact_id: Uuid,
        reason: &'static str,
    },
    #[error("artifact {artifact_id} exceeds the {limit_kind} limit of {limit_bytes} bytes")]
    SizeLimit {
        artifact_id: Uuid,
        limit_kind: &'static str,
        limit_bytes: u64,
    },
    #[error("artifact {artifact_id} metadata changed while resolving immutable content")]
    MetadataChanged { artifact_id: Uuid },
    #[error("artifact {artifact_id} encrypted envelope identity does not match the request")]
    EnvelopeIdentity { artifact_id: Uuid },
    #[error("artifact {artifact_id} {content_kind} digest does not match platform metadata")]
    DigestMismatch {
        artifact_id: Uuid,
        content_kind: &'static str,
    },
    #[error("artifact {artifact_id} {content_kind} size does not match platform metadata")]
    SizeMismatch {
        artifact_id: Uuid,
        content_kind: &'static str,
    },
    #[error("artifact {artifact_id} encrypted payload is malformed")]
    InvalidEnvelope {
        artifact_id: Uuid,
        #[source]
        source: anyhow::Error,
    },
    #[error("artifact {artifact_id} could not be decoded")]
    Decode {
        artifact_id: Uuid,
        #[source]
        source: anyhow::Error,
    },
    #[error("artifact {artifact_id} transport failed")]
    Transport {
        artifact_id: Uuid,
        #[source]
        source: anyhow::Error,
    },
    #[error("artifact plaintext cache operation failed")]
    Cache(#[source] anyhow::Error),
    #[error("artifact materialization coordinator is closed")]
    CoordinatorClosed,
    #[error(transparent)]
    InvalidReference(#[from] nenjo_content::ContentValueError),
}
