use std::fmt;
use std::sync::{Arc, Weak};

use anyhow::anyhow;
use async_trait::async_trait;
use base64::Engine;
use dashmap::DashMap;
use nenjo_content::{ArtifactId, ArtifactRef, Sha256Digest};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

use super::cache::ArtifactCache;
use super::repository::MAX_ARTIFACT_BYTES;
use super::{
    ArtifactMaterializationError, ArtifactMetadata, ArtifactRepository, PlatformArtifactRepository,
};
use crate::SensitivePayloadEncoder;

const ARTIFACT_CONTENT_OBJECT_TYPE: &str = "artifact.content";
const DEFAULT_MAX_CONCURRENT_FILLS: usize = 4;
const SINGLE_FLIGHT_PRUNE_THRESHOLD: usize = 4_096;
const AUTHORIZED_METADATA_CACHE_LIMIT: usize = 4_096;

/// Immutable plaintext materialized inside the trusted harness boundary.
#[derive(Clone)]
pub struct MaterializedArtifact {
    reference: ArtifactRef,
    bytes: Arc<[u8]>,
}

impl fmt::Debug for MaterializedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaterializedArtifact")
            .field("reference", &self.reference)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

impl MaterializedArtifact {
    /// Construct materialized plaintext only after checking its immutable size and digest.
    pub fn new_verified(
        reference: ArtifactRef,
        bytes: Arc<[u8]>,
    ) -> Result<Self, ArtifactMaterializationError> {
        verify_plaintext(&reference, &bytes)?;
        Ok(Self { reference, bytes })
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }
}

/// Runtime seam for resolving immutable artifact references into verified plaintext.
#[async_trait]
pub trait ArtifactMaterializer: Send + Sync {
    async fn materialize(
        &self,
        org_id: Uuid,
        artifact: &ArtifactRef,
    ) -> Result<MaterializedArtifact, ArtifactMaterializationError>;
}

