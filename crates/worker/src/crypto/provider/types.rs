use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

pub(crate) const CURRENT_CRYPTO_VERSION: u32 = 1;
pub(crate) const ACK_LEN: usize = 32;

/// In-memory account content key used for encrypting and decrypting protected envelopes.
#[derive(Clone)]
pub struct AccountContentKey(pub(crate) Arc<Zeroizing<[u8; ACK_LEN]>>);

impl AccountContentKey {
    /// Builds an account content key from the provided raw 32-byte secret.
    pub fn from_bytes(bytes: [u8; ACK_LEN]) -> Self {
        Self(Arc::new(Zeroizing::new(bytes)))
    }

    /// Exposes the wrapped key material for local cryptographic operations.
    pub fn as_bytes(&self) -> &[u8; ACK_LEN] {
        self.0.as_ref()
    }
}

/// Current worker enrollment state from the local worker's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentStatus {
    Pending,
    Active,
}

/// Public portion of the worker identity that is safe to send to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerIdentityPublic {
    pub worker_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub crypto_version: u32,
    pub enc_public_key: String,
    pub sign_public_key: String,
}

/// Enrollment request payload persisted locally and shown for approval flows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEnrollmentRequest {
    pub api_key_id: Uuid,
    pub worker: WorkerIdentityPublic,
    pub requested_at: DateTime<Utc>,
    pub verification_code: String,
}

/// Persisted enrollment state stored alongside the worker identity.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredWorkerEnrollment {
    pub certificate: Option<WorkerCertificate>,
    pub wrapped_ack: Option<WrappedAccountContentKey>,
    pub enrolled_at: Option<DateTime<Utc>>,
    pub pending_verification_code: Option<String>,
}

/// Browser-signed certificate binding the approved worker public keys to an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCertificate {
    pub account_id: Uuid,
    pub api_key_id: Uuid,
    pub issued_at: DateTime<Utc>,
    pub enc_public_key: String,
    pub sign_public_key: String,
    pub signature: String,
}

/// Wrapped `ACK` delivered by the backend after worker approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedAccountContentKey {
    pub key_version: u32,
    pub algorithm: String,
    pub ephemeral_public_key: String,
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredWorkerIdentity {
    pub(crate) worker_id: Uuid,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) crypto_version: u32,
    pub(crate) enc_secret_key: String,
    pub(crate) enc_public_key: String,
    pub(crate) sign_secret_key: String,
    pub(crate) sign_public_key: String,
}
