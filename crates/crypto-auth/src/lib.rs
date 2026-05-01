//! Worker crypto enrollment and key-state primitives.
//!
//! This crate owns:
//! - persisted worker identity/enrollment state
//! - wrapped ACK/OCK unwrap and caching
//! - the enrollment-backed key provider used by secure envelope codecs

mod key_provider;
mod provider;

/// Enrollment-backed key provider used by secure-envelope codecs.
pub use key_provider::{EnrollmentBackedKeyProvider, EnvelopeKeyProvider};
/// Worker crypto identity, enrollment, and wrapped-key primitives.
pub use provider::{
    ContentKey, ContentScope, EnrollmentStatus, StoredWorkerEnrollment, WorkerAuthProvider,
    WorkerCertificate, WorkerEnrollmentRequest, WorkerIdentityPublic, WrappedAccountContentKey,
    WrappedOrgContentKey,
};