/// Platform-backed materializer with a persistent plaintext cache.
pub struct PlatformArtifactMaterializer<E, R = PlatformArtifactRepository> {
    repository: R,
    decoder: E,
    cache: ArtifactCache,
    authorized: DashMap<ArtifactAccessKey, ArtifactMetadata>,
    single_flight: DashMap<ArtifactAccessKey, Weak<Mutex<()>>>,
    fill_permits: Semaphore,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ArtifactAccessKey {
    org_id: Uuid,
    artifact_id: ArtifactId,
}

impl ArtifactAccessKey {
    fn new(org_id: Uuid, artifact_id: ArtifactId) -> Self {
        Self {
            org_id,
            artifact_id,
        }
    }
}

impl<E, R> PlatformArtifactMaterializer<E, R>
where
    R: ArtifactRepository,
{
    pub fn new(repository: R, decoder: E, cache_root: impl Into<std::path::PathBuf>) -> Self {
        Self::with_max_concurrent_fills(
            repository,
            decoder,
            cache_root,
            DEFAULT_MAX_CONCURRENT_FILLS,
        )
    }

    pub fn with_max_concurrent_fills(
        repository: R,
        decoder: E,
        cache_root: impl Into<std::path::PathBuf>,
        max_concurrent_fills: usize,
    ) -> Self {
        Self {
            repository,
            decoder,
            cache: ArtifactCache::new(cache_root),
            authorized: DashMap::new(),
            single_flight: DashMap::new(),
            fill_permits: Semaphore::new(max_concurrent_fills.max(1)),
        }
    }

    pub async fn seed(
        &self,
        org_id: Uuid,
        metadata: &ArtifactMetadata,
        bytes: &[u8],
    ) -> Result<(), ArtifactMaterializationError> {
        verify_plaintext(metadata.reference(), bytes)?;
        self.cache
            .write(org_id, metadata.reference(), bytes)
            .await
            .map_err(ArtifactMaterializationError::Cache)?;
        self.remember_metadata(org_id, metadata);
        Ok(())
    }

    fn remember_metadata(&self, org_id: Uuid, metadata: &ArtifactMetadata) {
        if self.authorized.len() >= AUTHORIZED_METADATA_CACHE_LIMIT {
            self.authorized.clear();
        }
        self.authorized.insert(
            ArtifactAccessKey::new(org_id, metadata.reference().id()),
            metadata.clone(),
        );
    }

    async fn resolve_reference_metadata(
        &self,
        org_id: Uuid,
        artifact: &ArtifactRef,
    ) -> Result<ArtifactMetadata, ArtifactMaterializationError> {
        let metadata = self.resolve_metadata(org_id, artifact.id()).await?;
        if metadata.reference() != artifact {
            return Err(ArtifactMaterializationError::MetadataChanged {
                artifact_id: artifact.id().as_uuid(),
            });
        }
        Ok(metadata)
    }

    /// Resolve and authenticate one immutable artifact's metadata.
    pub async fn resolve_metadata(
        &self,
        org_id: Uuid,
        artifact_id: ArtifactId,
    ) -> Result<ArtifactMetadata, ArtifactMaterializationError> {
        let key = ArtifactAccessKey::new(org_id, artifact_id);
        if let Some(metadata) = self.authorized.get(&key) {
            return Ok(metadata.clone());
        }

        let authorization_lock = self.fill_lock(&key);
        let _authorization_guard = authorization_lock.lock().await;
        if let Some(metadata) = self.authorized.get(&key) {
            return Ok(metadata.clone());
        }
        let metadata = self.repository.metadata(artifact_id).await?;
        if metadata.org_id() != org_id {
            return Err(ArtifactMaterializationError::MetadataChanged {
                artifact_id: artifact_id.as_uuid(),
            });
        }
        self.remember_metadata(org_id, &metadata);
        Ok(metadata)
    }

    fn fill_lock(&self, key: &ArtifactAccessKey) -> Arc<Mutex<()>> {
        if self.single_flight.len() > SINGLE_FLIGHT_PRUNE_THRESHOLD {
            self.single_flight.retain(|_, lock| lock.strong_count() > 0);
        }
        let mut entry = self.single_flight.entry(key.clone()).or_default();
        if let Some(lock) = entry.value().upgrade() {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        *entry.value_mut() = Arc::downgrade(&lock);
        lock
    }

    async fn fill(
        &self,
        org_id: Uuid,
        metadata: &ArtifactMetadata,
    ) -> Result<MaterializedArtifact, ArtifactMaterializationError>
    where
        E: SensitivePayloadEncoder,
    {
        let expected = metadata.reference();
        let downloaded = self.repository.download(metadata).await?;
        if !metadata.matches_record(&downloaded.artifact) {
            return Err(ArtifactMaterializationError::MetadataChanged {
                artifact_id: expected.id().as_uuid(),
            });
        }
        verify_ciphertext(metadata, &downloaded.ciphertext)?;
        let encrypted_value: Value =
            serde_json::from_slice(&downloaded.ciphertext).map_err(|e| {
                ArtifactMaterializationError::InvalidEnvelope {
                    artifact_id: expected.id().as_uuid(),
                    source: e.into(),
                }
            })?;
        let encrypted_payload: nenjo_events::EncryptedPayload =
            serde_json::from_value(encrypted_value.clone()).map_err(|e| {
                ArtifactMaterializationError::InvalidEnvelope {
                    artifact_id: expected.id().as_uuid(),
                    source: e.into(),
                }
            })?;
        if encrypted_payload.account_id != org_id
            || encrypted_payload.object_id != expected.id().as_uuid()
            || encrypted_payload.object_type != ARTIFACT_CONTENT_OBJECT_TYPE
            || encrypted_payload.encryption_scope.as_deref() != Some("org")
        {
            return Err(ArtifactMaterializationError::EnvelopeIdentity {
                artifact_id: expected.id().as_uuid(),
            });
        }
        let decoded = self
            .decoder
            .decode_payload(&encrypted_value)
            .await
            .map_err(|source| ArtifactMaterializationError::Decode {
                artifact_id: expected.id().as_uuid(),
                source,
            })?
            .ok_or_else(|| ArtifactMaterializationError::Decode {
                artifact_id: expected.id().as_uuid(),
                source: anyhow!("artifact payload decoder returned no plaintext"),
            })?;
        let plaintext: ArtifactPlaintextEnvelope =
            serde_json::from_value(decoded).map_err(|e| {
                ArtifactMaterializationError::InvalidEnvelope {
                    artifact_id: expected.id().as_uuid(),
                    source: e.into(),
                }
            })?;
        let expected = expected.clone();
        let decode_reference = expected.clone();
        let artifact_id = expected.id().as_uuid();
        let bytes =
            tokio::task::spawn_blocking(move || decode_plaintext(&decode_reference, plaintext))
                .await
                .map_err(|source| ArtifactMaterializationError::Decode {
                    artifact_id,
                    source: anyhow!("artifact plaintext decode task failed: {source}"),
                })??;
        self.cache
            .write(org_id, &expected, &bytes)
            .await
            .map_err(ArtifactMaterializationError::Cache)?;
        Ok(MaterializedArtifact {
            reference: expected,
            bytes: Arc::from(bytes),
        })
    }

    /// Materialize metadata already resolved through an authenticated platform call.
    pub async fn materialize_metadata(
        &self,
        org_id: Uuid,
        metadata: ArtifactMetadata,
    ) -> Result<MaterializedArtifact, ArtifactMaterializationError>
    where
        E: SensitivePayloadEncoder,
    {
        let key = ArtifactAccessKey::new(org_id, metadata.reference().id());
        self.remember_metadata(org_id, &metadata);
        if let Some(bytes) = self
            .cache
            .read(org_id, metadata.reference())
            .await
            .map_err(ArtifactMaterializationError::Cache)?
        {
            return Ok(MaterializedArtifact {
                reference: metadata.reference().clone(),
                bytes: Arc::from(bytes),
            });
        }

        let fill_lock = self.fill_lock(&key);
        let _fill_guard = fill_lock.lock().await;
        if let Some(bytes) = self
            .cache
            .read(org_id, metadata.reference())
            .await
            .map_err(ArtifactMaterializationError::Cache)?
        {
            return Ok(MaterializedArtifact {
                reference: metadata.reference().clone(),
                bytes: Arc::from(bytes),
            });
        }

        let _permit = self
            .fill_permits
            .acquire()
            .await
            .map_err(|_| ArtifactMaterializationError::CoordinatorClosed)?;
        self.fill(org_id, &metadata).await
    }
}

#[async_trait]
impl<E, R> ArtifactMaterializer for PlatformArtifactMaterializer<E, R>
where
    E: SensitivePayloadEncoder + Send + Sync,
    R: ArtifactRepository,
{
    async fn materialize(
        &self,
        org_id: Uuid,
        artifact: &ArtifactRef,
    ) -> Result<MaterializedArtifact, ArtifactMaterializationError> {
        let metadata = self.resolve_reference_metadata(org_id, artifact).await?;
        self.materialize_metadata(org_id, metadata).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactPlaintextEnvelope {
    content_base64: String,
}

fn decode_plaintext(
    expected: &ArtifactRef,
    plaintext: ArtifactPlaintextEnvelope,
) -> Result<Vec<u8>, ArtifactMaterializationError> {
    let max_base64_len = usize::try_from(MAX_ARTIFACT_BYTES)
        .unwrap_or(usize::MAX)
        .div_ceil(3)
        .saturating_mul(4);
    if plaintext.content_base64.len() > max_base64_len {
        return Err(ArtifactMaterializationError::SizeLimit {
            artifact_id: expected.id().as_uuid(),
            limit_kind: "plaintext",
            limit_bytes: MAX_ARTIFACT_BYTES,
        });
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(plaintext.content_base64)
        .map_err(|source| ArtifactMaterializationError::Decode {
            artifact_id: expected.id().as_uuid(),
            source: source.into(),
        })?;
    verify_plaintext(expected, &bytes)?;
    Ok(bytes)
}

fn verify_plaintext(
    expected: &ArtifactRef,
    bytes: &[u8],
) -> Result<(), ArtifactMaterializationError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != expected.size().bytes() {
        return Err(ArtifactMaterializationError::SizeMismatch {
            artifact_id: expected.id().as_uuid(),
            content_kind: "plaintext",
        });
    }
    verify_digest(
        expected.id().as_uuid(),
        expected.digest(),
        bytes,
        "plaintext",
    )
}

fn verify_ciphertext(
    metadata: &ArtifactMetadata,
    bytes: &[u8],
) -> Result<(), ArtifactMaterializationError> {
    let artifact_id = metadata.reference().id().as_uuid();
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.ciphertext_size() {
        return Err(ArtifactMaterializationError::SizeMismatch {
            artifact_id,
            content_kind: "ciphertext",
        });
    }
    verify_digest(
        artifact_id,
        metadata.ciphertext_digest(),
        bytes,
        "ciphertext",
    )
}

fn verify_digest(
    artifact_id: Uuid,
    expected: &Sha256Digest,
    bytes: &[u8],
    content_kind: &'static str,
) -> Result<(), ArtifactMaterializationError> {
    let actual = format!("sha256:{:x}", Sha256::digest(bytes));
    if actual != expected.as_str() {
        return Err(ArtifactMaterializationError::DigestMismatch {
            artifact_id,
            content_kind,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::Result;
    use nenjo_content::{ArtifactId, ArtifactSize, MediaType};
    use serde_json::json;

    use super::*;
    use crate::artifact_tools::{ArtifactRecord, DownloadedArtifact};

    #[derive(Clone)]
    struct TestDecoder {
        decoded: Value,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SensitivePayloadEncoder for TestDecoder {
        async fn encode_payload(
            &self,
            _account_id: Uuid,
            _object_id: Uuid,
            _object_type: &str,
            _payload: &Value,
        ) -> Result<Option<Value>> {
            unreachable!("materialization never encodes payloads")
        }

        async fn decode_payload(&self, _payload: &Value) -> Result<Option<Value>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(self.decoded.clone()))
        }
    }

    struct TestRepository {
        metadata: ArtifactMetadata,
        record: ArtifactRecord,
        ciphertext: Vec<u8>,
        metadata_calls: AtomicUsize,
        download_calls: AtomicUsize,
        failing_downloads: AtomicUsize,
    }

    type TestMaterializer = PlatformArtifactMaterializer<TestDecoder, Arc<TestRepository>>;

    #[async_trait]
    impl ArtifactRepository for Arc<TestRepository> {
        async fn metadata(
            &self,
            artifact_id: ArtifactId,
        ) -> Result<ArtifactMetadata, ArtifactMaterializationError> {
            assert_eq!(artifact_id, self.metadata.reference().id());
            self.metadata_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.metadata.clone())
        }

        async fn download(
            &self,
            metadata: &ArtifactMetadata,
        ) -> Result<DownloadedArtifact, ArtifactMaterializationError> {
            assert_eq!(metadata, &self.metadata);
            self.download_calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            if self
                .failing_downloads
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ArtifactMaterializationError::Transport {
                    artifact_id: metadata.reference().id().as_uuid(),
                    source: anyhow!("simulated artifact download failure"),
                });
            }
            Ok(DownloadedArtifact {
                artifact: self.record.clone(),
                ciphertext: self.ciphertext.clone(),
            })
        }
    }

    fn materializer_fixture() -> (
        TestMaterializer,
        ArtifactRef,
        Uuid,
        Arc<TestRepository>,
        Arc<AtomicUsize>,
        tempfile::TempDir,
    ) {
        let org_id = Uuid::new_v4();
        let artifact_id = Uuid::new_v4();
        let lineage_id = Uuid::new_v4();
        let plaintext = b"one immutable artifact";
        let reference = ArtifactRef::new(
            ArtifactId::parse(artifact_id).unwrap(),
            Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(plaintext))).unwrap(),
            MediaType::parse("text/plain").unwrap(),
            ArtifactSize::new(u64::try_from(plaintext.len()).unwrap()),
        );
        let envelope = nenjo_events::EncryptedPayload {
            account_id: org_id,
            encryption_scope: Some("org".to_string()),
            object_id: artifact_id,
            object_type: ARTIFACT_CONTENT_OBJECT_TYPE.to_string(),
            algorithm: "test".to_string(),
            key_version: 1,
            nonce: "test".to_string(),
            ciphertext: "test".to_string(),
        };
        let ciphertext = serde_json::to_vec(&envelope).unwrap();
        let record = ArtifactRecord {
            id: artifact_id,
            org_id,
            state: "ready".to_string(),
            lineage_id,
            revision_number: 1,
            previous_artifact_id: None,
            name: "artifact.txt".to_string(),
            media_type: "text/plain".to_string(),
            plaintext_size_bytes: i64::try_from(plaintext.len()).unwrap(),
            ciphertext_size_bytes: i64::try_from(ciphertext.len()).unwrap(),
            plaintext_digest: reference.digest().to_string(),
            ciphertext_digest: format!("sha256:{:x}", Sha256::digest(&ciphertext)),
            created_at: "2026-08-13T00:00:00Z".to_string(),
            ready_at: Some("2026-08-13T00:00:00Z".to_string()),
        };
        let metadata = ArtifactMetadata::parse(&record).unwrap();
        let repository = Arc::new(TestRepository {
            metadata,
            record,
            ciphertext,
            metadata_calls: AtomicUsize::new(0),
            download_calls: AtomicUsize::new(0),
            failing_downloads: AtomicUsize::new(0),
        });
        let decode_calls = Arc::new(AtomicUsize::new(0));
        let decoder = TestDecoder {
            decoded: json!({
                "content_base64": base64::engine::general_purpose::STANDARD.encode(plaintext)
            }),
            calls: Arc::clone(&decode_calls),
        };
        let cache = tempfile::tempdir().unwrap();
        let materializer = PlatformArtifactMaterializer {
            repository: repository.clone(),
            decoder,
            cache: ArtifactCache::new(cache.path()),
            authorized: DashMap::new(),
            single_flight: DashMap::new(),
            fill_permits: Semaphore::new(4),
        };
        (
            materializer,
            reference,
            org_id,
            repository,
            decode_calls,
            cache,
        )
    }

    #[test]
    fn verified_materialized_artifact_redacts_plaintext_from_debug() {
        let bytes: Arc<[u8]> = Arc::from(&b"trusted-boundary-secret"[..]);
        let reference = ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap(),
            MediaType::parse("image/png").unwrap(),
            ArtifactSize::new(bytes.len() as u64),
        );

        let materialized = MaterializedArtifact::new_verified(reference, bytes).unwrap();

        assert!(!format!("{materialized:?}").contains("trusted-boundary-secret"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_reads_share_one_authenticated_fill_and_cache_hits_are_local() {
        let (materializer, reference, org_id, repository, decode_calls, _cache) =
            materializer_fixture();
        let materializer = Arc::new(materializer);
        let reads = (0..12).map(|_| {
            let materializer = Arc::clone(&materializer);
            let reference = reference.clone();
            tokio::spawn(async move { materializer.materialize(org_id, &reference).await.unwrap() })
        });
        for read in reads {
            assert_eq!(read.await.unwrap().bytes(), b"one immutable artifact");
        }

        assert_eq!(repository.metadata_calls.load(Ordering::SeqCst), 1);
        assert_eq!(repository.download_calls.load(Ordering::SeqCst), 1);
        assert_eq!(decode_calls.load(Ordering::SeqCst), 1);

        materializer.materialize(org_id, &reference).await.unwrap();
        assert_eq!(repository.metadata_calls.load(Ordering::SeqCst), 1);
        assert_eq!(repository.download_calls.load(Ordering::SeqCst), 1);
        assert_eq!(decode_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_fills_release_single_flight_and_can_be_retried() {
        let (materializer, reference, org_id, repository, decode_calls, _cache) =
            materializer_fixture();
        repository.failing_downloads.store(1, Ordering::SeqCst);

        assert!(materializer.materialize(org_id, &reference).await.is_err());
        let recovered = materializer
            .materialize(org_id, &reference)
            .await
            .expect("retry materialization");

        assert_eq!(recovered.bytes(), b"one immutable artifact");
        assert_eq!(repository.metadata_calls.load(Ordering::SeqCst), 1);
        assert_eq!(repository.download_calls.load(Ordering::SeqCst), 2);
        assert_eq!(decode_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn expired_single_flight_entries_are_pruned_at_the_bound() {
        let (materializer, _reference, org_id, _repository, _decode_calls, _cache) =
            materializer_fixture();
        for _ in 0..=SINGLE_FLIGHT_PRUNE_THRESHOLD + 1 {
            let key = ArtifactAccessKey::new(
                org_id,
                ArtifactId::parse(Uuid::new_v4()).expect("artifact id"),
            );
            drop(materializer.fill_lock(&key));
        }

        assert!(materializer.single_flight.len() <= SINGLE_FLIGHT_PRUNE_THRESHOLD);
    }
}
