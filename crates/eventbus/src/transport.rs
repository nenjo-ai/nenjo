//! Transport trait — the abstraction point for pluggable message delivery.

use crate::EventBusError;
use nenjo_events::Capability;

#[derive(Debug, Clone)]
pub enum Subscription {
    Subject(String),
    WorkerCommands {
        worker_id: uuid::Uuid,
        capabilities: Vec<Capability>,
    },
}

impl Subscription {
    pub fn worker_commands(worker_id: uuid::Uuid, capabilities: Vec<Capability>) -> Self {
        Self::WorkerCommands {
            worker_id,
            capabilities,
        }
    }
}

/// A message received from the transport layer.
pub struct Message {
    /// Raw payload bytes (UTF-8 JSON).
    pub payload: Vec<u8>,
    /// Transport-specific delivery source, useful for diagnosing duplicate deliveries.
    pub source: Option<MessageSource>,
    /// Opaque handle for acknowledging the message.
    ack_fn: Box<dyn AckHandle>,
}

#[derive(Debug, Clone)]
pub struct MessageSource {
    pub stream: Option<String>,
    pub consumer: Option<String>,
    pub filter_subject: Option<String>,
    pub subject: Option<String>,
}

impl Message {
    /// Create a new message with an ack handle.
    ///
    /// Intended for [`Transport`] implementors wrapping raw transport data.
    /// Use [`NoOpAck`] when no acknowledgment is needed.
    pub fn new(payload: Vec<u8>, ack_fn: impl AckHandle + 'static) -> Self {
        Self {
            payload,
            source: None,
            ack_fn: Box::new(ack_fn),
        }
    }

    pub fn with_source(mut self, source: MessageSource) -> Self {
        self.source = Some(source);
        self
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
#[async_trait::async_trait]
pub trait AckHandle: Send + Sync {
    /// Acknowledge the message.
    async fn ack(self: Box<Self>) -> Result<(), EventBusError>;
}

/// No-op ack handle for transports that don't require acknowledgment.
///
/// Useful for in-memory or fire-and-forget transports where redelivery is not a concern.
pub struct NoOpAck;

#[async_trait::async_trait]
impl AckHandle for NoOpAck {
    async fn ack(self: Box<Self>) -> Result<(), EventBusError> {
        Ok(())
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
#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Publish a message to the given subject.
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), EventBusError>;

    /// Subscribe to a subject and return a receiver for incoming messages.
    ///
    /// The returned receiver yields [`Message`] values that must be
    /// acknowledged after processing.
    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<tokio::sync::mpsc::Receiver<Message>, EventBusError> {
        match subscription {
            Subscription::Subject(subject) => self.subscribe_subject(&subject).await,
            Subscription::WorkerCommands { .. } => {
                self.subscribe_subject(&nenjo_events::requests_subject_all())
                    .await
            }
        }
    }

    /// Subscribe to one broker subject.
    async fn subscribe_subject(
        &self,
        subject: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<Message>, EventBusError>;

    /// The unique instance ID for this worker process.
    ///
    /// Used for consumer naming and presence tracking. Each transport
    /// instance generates a unique ID at construction time.
    fn worker_id(&self) -> uuid::Uuid;
}
