//! Shared session contracts for Nenjo.
//!
//! This crate contains transport- and storage-agnostic session types and
//! traits. Runtime-specific implementations live in the worker harness.

pub mod checkpoint;
pub mod lease;
pub mod runtime;
pub mod store;
pub mod trace;
pub mod transcript;
pub mod types;
pub mod updates;

pub use checkpoint::{CheckpointQuery, CheckpointStore};
pub use lease::SessionLeaseGrant;
pub use runtime::{
    ChatSessionUpsert, CheckpointRecord, DomainSessionUpsert, NoopSessionRuntime,
    SchedulerSessionUpsert, SessionCheckpointUpdate, SessionLeaseRequest, SessionOwnerKind,
    SessionRuntime, SessionRuntimeEvent, SessionRuntimeEventType, SessionTranscriptAppend,
    SessionTranscriptRecord, SessionTransition, SessionUpsert, SessionWriteOutcome,
    TaskSessionUpsert,
};
pub use store::SessionStore;
pub use trace::{TokenUsage, TraceEvent, TracePhase, TraceQuery, TraceStore};
pub use transcript::{TranscriptQuery, TranscriptStore};
pub use types::{
    CronScheduleState, DomainState, ExecutionPhase, HeartbeatScheduleState, RunCompletion,
    ScheduleState, SchedulerRuntimeSnapshot, SessionCheckpoint, SessionKind, SessionLease,
    SessionRecord, SessionRefs, SessionStatus, SessionSummary, SessionTranscriptChatMessage,
    SessionTranscriptEvent, SessionTranscriptEventPayload, TranscriptState, WorktreeSnapshot,
};
pub use updates::SessionUpdate;
