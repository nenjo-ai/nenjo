//! Transport trait — the abstraction point for pluggable message delivery.

use crate::EventBusError;

/// A message received from the transport layer.
pub struct Message {
    /// Raw payload bytes (UTF-8 JSON).
    pub payload: Vec<u8>,
    /// Opaque handle for acknowledging the message.
    ack_fn: Box<dyn AckHandle>,
}

impl Message {
    /// Create a new message with an ack handle.
    ///
    /// Intended for [`Transport`] implementors wrapping raw transport data.
    /// Use [`NoOpAck`] when no acknowledgment is needed.
    pub fn new(payload: Vec<u8>, ack_fn: impl AckHandle + 'static) -> Self {
        Self {
            payload,
            ack_fn: Box::new(ack_fn),
        }
    }

    /// Acknowledge successful processing of this message.
    ///
    /// Must be called after handling; unacked messages may be redelivered
    /// depending on the transport.
    pub async fn ack(self) -> Result<(), EventBusError> {
        self.ack_fn.ack().await
    }

    /// Get the payload as a UTF-8 string slice.
    pub fn as_str(&self) -> Result<&str, EventBusError> {
        std::str::from_utf8(&self.payload).map_err(|e| EventBusError::Deserialize {
            message: format!("invalid UTF-8: {e}"),
            raw: String::from_utf8_lossy(&self.payload).into_owned(),
        })
    }
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Message")
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

/// Handle for acknowledging a message back to the transport.
///
/// Implementors should make `ack()` idempotent — calling it twice should
/// not error.
pub trait AckHandle: Send + Sync {
    /// Acknowledge the message.
    fn ack(
        self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EventBusError>> + Send>>;
}

/// No-op ack handle for transports that don't require acknowledgment.
///
/// Useful for in-memory or fire-and-forget transports where redelivery is not a concern.
pub struct NoOpAck;

impl AckHandle for NoOpAck {
    fn ack(
        self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EventBusError>> + Send>>
    {
        Box::pin(async { Ok(()) })
    }
}

/// The transport layer abstraction.
///
/// Implementations handle connection management, serialization boundaries,
/// and delivery guarantees. The event bus calls these methods with raw bytes
/// and subjects — it handles JSON ser/de itself.
///
/// # Implementing a transport
///
/// Implement [`publish`](Self::publish) to send raw bytes to a subject, and
/// [`subscribe`](Self::subscribe) to spawn a background task that feeds
/// incoming messages into the returned channel. Wrap each incoming message
/// with [`Message::new`] and a suitable [`AckHandle`] (or [`NoOpAck`]).
pub trait Transport: Send + Sync + 'static {
    /// Publish a message to the given subject.
    fn publish(
        &self,
        subject: &str,
        payload: &[u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EventBusError>> + Send + '_>>;

    /// Subscribe to a subject and return a receiver for incoming messages.
    ///
    /// The returned receiver yields [`Message`] values that must be
    /// acknowledged after processing.
    fn subscribe(
        &self,
        subject: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<tokio::sync::mpsc::Receiver<Message>, EventBusError>,
                > + Send
                + '_,
        >,
    >;

    /// The unique instance ID for this worker process.
    ///
    /// Used for consumer naming and presence tracking. Each transport
    /// instance generates a unique ID at construction time.
    fn worker_id(&self) -> uuid::Uuid;
}
