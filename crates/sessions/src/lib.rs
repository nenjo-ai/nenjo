//! Shared session contracts for Nenjo.
//!
//! This crate contains transport- and storage-agnostic session types and
//! traits. Runtime-specific implementations live in the worker harness.

pub mod content;
pub mod coordinator;
pub mod store;
pub mod types;
pub mod updates;

pub use content::SessionContentStore;
pub use coordinator::{SessionCoordinator, SessionLeaseGrant};
pub use store::SessionStore;
pub use types::{
    CronScheduleState, DomainState, ExecutionPhase, HeartbeatScheduleState, RunCompletion,
    ScheduleState, SessionCheckpoint, SessionKind, SessionLease, SessionRecord, SessionRefs,
    SessionStatus, SessionSummary, SessionTranscriptChatMessage, SessionTranscriptEvent,
    SessionTranscriptEventPayload, TranscriptState, WorktreeSnapshot,
};
pub use updates::SessionUpdate;
