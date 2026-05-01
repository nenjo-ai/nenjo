use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tokio::sync::RwLock;
use uuid::Uuid;
use x25519_dalek::StaticSecret;

mod storage;
mod types;
mod wrap;

pub use types::{
    AccountContentKey, EnrollmentStatus, StoredWorkerEnrollment, WorkerCertificate,
    WorkerEnrollmentRequest, WorkerIdentityPublic, WrappedAccountContentKey,
};
#[cfg(test)]
pub(crate) use wrap::wrap_ack_for_recipient;

use self::storage::{
    generate_verification_code, load_enrollment, load_or_create_identity, persist_enrollment,
};
use self::types::StoredWorkerIdentity;
use self::wrap::unwrap_ack;

/// Owns the worker's long-lived local identity and enrolled account content key state.
///
/// This is the worker-side trust/bootstrap boundary:
/// - creates and persists the local X25519 + Ed25519 identity
/// - persists backend-issued enrollment state
/// - unwraps the enrolled `ACK` for local use
#[derive(Clone)]
pub struct WorkerAuthProvider {
    root: PathBuf,
    identity: Arc<StoredWorkerIdentity>,
    enrollment: Arc<RwLock<StoredWorkerEnrollment>>,
    ack_cache: Arc<RwLock<Option<AccountContentKey>>>,
}

impl WorkerAuthProvider {
    /// Loads existing local worker crypto state or creates a fresh identity and empty enrollment.
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
            ack_cache: Arc::new(RwLock::new(None)),
        })
    }

    /// Returns the current worker public identity.
    pub fn identity(&self) -> WorkerIdentityPublic {
        self.identity.public()
    }

    /// Builds the local enrollment request record used by non-API flows and tests.
    pub fn enrollment_request(&self, api_key_id: Uuid) -> WorkerEnrollmentRequest {
        WorkerEnrollmentRequest {
            api_key_id,
            worker: self.identity(),
            requested_at: chrono::Utc::now(),
            verification_code: self.pending_verification_code(),
        }
    }

    /// Builds the API enrollment request using the current local worker identity.
    pub fn api_enrollment_request(
        &self,
        api_key_id: Uuid,
    ) -> nenjo::client::WorkerEnrollmentRequest {
        let worker = self.identity();
        nenjo::client::WorkerEnrollmentRequest {
            api_key_id,
            requested_at: chrono::Utc::now(),
            crypto_version: worker.crypto_version,
            enc_public_key: worker.enc_public_key,
            sign_public_key: worker.sign_public_key,
            verification_code: self.pending_verification_code(),
        }
    }

    /// Async variant of [`Self::api_enrollment_request`] that persists a stable
    /// per-request verification code before returning.
    pub async fn api_enrollment_request_async(
        &self,
        api_key_id: Uuid,
    ) -> Result<nenjo::client::WorkerEnrollmentRequest> {
        let worker = self.identity();
        Ok(nenjo::client::WorkerEnrollmentRequest {
            api_key_id,
            requested_at: chrono::Utc::now(),
            crypto_version: worker.crypto_version,
            enc_public_key: worker.enc_public_key,
            sign_public_key: worker.sign_public_key,
            verification_code: self.pending_verification_code_async().await?,
        })
    }

    /// Returns the currently pending verification code, creating and persisting
    /// one if the worker is not already waiting for approval.
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

    /// Returns the locally stored enrollment snapshot.
    pub async fn enrollment(&self) -> StoredWorkerEnrollment {
        self.enrollment.read().await.clone()
    }

    /// Returns the key version of the currently enrolled wrapped `ACK`, if any.
    pub async fn current_key_version(&self) -> Option<u32> {
        self.enrollment
            .read()
            .await
            .wrapped_ack
            .as_ref()
            .map(|wrapped| wrapped.key_version)
    }

    /// Returns whether the worker currently has an enrolled wrapped `ACK`.
    pub async fn enrollment_status(&self) -> EnrollmentStatus {
        let enrollment = self.enrollment.read().await;
        if enrollment.wrapped_ack.is_some() {
            EnrollmentStatus::Active
        } else {
            EnrollmentStatus::Pending
        }
    }

    /// Persists the latest backend-issued certificate and wrapped `ACK`.
    pub async fn store_enrollment(
        &self,
        certificate: Option<WorkerCertificate>,
        wrapped_ack: Option<WrappedAccountContentKey>,
    ) -> Result<()> {
        let mut enrollment = self.enrollment.write().await;
        if let Some(certificate) = certificate {
            enrollment.certificate = Some(certificate);
        }
        if let Some(wrapped_ack) = wrapped_ack {
            enrollment.wrapped_ack = Some(wrapped_ack);
            enrollment.enrolled_at = Some(chrono::Utc::now());
            enrollment.pending_verification_code = None;
            *self.ack_cache.write().await = None;
        }
        persist_enrollment(&self.root, &enrollment)
    }

    /// Applies the backend enrollment response to local state.
    pub async fn apply_backend_enrollment(
        &self,
        status: &nenjo::client::WorkerEnrollmentStatusResponse,
    ) -> Result<()> {
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
        let wrapped_ack = status
            .wrapped_ack
            .clone()
            .map(|wrapped| WrappedAccountContentKey {
                key_version: wrapped.key_version,
                algorithm: wrapped.algorithm,
                ephemeral_public_key: wrapped.ephemeral_public_key,
                nonce: wrapped.nonce,
                ciphertext: wrapped.ciphertext,
                created_at: wrapped.created_at,
            });
        self.store_enrollment(certificate, wrapped_ack).await
    }

    /// Registers or refreshes this worker's enrollment with the backend.
    pub async fn sync_worker_enrollment(
        &self,
        api: &nenjo::client::NenjoClient,
        api_key_id: Uuid,
    ) -> Result<()> {
        let enrollment_request = self.api_enrollment_request_async(api_key_id).await?;
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

    /// Drops the in-memory `ACK` cache. The persisted wrapped key remains intact.
    pub async fn clear_cached_ack(&self) {
        *self.ack_cache.write().await = None;
    }

    /// Loads and unwraps the current `ACK`, if the worker has been approved.
    pub async fn load_ack(&self) -> Result<Option<AccountContentKey>> {
        if let Some(key) = self.ack_cache.read().await.clone() {
            return Ok(Some(key));
        }

        let wrapped = {
            let enrollment = self.enrollment.read().await;
            enrollment.wrapped_ack.clone()
        };
        let Some(wrapped) = wrapped else {
            return Ok(None);
        };

        let key = unwrap_ack(&self.identity.encryption_secret()?, &wrapped)?;
        *self.ack_cache.write().await = Some(key.clone());
        Ok(Some(key))
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
    use super::{
        AccountContentKey, EnrollmentStatus, WorkerAuthProvider, WrappedAccountContentKey,
        wrap_ack_for_recipient,
    };
    use chrono::Utc;
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

        state.store_enrollment(None, Some(wrapped)).await.unwrap();

        let loaded = state.load_ack().await.unwrap().expect("ack should load");
        assert_eq!(loaded.as_bytes(), &ack);
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

        state.store_enrollment(None, Some(wrapped)).await.unwrap();

        assert!(state.load_ack().await.is_err());
    }

    #[test]
    fn account_content_key_exposes_stable_bytes() {
        let key = AccountContentKey::from_bytes([9_u8; 32]);
        assert_eq!(key.as_bytes(), &[9_u8; 32]);
    }
}
