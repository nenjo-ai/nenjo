//! Stream event abstractions for the harness.
//!
//! Re-exports `StreamEvent` from the canonical event types and provides a
//! `StreamSender` trait for publishing events to different transports.

use anyhow::Result;
use async_trait::async_trait;

// Re-export the canonical event types.
pub use nenjo_events::Response;
pub use nenjo_events::StreamEvent;

/// Trait for sending stream events to the frontend.
///
/// Implementations forward events over NATS, WebSocket, or discard them (no-op).
#[async_trait]
pub trait StreamSender: Send + Sync {
    /// Send a stream event. Errors are non-fatal and should be logged by the implementation.
    async fn send(&self, event: StreamEvent) -> Result<()>;

    /// Send a raw JSON message (used for step events, completion messages, etc.).
    async fn send_json(&self, json: serde_json::Value) -> Result<()>;
}

/// A no-op sender that discards all events. Used for tests and cron execution.
pub struct NoOpSender;

#[async_trait]
impl StreamSender for NoOpSender {
    async fn send(&self, _event: StreamEvent) -> Result<()> {
        Ok(())
    }

    async fn send_json(&self, _json: serde_json::Value) -> Result<()> {
        Ok(())
    }
}

