use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use dashmap::DashMap;
use tokio::sync::RwLock;
use uuid::Uuid;
use x25519_dalek::StaticSecret;

mod storage;
mod types;
mod wrap;

pub use types::{
    ContentKey, ContentScope, EnrollmentStatus, StoredWorkerEnrollment, WorkerCertificate,
    WorkerEnrollmentRequest, WorkerIdentityPublic, WrappedAccountContentKey, WrappedOrgContentKey,
};
#[cfg(test)]
pub(crate) use wrap::wrap_ack_for_recipient;

use self::storage::{
    generate_verification_code, load_enrollment, load_or_create_identity, persist_enrollment,
};
use self::types::StoredWorkerIdentity;
use self::wrap::{unwrap_ack, unwrap_ock};

/// Local worker trust-state manager.
///
/// Owns persisted worker identity, enrollment snapshots, wrapped key caches,
/// and helper methods for refreshing enrollment from the backend.
#[derive(Clone)]
pub struct WorkerAuthProvider {
    root: PathBuf,
    identity: Arc<StoredWorkerIdentity>,
    enrollment: Arc<RwLock<StoredWorkerEnrollment>>,
    user_ack_cache: Arc<DashMap<Uuid, ContentKey>>,
    ock_cache: Arc<RwLock<Option<ContentKey>>>,
}

impl WorkerAuthProvider {
    /// Load existing crypto state from disk or create a fresh worker identity
    /// plus empty enrollment state at the given root directory.
    pub fn load_or_create(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("Failed to create crypto state dir: {}", root.display()))?;

        let identity = Arc::new(load_or_create_identity(&root)?);
        let enrollment = Arc::new(RwLock::new(load_enrollment(&root)?));

