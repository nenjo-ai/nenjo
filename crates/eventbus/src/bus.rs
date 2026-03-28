//! The typed event bus built on top of a [`Transport`].

use nenjo_events::{Command, Envelope, Response};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::EventBusError;
use crate::transport::{Message, Transport};

/// Typed event bus for sending commands and receiving responses.
///
/// Built via [`EventBusBuilder`]. The bus owns its transport and manages
/// the subscription lifecycle.
pub struct EventBus<T: Transport> {
    transport: T,
    user_id: Uuid,
    rx: mpsc::Receiver<Message>,
}

impl<T: Transport> std::fmt::Debug for EventBus<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> EventBus<T> {
    /// Create a builder for configuring an `EventBus`.
    pub fn builder() -> EventBusBuilder<T> {
        EventBusBuilder::new()
    }

    /// The user ID this bus is scoped to.
    pub fn user_id(&self) -> Uuid {
        self.user_id
    }

    /// Send a command to the harness.
    ///
    /// The command is wrapped in an [`Envelope`] and published to
    /// `agent.requests.<user_id>.<capability>`.
    pub async fn send_command(&self, command: Command) -> Result<(), EventBusError> {
        let capability = command.capability();
        let payload = serde_json::to_value(&command)?;
        let envelope = Envelope::new(self.user_id, payload);
        let bytes = serde_json::to_vec(&envelope)?;
        let subject = nenjo_events::requests_subject(self.user_id, capability);

        self.transport.publish(&subject, &bytes).await?;
        debug!(user_id = %self.user_id, %capability, "command sent");
        Ok(())
    }

    /// Send a response back to the backend.
    ///
    /// The response is wrapped in an [`Envelope`] and published to
    /// `agent.responses.<user_id>`.
    pub async fn send_response(&self, response: Response) -> Result<(), EventBusError> {
        let payload = serde_json::to_value(&response)?;
        let envelope = Envelope::new(self.user_id, payload);
        let bytes = serde_json::to_vec(&envelope)?;
        let subject = nenjo_events::responses_subject(self.user_id);

        self.transport.publish(&subject, &bytes).await?;
        debug!(user_id = %self.user_id, "response sent");
        Ok(())
    }

    /// Receive the next command from the bus.
    ///
    /// Returns `None` when the transport stream ends. The returned
    /// [`ReceivedCommand`] must be acknowledged after processing.
    pub async fn recv_command(&mut self) -> Result<Option<ReceivedCommand>, EventBusError> {
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

        let command: Command = serde_json::from_value(envelope.payload.clone()).map_err(|e| {
            warn!(
                error = %e,
                raw = %envelope.payload,
                "failed to parse command payload"
            );
            EventBusError::Deserialize {
                message: format!("invalid command payload: {e}"),
                raw: envelope.payload.to_string(),
            }
        })?;

        Ok(Some(ReceivedCommand {
            command,
            envelope,
            msg,
        }))
    }

    /// Receive the next response from the bus.
    ///
    /// Returns `None` when the transport stream ends. The returned
    /// [`ReceivedResponse`] must be acknowledged after processing.
    pub async fn recv_response(&mut self) -> Result<Option<ReceivedResponse>, EventBusError> {
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

        let response: Response = serde_json::from_value(envelope.payload.clone()).map_err(|e| {
            warn!(
                error = %e,
                raw = %envelope.payload,
                "failed to parse response payload"
            );
            EventBusError::Deserialize {
                message: format!("invalid response payload: {e}"),
                raw: envelope.payload.to_string(),
            }
        })?;

        Ok(Some(ReceivedResponse {
            response,
            envelope,
            msg,
        }))
    }

    /// Access the underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

// ---------------------------------------------------------------------------
// Received wrappers (carry ack handle)
// ---------------------------------------------------------------------------

/// A command received from the bus, paired with its envelope and ack handle.
///
/// Call [`ack()`](Self::ack) after processing to prevent redelivery.
pub struct ReceivedCommand {
    /// The deserialized command payload.
    pub command: Command,
    /// The original envelope containing metadata (user ID, timestamp, attempt count).
    pub envelope: Envelope,
    msg: Message,
}

impl std::fmt::Debug for ReceivedCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceivedCommand")
            .field("command", &self.command)
            .field("envelope", &self.envelope)
            .finish_non_exhaustive()
    }
}

impl ReceivedCommand {
    /// Acknowledge processing of this command.
    pub async fn ack(self) -> Result<(), EventBusError> {
        self.msg.ack().await
    }
}

/// A response received from the bus, paired with its envelope and ack handle.
///
/// Call [`ack()`](Self::ack) after processing to prevent redelivery.
pub struct ReceivedResponse {
    /// The deserialized response payload.
    pub response: Response,
    /// The original envelope containing metadata (user ID, timestamp, attempt count).
    pub envelope: Envelope,
    msg: Message,
}

impl std::fmt::Debug for ReceivedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceivedResponse")
            .field("response", &self.response)
            .field("envelope", &self.envelope)
            .finish_non_exhaustive()
    }
}

impl ReceivedResponse {
    /// Acknowledge processing of this response.
    pub async fn ack(self) -> Result<(), EventBusError> {
        self.msg.ack().await
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for constructing an [`EventBus`].
///
/// Requires both a [`Transport`] and a user ID before building.
pub struct EventBusBuilder<T: Transport> {
    transport: Option<T>,
    user_id: Option<Uuid>,
    /// Which subject to subscribe to. Defaults to `agent.requests.<user_id>`.
    subscribe_subject: Option<String>,
}

impl<T: Transport> std::fmt::Debug for EventBusBuilder<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBusBuilder")
            .field("user_id", &self.user_id)
            .field("subscribe_subject", &self.subscribe_subject)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> EventBusBuilder<T> {
    fn new() -> Self {
        Self {
            transport: None,
            user_id: None,
            subscribe_subject: None,
        }
    }

    /// Set the transport implementation (required).
    pub fn transport(mut self, transport: T) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Set the user ID this bus is scoped to (required).
    ///
    /// All routing subjects are user-scoped, so this determines which
    /// message stream the bus subscribes to and publishes on.
    pub fn user_id(mut self, user_id: Uuid) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Override the subject to subscribe to.
    ///
    /// By default, the bus subscribes to `agent.requests.<user_id>` (for
    /// harnesses receiving commands) or `agent.responses.<user_id>` (for
    /// clients receiving responses). Use this to pick the direction.
    pub fn subscribe_subject(mut self, subject: impl Into<String>) -> Self {
        self.subscribe_subject = Some(subject.into());
        self
    }

    /// Build the event bus, establishing the transport subscription.
    ///
    /// Returns [`EventBusError::Builder`] if `transport` or `user_id` were
    /// not set, or a transport error if the subscription fails.
    pub async fn build(self) -> Result<EventBus<T>, EventBusError> {
        let transport = self
            .transport
            .ok_or_else(|| EventBusError::Builder("transport is required".into()))?;
        let user_id = self
            .user_id
            .ok_or_else(|| EventBusError::Builder("user_id is required".into()))?;

        let subject = self
            .subscribe_subject
            .unwrap_or_else(|| nenjo_events::requests_subject_all(user_id));

        let rx = transport.subscribe(&subject).await?;

        Ok(EventBus {
            transport,
            user_id,
            rx,
        })
    }
}
