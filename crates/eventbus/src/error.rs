//! Error types for the event bus.

/// Errors returned by event bus operations.
#[derive(Debug, thiserror::Error)]
pub enum EventBusError {
    /// Transport-level failure (connection lost, publish rejected, etc.).
    #[error("transport error: {0}")]
    Transport(String),

    /// Failed to serialize an outgoing event.
    #[error("serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),

    /// Failed to deserialize an incoming event.
    #[error("deserialization failed: {message}")]
    Deserialize { message: String, raw: String },

    /// The event bus has not been fully configured.
    #[error("builder error: {0}")]
    Builder(String),

    /// The receive stream has ended.
    #[error("event stream closed")]
    StreamClosed,
}
