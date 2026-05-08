//! Raw event bus built on top of a [`Transport`].

use nenjo_events::Envelope;
use tokio::sync::mpsc;

use crate::error::EventBusError;
use crate::transport::{Message, Transport};

/// Raw event bus for sending and receiving transport envelopes.
///
/// Built via [`EventBusBuilder`]. The bus owns its transport and manages
/// the subscription lifecycle.
pub struct EventBus<T: Transport> {
    transport: T,
    rx: mpsc::Receiver<Message>,
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
    /// Which subject to subscribe to. Defaults to `requests.*`.
    subscribe_subject: Option<String>,
}

impl<T: Transport> std::fmt::Debug for EventBusBuilder<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBusBuilder")
            .field("subscribe_subject", &self.subscribe_subject)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> EventBusBuilder<T> {
    fn new() -> Self {
        Self {
            transport: None,
            subscribe_subject: None,
        }
    }

    /// Set the transport implementation (required).
    pub fn transport(mut self, transport: T) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Override the subject to subscribe to.
    ///
    /// By default, the bus subscribes to `requests.*`.
    pub fn subscribe_subject(mut self, subject: impl Into<String>) -> Self {
        self.subscribe_subject = Some(subject.into());
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

        let subject = self
            .subscribe_subject
            .unwrap_or_else(nenjo_events::requests_subject_all);

        let rx = transport.subscribe(&subject).await?;

        Ok(EventBus { transport, rx })
    }
}
