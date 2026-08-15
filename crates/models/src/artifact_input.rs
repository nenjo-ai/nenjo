//! Provider transport contracts for ephemeral artifact inputs.

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

use nenjo_tool_api::{ArtifactRef, ArtifactSize};
use sha2::{Digest, Sha256};

/// Provider-native mechanism available for carrying one artifact into a model capability call.
///
/// This is deliberately distinct from configured model modalities: direct input is
/// allowed only when both the model metadata and its concrete provider adapter agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactInputTransport {
    Unsupported,
    /// Verified UTF-8 bytes embedded as a guarded text content part.
    InlineText {
        max_bytes: NonZeroU64,
    },
    Inline {
        max_bytes: NonZeroU64,
    },
    FileUpload {
        max_bytes: NonZeroU64,
    },
}

impl ArtifactInputTransport {
    /// Return whether this transport can carry an artifact of the given size.
    pub fn accepts(self, size: ArtifactSize) -> bool {
        match self {
            Self::Unsupported => false,
            Self::InlineText { max_bytes }
            | Self::Inline { max_bytes }
            | Self::FileUpload { max_bytes } => size.bytes() <= max_bytes.get(),
        }
    }
}

/// Digest-verified plaintext retained only while preparing one provider request.
///
/// This type deliberately has no serialization implementation. Durable messages
/// continue to contain only [`ArtifactRef`] values.
#[derive(Clone)]
pub struct PreparedArtifact {
    reference: ArtifactRef,
    bytes: Arc<[u8]>,
}

impl fmt::Debug for PreparedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedArtifact")
            .field("reference", &self.reference)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

impl PreparedArtifact {
    /// Bind plaintext to its immutable reference after checking size and digest.
    pub fn new(reference: ArtifactRef, bytes: Arc<[u8]>) -> Result<Self, PreparedArtifactError> {
        let actual_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual_size != reference.size().bytes() {
            return Err(PreparedArtifactError::SizeMismatch {
                artifact: Box::new(reference.clone()),
                actual_size,
            });
        }
        let actual_digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        if actual_digest != reference.digest().as_str() {
            return Err(PreparedArtifactError::DigestMismatch {
                artifact: Box::new(reference.clone()),
            });
        }
        Ok(Self { reference, bytes })
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Ephemeral artifact lookup for one provider request.
#[derive(Clone, Default)]
pub struct PreparedArtifactInputs {
    by_reference: HashMap<ArtifactRef, PreparedArtifact>,
}

impl fmt::Debug for PreparedArtifactInputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedArtifactInputs")
            .field("artifact_count", &self.by_reference.len())
            .finish()
    }
}

impl PreparedArtifactInputs {
    pub fn new(artifacts: impl IntoIterator<Item = PreparedArtifact>) -> Self {
        Self {
            by_reference: artifacts
                .into_iter()
                .map(|artifact| (artifact.reference.clone(), artifact))
                .collect(),
        }
    }

    pub fn get(&self, reference: &ArtifactRef) -> Option<&PreparedArtifact> {
        self.by_reference.get(reference)
    }

    pub fn is_empty(&self) -> bool {
        self.by_reference.is_empty()
    }
}

/// Plaintext does not match the immutable artifact metadata it claims.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreparedArtifactError {
    #[error(
        "prepared artifact {artifact:?} has {actual_size} bytes, which does not match its authoritative size"
    )]
    SizeMismatch {
        artifact: Box<ArtifactRef>,
        actual_size: u64,
    },
    #[error("prepared artifact {artifact:?} does not match its authoritative SHA-256 digest")]
    DigestMismatch { artifact: Box<ArtifactRef> },
}

#[cfg(test)]
mod tests {
    use nenjo_tool_api::{ArtifactId, MediaType, Sha256Digest};
    use uuid::Uuid;

    use super::*;

    fn reference(bytes: &[u8]) -> ArtifactRef {
        ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes))).unwrap(),
            MediaType::parse("image/png").unwrap(),
            ArtifactSize::new(bytes.len() as u64),
        )
    }

    #[test]
    fn prepared_artifact_checks_size_and_digest() {
        let bytes: Arc<[u8]> = Arc::from(&b"image"[..]);
        let reference = reference(&bytes);
        let prepared = PreparedArtifact::new(reference.clone(), bytes).unwrap();
        assert_eq!(prepared.reference(), &reference);

        let wrong_size = ArtifactRef::new(
            reference.id(),
            reference.digest().clone(),
            reference.media_type().clone(),
            ArtifactSize::new(99),
        );
        assert!(matches!(
            PreparedArtifact::new(wrong_size, Arc::from(&b"image"[..])),
            Err(PreparedArtifactError::SizeMismatch { .. })
        ));
        assert!(matches!(
            PreparedArtifact::new(reference, Arc::from(&b"other"[..])),
            Err(PreparedArtifactError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn prepared_lookup_requires_the_exact_immutable_reference() {
        let bytes: Arc<[u8]> = Arc::from(&b"image"[..]);
        let artifact_reference = reference(&bytes);
        let prepared = PreparedArtifact::new(artifact_reference.clone(), bytes).unwrap();
        let inputs = PreparedArtifactInputs::new([prepared]);

        assert!(inputs.get(&artifact_reference).is_some());
        let other = reference(b"image");
        assert!(inputs.get(&other).is_none());
    }

    #[test]
    fn debug_output_never_contains_prepared_plaintext() {
        let bytes: Arc<[u8]> = Arc::from(&b"secret-image-bytes"[..]);
        let artifact_reference = reference(&bytes);
        let prepared = PreparedArtifact::new(artifact_reference, bytes).unwrap();
        let inputs = PreparedArtifactInputs::new([prepared.clone()]);

        assert!(!format!("{prepared:?}").contains("secret-image-bytes"));
        assert!(!format!("{inputs:?}").contains("secret-image-bytes"));
    }
}
