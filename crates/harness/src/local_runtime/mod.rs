//! File-backed local session runtime for embedded harness users.
//!
//! Enable the `local-runtime` feature to use these filesystem-backed session,
//! transcript, trace, checkpoint, and lease implementations without depending
//! on `nenjo-worker`.

mod event_store;
mod lease_store;
mod record_store;
mod runtime;

pub use event_store::{
    FileCheckpointStore, FileSessionStores, FileTraceStore, FileTranscriptStore,
    SessionCleanupReport,
};
pub use record_store::FileSessionStore;
pub use runtime::{
    CronSessionRecovery, DomainSessionRecovery, FileSessionRuntime, HeartbeatSessionRecovery,
    SessionRecoveryHandler,
};
