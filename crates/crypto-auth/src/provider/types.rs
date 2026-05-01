use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

pub(crate) const CURRENT_CRYPTO_VERSION: u32 = 1;
pub(crate) const ACK_LEN: usize = 32;

/// Scope used when selecting between user-private and org-shared content keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentScope {
    /// User-private account content encrypted with an ACK.
    User,
    /// Org-shared content encrypted with an OCK.
    Org,
}

impl ContentScope {
    /// Infer the content scope from an encrypted payload's declared scope marker.
    pub fn from_payload(payload: &EncryptedPayload) -> Self {
        if payload.encryption_scope.as_deref() == Some("org") {
            Self::Org
        } else {
            Self::User
        }
    }

    /// Return the serialized `encryption_scope` value used on payloads.
    pub fn encryption_scope_value(self) -> Option<&'static str> {
        match self {
            Self::User => None,
            Self::Org => Some("org"),
        }
    }
}

/// In-memory 32-byte content key used for envelope and payload crypto operations.
#[derive(Clone)]
pub struct ContentKey(pub(crate) Arc<Zeroizing<[u8; ACK_LEN]>>);

impl ContentKey {
    /// Construct a content key from raw 32-byte secret material.
    pub fn from_bytes(bytes: [u8; ACK_LEN]) -> Self {
        Self(Arc::new(Zeroizing::new(bytes)))
    }

    /// Borrow the raw key bytes for local cryptographic operations.
    pub fn as_bytes(&self) -> &[u8; ACK_LEN] {
        self.0.as_ref()
    }
}

/// High-level enrollment state from the worker's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentStatus {
    /// Worker has identity material but no user-routed wrapped ACK enrollment yet.
    Pending,
    /// Worker has at least one active wrapped ACK associated with a user id.
    Active,
}

/// Public worker identity that is safe to send to the backend or display to users.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerIdentityPublic {
    pub worker_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub crypto_version: u32,
    pub enc_public_key: String,
    pub sign_public_key: String,
}

/// Local enrollment request record used for approval workflows and diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEnrollmentRequest {
    pub api_key_id: Uuid,
    pub worker: WorkerIdentityPublic,
    pub requested_at: DateTime<Utc>,
    pub verification_code: String,
}

/// Persisted local worker enrollment state, including user-routed wrapped
/// ACKs and org-scoped OCK material.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredWorkerEnrollment {
    pub certificate: Option<WorkerCertificate>,
    pub wrapped_ock: Option<WrappedOrgContentKey>,
    #[serde(default)]
    pub user_wrapped_acks: HashMap<Uuid, WrappedAccountContentKey>,
    pub enrolled_at: Option<DateTime<Utc>>,
    pub pending_verification_code: Option<String>,
}

/// Backend-signed worker certificate binding public keys to an account and API key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCertificate {
    pub account_id: Uuid,
    pub api_key_id: Uuid,
    pub issued_at: DateTime<Utc>,
    pub enc_public_key: String,
    pub sign_public_key: String,
    pub signature: String,
}

/// Wrapped account content key addressed to the worker's encryption identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedAccountContentKey {
    pub key_version: u32,
    pub algorithm: String,
    pub ephemeral_public_key: String,
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: DateTime<Utc>,
}

/// Wrapped org content key addressed to the worker's encryption identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedOrgContentKey {
    pub key_version: u32,
    pub algorithm: String,
    pub ephemeral_public_key: Option<String>,
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