        Ok(Self {
            root,
            identity,
            enrollment,
            user_ack_cache: Arc::new(DashMap::new()),
            ock_cache: Arc::new(RwLock::new(None)),
        })
    }

    /// Return the public worker identity used for enrollment and diagnostics.
    pub fn identity(&self) -> WorkerIdentityPublic {
        self.identity.public()
    }

    /// Build a local enrollment request snapshot for the current worker identity.
    pub fn enrollment_request(&self, api_key_id: Uuid) -> WorkerEnrollmentRequest {
        WorkerEnrollmentRequest {
            api_key_id,
            worker: self.identity(),
            requested_at: chrono::Utc::now(),
            verification_code: self.pending_verification_code(),
        }
    }

    /// Build the API enrollment request payload using the current worker identity.
    pub fn api_enrollment_request(
        &self,
        api_key_id: Uuid,
        metadata: Option<serde_json::Value>,
    ) -> nenjo::client::WorkerEnrollmentRequest {
        let worker = self.identity();
        nenjo::client::WorkerEnrollmentRequest {
            api_key_id,
            requested_at: chrono::Utc::now(),
            crypto_version: worker.crypto_version,
            enc_public_key: worker.enc_public_key,
            sign_public_key: worker.sign_public_key,
            verification_code: self.pending_verification_code(),
            metadata,
        }
    }

    /// Async variant of [`Self::api_enrollment_request`] that persists a stable
    /// pending verification code before returning.
    pub async fn api_enrollment_request_async(
        &self,
        api_key_id: Uuid,
        metadata: Option<serde_json::Value>,
    ) -> Result<nenjo::client::WorkerEnrollmentRequest> {
        let worker = self.identity();
        Ok(nenjo::client::WorkerEnrollmentRequest {
            api_key_id,
            requested_at: chrono::Utc::now(),
            crypto_version: worker.crypto_version,
            enc_public_key: worker.enc_public_key,
            sign_public_key: worker.sign_public_key,
            verification_code: self.pending_verification_code_async().await?,
            metadata,
        })
    }

    /// Return the current pending verification code, creating and persisting one
    /// if necessary.
    pub async fn pending_verification_code_async(&self) -> Result<String> {
        let mut enrollment = self.enrollment.write().await;
        if let Some(code) = enrollment.pending_verification_code.clone() {
            return Ok(code);
        }
        let code = generate_verification_code();
        enrollment.pending_verification_code = Some(code.clone());
        persist_enrollment(&self.root, &enrollment)?;
        Ok(code)
    }

    fn pending_verification_code(&self) -> String {
        if let Ok(enrollment) = self.enrollment.try_read()
            && let Some(code) = enrollment.pending_verification_code.clone()
        {
            return code;
        }
        generate_verification_code()
    }

    /// Return the current persisted enrollment snapshot.
    pub async fn enrollment(&self) -> StoredWorkerEnrollment {
        self.enrollment.read().await.clone()
    }

    /// Return the key version of the currently enrolled wrapped OCK, if present.
    pub async fn current_ock_key_version(&self) -> Option<u32> {
        self.enrollment
            .read()
            .await
            .wrapped_ock
            .as_ref()
            .map(|wrapped| wrapped.key_version)
    }

    /// Return whether the worker currently has a complete active enrollment snapshot.
    ///
    /// Active requires both a backend-signed certificate and at least one
    /// user-routed wrapped ACK. Fixtures that seed wrapped keys without the
    /// certificate should remain pending so the worker refreshes from the backend
    /// instead of treating the local snapshot as complete.
    pub async fn enrollment_status(&self) -> EnrollmentStatus {
        let enrollment = self.enrollment.read().await;
        if enrollment.certificate.is_some() && !enrollment.user_wrapped_acks.is_empty() {
            EnrollmentStatus::Active
        } else {
            EnrollmentStatus::Pending
        }
    }

    /// Persist new certificate and wrapped key material into local enrollment state.
    pub async fn store_enrollment(
        &self,
        certificate: Option<WorkerCertificate>,
        bootstrap_user_id: Option<Uuid>,
        wrapped_ack: Option<WrappedAccountContentKey>,
        wrapped_ock: Option<WrappedOrgContentKey>,
    ) -> Result<()> {
        let mut enrollment = self.enrollment.write().await;
        if let Some(certificate) = certificate {
            enrollment.certificate = Some(certificate);
        }
        if let Some(wrapped_ack) = wrapped_ack {
            let user_id = bootstrap_user_id
                .context("bootstrap user id is required when persisting an enrolled ACK")?;
            enrollment.user_wrapped_acks.insert(user_id, wrapped_ack);
            enrollment.enrolled_at = Some(chrono::Utc::now());
            enrollment.pending_verification_code = None;
            self.user_ack_cache.remove(&user_id);
        }
        if let Some(wrapped_ock) = wrapped_ock {
            enrollment.wrapped_ock = Some(wrapped_ock);
            *self.ock_cache.write().await = None;
        }
        persist_enrollment(&self.root, &enrollment)
    }

    /// Apply a backend enrollment status response to local persisted state.
    pub async fn apply_backend_enrollment(
        &self,
        status: &nenjo::client::WorkerEnrollmentStatusResponse,
    ) -> Result<()> {
        if matches!(
            status.state,
            nenjo::client::WorkerEnrollmentState::Pending
                | nenjo::client::WorkerEnrollmentState::Revoked
        ) {
            let mut enrollment = self.enrollment.write().await;
            enrollment.certificate = None;
            enrollment.wrapped_ock = None;
            enrollment.user_wrapped_acks.clear();
            enrollment.enrolled_at = None;
            self.user_ack_cache.clear();
            *self.ock_cache.write().await = None;
            return persist_enrollment(&self.root, &enrollment);
        }

        let certificate = status
            .certificate
            .clone()
            .map(|certificate| WorkerCertificate {
                account_id: certificate.account_id,
                api_key_id: certificate.api_key_id,
                issued_at: certificate.issued_at,
                enc_public_key: certificate.enc_public_key,
                sign_public_key: certificate.sign_public_key,
                signature: certificate.signature,
            });
        let user_wrapped_acks = status
            .user_wrapped_acks
            .iter()
            .map(|(user_id, wrapped)| {
                (
                    *user_id,
                    WrappedAccountContentKey {
                        key_version: wrapped.key_version,
                        algorithm: wrapped.algorithm.clone(),
                        ephemeral_public_key: wrapped.ephemeral_public_key.clone(),
                        nonce: wrapped.nonce.clone(),
                        ciphertext: wrapped.ciphertext.clone(),
                        created_at: wrapped.created_at,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let wrapped_ock = status
            .wrapped_ock
            .clone()
            .map(|wrapped| WrappedOrgContentKey {
                key_version: wrapped.key_version,
                algorithm: wrapped.algorithm,
                ephemeral_public_key: wrapped.ephemeral_public_key,
                nonce: wrapped.nonce,
                ciphertext: wrapped.ciphertext,
                created_at: wrapped.created_at,
            });
        let mut enrollment = self.enrollment.write().await;
        if let Some(certificate) = certificate {
            enrollment.certificate = Some(certificate);
        }
        enrollment.user_wrapped_acks = user_wrapped_acks;
        enrollment.wrapped_ock = wrapped_ock;
        enrollment.enrolled_at = if enrollment.user_wrapped_acks.is_empty() {
            None
        } else {
            Some(chrono::Utc::now())
        };
        enrollment.pending_verification_code = if enrollment.user_wrapped_acks.is_empty() {
            enrollment.pending_verification_code.clone()
        } else {
            None
        };
        self.user_ack_cache.clear();
        *self.ock_cache.write().await = None;
        persist_enrollment(&self.root, &enrollment)
    }

    /// Register or refresh this worker's enrollment with the backend and apply
    /// the resulting status to local state.
    pub async fn sync_worker_enrollment(
        &self,
        api: &nenjo::client::NenjoClient,
        api_key_id: Uuid,
        _bootstrap_user_id: Uuid,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        let enrollment_request = self
            .api_enrollment_request_async(api_key_id, metadata)
            .await?;
        let verification_code = enrollment_request.verification_code.clone();

        match api.register_worker_enrollment(&enrollment_request).await {
            Ok(status) => {
                self.apply_backend_enrollment(&status).await?;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    %api_key_id,
                    "Worker enrollment registration failed; continuing with local pending state"
                );
            }
        }

        if matches!(self.enrollment_status().await, EnrollmentStatus::Pending) {
            tracing::info!(
                %api_key_id,
                verification_code = %verification_code,
                "Harness pending approval; match this code on a trusted device before approving"
            );
        }

        Ok(())
    }

    /// Drop a cached user-routed ACK from memory.
    pub async fn clear_cached_ack_for_user(&self, user_id: Uuid) {
        self.user_ack_cache.remove(&user_id);
    }

    /// Drop the cached OCK from memory.
    pub async fn clear_cached_ock(&self) {
        *self.ock_cache.write().await = None;
    }

    /// Load and unwrap a user-routed ACK delivered through enrollment or account-key sync.
    pub async fn load_ack_for_user(&self, user_id: Uuid) -> Result<Option<ContentKey>> {
        if let Some(key) = self.user_ack_cache.get(&user_id) {
            return Ok(Some(key.clone()));
        }

        let wrapped = {
            let enrollment = self.enrollment.read().await;
            enrollment.user_wrapped_acks.get(&user_id).cloned()
        };
        let Some(wrapped) = wrapped else {
            return Ok(None);
        };

        let key = unwrap_ack(&self.identity.encryption_secret()?, &wrapped)?;
        self.user_ack_cache.insert(user_id, key.clone());
        Ok(Some(key))
    }

    /// Load and unwrap the enrolled OCK, if present.
    pub async fn load_ock(&self) -> Result<Option<ContentKey>> {
        if let Some(key) = self.ock_cache.read().await.clone() {
            return Ok(Some(key));
        }

        let wrapped = {
            let enrollment = self.enrollment.read().await;
            enrollment.wrapped_ock.clone()
        };
        let Some(wrapped) = wrapped else {
            return Ok(None);
        };

        let key = unwrap_ock(&self.identity.encryption_secret()?, &wrapped)?;
        *self.ock_cache.write().await = Some(key.clone());
        Ok(Some(key))
    }

    /// Return the key version for a stored actor ACK, if present.
    pub async fn current_key_version_for_user(&self, user_id: Uuid) -> Option<u32> {
        self.enrollment
            .read()
            .await
            .user_wrapped_acks
            .get(&user_id)
            .map(|wrapped| wrapped.key_version)
    }

    /// Persist a wrapped actor ACK delivered through account-key sync.
    pub async fn store_user_ack(
        &self,
        user_id: Uuid,
        wrapped_ack: WrappedAccountContentKey,
    ) -> Result<()> {
        let mut enrollment = self.enrollment.write().await;
        enrollment.user_wrapped_acks.insert(user_id, wrapped_ack);
        self.user_ack_cache.remove(&user_id);
        persist_enrollment(&self.root, &enrollment)
    }
}

impl StoredWorkerIdentity {
    fn public(&self) -> WorkerIdentityPublic {
        WorkerIdentityPublic {
            worker_id: self.worker_id,
            created_at: self.created_at,
            crypto_version: self.crypto_version,
            enc_public_key: self.enc_public_key.clone(),
            sign_public_key: self.sign_public_key.clone(),
        }
    }

    fn encryption_secret(&self) -> Result<StaticSecret> {
        Ok(StaticSecret::from(decode_fixed::<32>(
            &self.enc_secret_key,
            "enc_secret_key",
        )?))
    }
}

fn decode_fixed<const N: usize>(raw: &str, field: &str) -> Result<[u8; N]> {
    let bytes = BASE64
        .decode(raw)
        .with_context(|| format!("Invalid base64 in {field}"))?;
    if bytes.len() != N {
        anyhow::bail!("Invalid {field} length: expected {N}, got {}", bytes.len());
    }
    let mut out = [0_u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        EnrollmentStatus, WorkerAuthProvider, WorkerCertificate, WrappedAccountContentKey,
        WrappedOrgContentKey, wrap_ack_for_recipient,
    };
    use chrono::Utc;
    use nenjo::client::{
        WorkerCertificate as ApiWorkerCertificate, WorkerEnrollmentState,
        WorkerEnrollmentStatusResponse,
    };
    use uuid::Uuid;

    #[tokio::test]
    async fn creates_identity_and_starts_pending() {
        let dir = tempfile::tempdir().unwrap();
        let state = WorkerAuthProvider::load_or_create(dir.path()).unwrap();
        let request = state.enrollment_request(Uuid::nil());

        assert_eq!(request.worker.crypto_version, 1);
        assert_eq!(request.api_key_id, Uuid::nil());
        assert_eq!(request.worker.worker_id, state.identity().worker_id);
        assert_eq!(state.enrollment_status().await, EnrollmentStatus::Pending);
    }

    #[tokio::test]
    async fn persists_identity_across_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let first = WorkerAuthProvider::load_or_create(dir.path()).unwrap();
        let second = WorkerAuthProvider::load_or_create(dir.path()).unwrap();

        assert_eq!(first.identity().worker_id, second.identity().worker_id);
        assert_eq!(
            first.identity().enc_public_key,
            second.identity().enc_public_key
        );
        assert_eq!(
            first.identity().sign_public_key,
            second.identity().sign_public_key
        );
    }

    #[tokio::test]
    async fn loads_wrapped_ack_after_enrollment() {
        let dir = tempfile::tempdir().unwrap();
        let state = WorkerAuthProvider::load_or_create(dir.path()).unwrap();
        let ack = [7_u8; 32];
        let wrapped = wrap_ack_for_recipient(&state.identity().enc_public_key, &ack, 1).unwrap();

        let user_id = Uuid::new_v4();
        state
            .store_enrollment(None, Some(user_id), Some(wrapped), None)
            .await
            .unwrap();

        let loaded = state
            .load_ack_for_user(user_id)
            .await
            .unwrap()
            .expect("ack should load");
        assert_eq!(loaded.as_bytes(), &ack);
        assert_eq!(state.enrollment_status().await, EnrollmentStatus::Pending);
    }

    #[tokio::test]
    async fn active_enrollment_requires_certificate_and_wrapped_ack() {
        let dir = tempfile::tempdir().unwrap();
        let state = WorkerAuthProvider::load_or_create(dir.path()).unwrap();
        let ack = [7_u8; 32];
        let wrapped = wrap_ack_for_recipient(&state.identity().enc_public_key, &ack, 1).unwrap();
        let user_id = Uuid::new_v4();
        let certificate = WorkerCertificate {
            account_id: Uuid::new_v4(),
            api_key_id: Uuid::new_v4(),
            issued_at: Utc::now(),
            enc_public_key: state.identity().enc_public_key,
            sign_public_key: state.identity().sign_public_key,
            signature: "test-signature".into(),
        };

        state
            .store_enrollment(Some(certificate), Some(user_id), Some(wrapped), None)
            .await
            .unwrap();

        assert_eq!(state.enrollment_status().await, EnrollmentStatus::Active);
    }

    #[tokio::test]
    async fn rejects_unsupported_wrap_algorithm() {
        let dir = tempfile::tempdir().unwrap();
        let state = WorkerAuthProvider::load_or_create(dir.path()).unwrap();
        let wrapped = WrappedAccountContentKey {
            key_version: 1,
            algorithm: "x25519-hkdf-sha256-xchacha20poly1305".into(),
            ephemeral_public_key: state.identity().enc_public_key,
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVydGV4dA==".into(),
            created_at: Utc::now(),
        };

        let user_id = Uuid::new_v4();
        state
            .store_enrollment(None, Some(user_id), Some(wrapped), None)
            .await
            .unwrap();

        assert!(state.load_ack_for_user(user_id).await.is_err());
    }

    #[tokio::test]
    async fn pending_backend_enrollment_clears_local_key_material() {
        let dir = tempfile::tempdir().unwrap();
        let state = WorkerAuthProvider::load_or_create(dir.path()).unwrap();
        let ack = [7_u8; 32];
        let actor_user_id = Uuid::new_v4();
        let wrapped_ack =
            wrap_ack_for_recipient(&state.identity().enc_public_key, &ack, 1).unwrap();
        let wrapped_ock = WrappedOrgContentKey {
            key_version: 1,
            algorithm: "x25519-hkdf-sha256-aes-256-gcm".into(),
            ephemeral_public_key: Some("ock-ephemeral".into()),
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVydGV4dA==".into(),
            created_at: Utc::now(),
        };
        state
            .store_enrollment(
                Some(super::WorkerCertificate {
                    account_id: Uuid::new_v4(),
                    api_key_id: Uuid::new_v4(),
                    issued_at: Utc::now(),
                    enc_public_key: state.identity().enc_public_key.clone(),
                    sign_public_key: state.identity().sign_public_key.clone(),
                    signature: "sig".into(),
                }),
                Some(actor_user_id),
                Some(wrapped_ack),
                Some(wrapped_ock),
            )
            .await
            .unwrap();

        state
            .apply_backend_enrollment(&WorkerEnrollmentStatusResponse {
                api_key_id: Uuid::new_v4(),
                state: WorkerEnrollmentState::Pending,
                certificate: Some(ApiWorkerCertificate {
                    account_id: Uuid::new_v4(),
                    api_key_id: Uuid::new_v4(),
                    issued_at: Utc::now(),
                    enc_public_key: "enc".into(),
                    sign_public_key: "sign".into(),
                    signature: "sig".into(),
                }),
                user_wrapped_acks: HashMap::new(),
                wrapped_ock: None,
                metadata: None,
            })
            .await
            .unwrap();

        assert!(
            state
                .load_ack_for_user(actor_user_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(state.load_ock().await.unwrap().is_none());
        let enrollment = state.enrollment().await;
        assert!(enrollment.certificate.is_none());
        assert!(enrollment.user_wrapped_acks.is_empty());
        assert!(enrollment.wrapped_ock.is_none());
    }
}
