use std::sync::Arc;

use async_trait::async_trait;
use nenjo_content::{ArtifactId, ArtifactRef, ArtifactSize, MediaType, Sha256Digest};
use uuid::Uuid;

use super::ArtifactMaterializationError;
use crate::PlatformManifestClient;
use crate::artifact_tools::{ArtifactRecord, DownloadedArtifact};

pub(crate) const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_ARTIFACT_ENVELOPE_BYTES: u64 = 32 * 1024 * 1024;

/// Validated authoritative metadata for one ready immutable artifact revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMetadata {
    org_id: Uuid,
    reference: ArtifactRef,
    lineage_id: Uuid,
    revision_number: i32,
    ciphertext_size: u64,
    ciphertext_digest: Sha256Digest,
}

impl ArtifactMetadata {
    pub const fn org_id(&self) -> Uuid {
        self.org_id
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub const fn lineage_id(&self) -> Uuid {
        self.lineage_id
    }

    pub const fn revision_number(&self) -> i32 {
        self.revision_number
    }

    pub(crate) const fn ciphertext_size(&self) -> u64 {
        self.ciphertext_size
    }

    pub(crate) fn ciphertext_digest(&self) -> &Sha256Digest {
        &self.ciphertext_digest
    }

    pub(crate) fn parse(record: &ArtifactRecord) -> Result<Self, ArtifactMaterializationError> {
        if record.state != "ready" {
            return Err(ArtifactMaterializationError::NotReady {
                artifact_id: record.id,
            });
        }
        let plaintext_size = u64::try_from(record.plaintext_size_bytes).map_err(|_| {
            ArtifactMaterializationError::InvalidMetadata {
                artifact_id: record.id,
                reason: "plaintext size is negative",
            }
        })?;
        if plaintext_size > MAX_ARTIFACT_BYTES {
            return Err(ArtifactMaterializationError::SizeLimit {
                artifact_id: record.id,
                limit_kind: "plaintext",
                limit_bytes: MAX_ARTIFACT_BYTES,
            });
        }
        let ciphertext_size = u64::try_from(record.ciphertext_size_bytes).map_err(|_| {
            ArtifactMaterializationError::InvalidMetadata {
                artifact_id: record.id,
                reason: "ciphertext size is negative",
            }
        })?;
        if ciphertext_size > MAX_ARTIFACT_ENVELOPE_BYTES {
            return Err(ArtifactMaterializationError::SizeLimit {
                artifact_id: record.id,
                limit_kind: "encrypted envelope",
                limit_bytes: MAX_ARTIFACT_ENVELOPE_BYTES,
            });
        }
        Ok(Self {
            org_id: record.org_id,
            reference: ArtifactRef::new(
                ArtifactId::parse(record.id)?,
                Sha256Digest::parse(&record.plaintext_digest)?,
                MediaType::parse(&record.media_type)?,
                ArtifactSize::new(plaintext_size),
            ),
            lineage_id: record.lineage_id,
            revision_number: record.revision_number,
            ciphertext_size,
            ciphertext_digest: Sha256Digest::parse(&record.ciphertext_digest)?,
        })
    }

    pub(crate) fn matches_record(&self, record: &ArtifactRecord) -> bool {
        Self::parse(record).is_ok_and(|actual| actual == *self)
    }
}

/// Authenticated platform access for artifact metadata and encrypted envelopes.
#[derive(Clone)]
pub struct PlatformArtifactRepository {
    client: Arc<PlatformManifestClient>,
}

impl PlatformArtifactRepository {
    pub fn new(client: Arc<PlatformManifestClient>) -> Self {
        Self { client }
    }

    pub async fn metadata(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<ArtifactMetadata, ArtifactMaterializationError> {
        let id = artifact_id.as_uuid();
        let detail = self.client.get_artifact(id).await.map_err(|source| {
            ArtifactMaterializationError::Transport {
                artifact_id: id,
                source,
            }
        })?;
        ArtifactMetadata::parse(&detail.artifact)
    }

    pub(crate) async fn download(
        &self,
        metadata: &ArtifactMetadata,
    ) -> Result<DownloadedArtifact, ArtifactMaterializationError> {
        let id = metadata.reference.id().as_uuid();
        self.client
            .download_artifact(id, MAX_ARTIFACT_ENVELOPE_BYTES)
            .await
            .map_err(|source| ArtifactMaterializationError::Transport {
                artifact_id: id,
                source,
            })
    }
}

/// Authenticated source for immutable artifact metadata and encrypted envelopes.
///
/// Materializers use this as a generic boundary so production and test repositories retain
/// their concrete types without runtime type erasure.
#[async_trait]
pub trait ArtifactRepository: Send + Sync {
    async fn metadata(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<ArtifactMetadata, ArtifactMaterializationError>;

    async fn download(
        &self,
        metadata: &ArtifactMetadata,
    ) -> Result<DownloadedArtifact, ArtifactMaterializationError>;
}

#[async_trait]
impl ArtifactRepository for PlatformArtifactRepository {
    async fn metadata(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<ArtifactMetadata, ArtifactMaterializationError> {
        Self::metadata(self, artifact_id).await
    }

    async fn download(
        &self,
        metadata: &ArtifactMetadata,
    ) -> Result<DownloadedArtifact, ArtifactMaterializationError> {
        Self::download(self, metadata).await
    }
}
