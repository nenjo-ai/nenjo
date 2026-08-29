use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
pub use nenjo_platform::ContentScope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

pub(crate) const CURRENT_CRYPTO_VERSION: u32 = 1;
pub(crate) const ACK_LEN: usize = 32;

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

/// Authenticated platform identity that owns one local enrollment snapshot.
///
/// The account and API key travel together so callers cannot refresh or reuse
/// org-scoped key material with only half of the required identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerEnrollmentBinding {
    account_id: Uuid,
    api_key_id: Uuid,
}

impl WorkerEnrollmentBinding {
    pub const fn new(account_id: Uuid, api_key_id: Uuid) -> Self {
        Self {
            account_id,
            api_key_id,
        }
    }

    pub const fn account_id(self) -> Uuid {
        self.account_id
    }

    pub const fn api_key_id(self) -> Uuid {
        self.api_key_id
    }
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

/// One persisted enrollment together with the authenticated platform binding
/// that owns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BoundWorkerEnrollment {
    pub(crate) binding: WorkerEnrollmentBinding,
    pub(crate) enrollment: StoredWorkerEnrollment,
}

/// Versioned on-disk collection of account-local worker enrollments.
///
/// A worker has one long-lived asymmetric identity, but it may be enrolled in
/// multiple platform accounts. Keeping the binding beside its wrapped key
/// material prevents a selected account from ever reading another account's
/// enrollment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredWorkerEnrollments {
    pub(crate) schema_version: u32,
    #[serde(default)]
    pub(crate) selected_binding: Option<WorkerEnrollmentBinding>,
    pub(crate) entries: Vec<BoundWorkerEnrollment>,
}

impl Default for StoredWorkerEnrollments {
    fn default() -> Self {
        Self {
            schema_version: 2,
            selected_binding: None,
            entries: Vec::new(),
        }
    }
}

impl StoredWorkerEnrollments {
    pub(crate) fn selected(&self) -> Option<WorkerEnrollmentBinding> {
        self.selected_binding
    }

    pub(crate) fn selected_enrollment(&self) -> Option<&StoredWorkerEnrollment> {
        self.selected().and_then(|binding| self.get(binding))
    }

    pub(crate) fn get(&self, binding: WorkerEnrollmentBinding) -> Option<&StoredWorkerEnrollment> {
        self.entries
            .iter()
            .find(|entry| entry.binding == binding)
            .map(|entry| &entry.enrollment)
    }

    pub(crate) fn get_or_insert_mut(
        &mut self,
        binding: WorkerEnrollmentBinding,
    ) -> &mut StoredWorkerEnrollment {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.binding == binding)
        {
            return &mut self.entries[index].enrollment;
        }
        self.entries.push(BoundWorkerEnrollment {
            binding,
            enrollment: StoredWorkerEnrollment::default(),
        });
        &mut self
            .entries
            .last_mut()
            .expect("entry was inserted")
            .enrollment
    }

    pub(crate) fn remove(
        &mut self,
        binding: WorkerEnrollmentBinding,
    ) -> Option<StoredWorkerEnrollment> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.binding == binding)?;
        Some(self.entries.remove(index).enrollment)
    }
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
