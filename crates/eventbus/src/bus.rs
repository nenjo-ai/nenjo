//! Raw event bus built on top of a [`Transport`].

use std::sync::Arc;

use nenjo_events::Envelope;
use tokio::sync::mpsc;

use crate::error::EventBusError;
use crate::transport::{Message, Subscription, Transport};

/// Raw event bus for sending and receiving transport envelopes.
///
/// Built via [`EventBusBuilder`]. The bus owns its transport and manages
/// the subscription lifecycle.
pub struct EventBus<T: Transport> {
    transport: Arc<T>,
    rx: mpsc::Receiver<Message>,
}

/// Cloneable outbound handle for publishing raw envelopes.
pub struct EventBusPublisher<T: Transport> {
    transport: Arc<T>,
}

impl<T: Transport> Clone for EventBusPublisher<T> {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
        }
    }
}

impl<T: Transport> std::fmt::Debug for EventBusPublisher<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBusPublisher").finish_non_exhaustive()
    }
}

impl<T: Transport> EventBusPublisher<T> {
    /// Send a raw envelope to a subject.
    pub async fn send_envelope(
        &self,
        subject: &str,
        envelope: &Envelope,
    ) -> Result<(), EventBusError> {
        let bytes = serde_json::to_vec(envelope)?;
        self.transport.publish(subject, &bytes).await
    }
}

impl<T: Transport> std::fmt::Debug for EventBus<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus").finish_non_exhaustive()
    }
}

impl<T: Transport> EventBus<T> {
    /// Create a builder for configuring an `EventBus`.
    pub fn builder() -> EventBusBuilder<T> {
        EventBusBuilder::new()
    }

    /// Access the underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Create a cloneable outbound publisher handle.
    pub fn publisher(&self) -> EventBusPublisher<T> {
        EventBusPublisher {
            transport: Arc::clone(&self.transport),
        }
    }

    /// Send a raw envelope to a subject.
    pub async fn send_envelope(
        &self,
        subject: &str,
        envelope: &Envelope,
    ) -> Result<(), EventBusError> {
        let bytes = serde_json::to_vec(envelope)?;
        self.transport.publish(subject, &bytes).await
    }

    /// Receive the next raw envelope from the bus.
    ///
    /// Returns `None` when the transport stream ends. The returned
    /// [`ReceivedEnvelope`] must be acknowledged after processing.
    pub async fn recv_envelope(&mut self) -> Result<Option<ReceivedEnvelope>, EventBusError> {
        let msg = match self.rx.recv().await {
            Some(m) => m,
            None => return Ok(None),
        };

        let text = msg.as_str()?;
        let envelope: Envelope =
            serde_json::from_str(text).map_err(|e| EventBusError::Deserialize {
                message: format!("invalid envelope: {e}"),
                raw: text.to_string(),
            })?;

        Ok(Some(ReceivedEnvelope { envelope, msg }))
    }
}

/// A raw envelope received from the bus, paired with its ack handle.
///
/// Call [`ack()`](Self::ack) after processing to prevent redelivery.
pub struct ReceivedEnvelope {
    /// The deserialized transport envelope.
    pub envelope: Envelope,
    pub msg: Message,
}

impl std::fmt::Debug for ReceivedEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceivedEnvelope")
            .field("envelope", &self.envelope)
            .finish_non_exhaustive()
    }
}

impl ReceivedEnvelope {
    /// Acknowledge processing of this envelope.
    pub async fn ack(self) -> Result<(), EventBusError> {
        self.msg.ack().await
    }
}

/// Builder for constructing an [`EventBus`].
///
/// Requires a [`Transport`] before building.
pub struct EventBusBuilder<T: Transport> {
    transport: Option<T>,
    subscription: Subscription,
}

impl<T: Transport> std::fmt::Debug for EventBusBuilder<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBusBuilder")
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> EventBusBuilder<T> {
    fn new() -> Self {
        Self {
            transport: None,
            subscription: Subscription::Subject(nenjo_events::requests_subject_all()),
        }
    }

    /// Set the transport implementation (required).
    pub fn transport(mut self, transport: T) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Set the transport subscription.
    pub fn subscription(mut self, subscription: Subscription) -> Self {
        self.subscription = subscription;
        self
    }

    /// Build the event bus, establishing the transport subscription.
    ///
    /// Returns [`EventBusError::Builder`] if `transport` was not set, or a
    /// transport error if the subscription fails.
    pub async fn build(self) -> Result<EventBus<T>, EventBusError> {
        let transport = self
            .transport
            .ok_or_else(|| EventBusError::Builder("transport is required".into()))?;

        let rx = transport.subscribe(self.subscription).await?;

        Ok(EventBus {
            transport: Arc::new(transport),
            rx,
        })
    }
}
