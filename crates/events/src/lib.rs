//! # nenjo-events
//!
//! Canonical event types for the Nenjo agent platform message bus.
//!
//! This crate defines every event that flows between the Nenjo backend and
//! agent harnesses over NATS JetStream. It is transport-agnostic — the types
//! serialize to/from JSON and can be used with NATS, WebSockets, or any other
//! message transport.
//!
//! ## Event directions
//!
//! | Direction | Worker local subject | PLATFORM subject | Rust type |
//! |-----------|---------------------|------------------|-----------|
//! | Backend → Harness | `work_requests.<capability>` / `worker_requests.<worker_id>.<capability>` / `broadcast_requests.<capability>` | `work_requests.<org_id>.<capability>` / `worker_requests.<org_id>.<worker_id>.<capability>` / `broadcast_requests.<org_id>.<capability>` | [`Command`] |
//! | Harness → Backend | `responses.<org_id>.<user_id>` / `streams.chat.<session_id>` | `responses.<org_id>.<user_id>` / `streams.chat.<session_id>` | [`Response`] |
//!
//! ## Wire format
//!
//! Events are wrapped in an [`Envelope`] for delivery tracking:
//!
//! ```json
//! {
//!   "message_id": "550e8400-...",
//!   "user_id": "6ba7b810-...",
//!   "payload": { "type": "chat.message", ... },
//!   "created_at": "2026-03-25T12:00:00Z",
//!   "attempt": 1
//! }
//! ```

mod capability;
mod command;
mod content;
mod envelope;
mod response;
mod subject;

pub use capability::*;
pub use command::*;
pub use content::*;
pub use envelope::*;
pub use response::*;
pub use subject::*;
