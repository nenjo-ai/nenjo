//! The typed event bus built on top of a [`Transport`].

use std::sync::Arc;

use nenjo_events::{Command, Envelope, Response, StreamEvent};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};
use uuid::Uuid;

use crate::codec::{EventCodec, NoopEventCodec};
use crate::error::EventBusError;
use crate::transport::{Message, Transport};

/// Typed event bus for sending commands and receiving responses.
///
/// Built via [`EventBusBuilder`]. The bus owns its transport and manages
/// the subscription lifecycle.
pub struct EventBus<T: Transport> {
    transport: T,
    user_id: Uuid,
    codec: Arc<dyn EventCodec>,
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

    /// Access the underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Send a command to the harness.
    ///
    /// The command is wrapped in an [`Envelope`] and published to
    /// `requests.<capability>` (local subject, mapped to PLATFORM via account import).
    pub async fn send_command(&self, command: Command) -> Result<(), EventBusError> {
        let capability = command.capability();
        let Some(command) = self
            .codec
            .encode_command(command)
            .await
            .map_err(|error| EventBusError::Codec(error.to_string()))?
        else {
            trace!(user_id = %self.user_id, %capability, "command dropped by codec");
            return Ok(());
        };
        let payload = serde_json::to_value(&command)?;
        let envelope = Envelope::new(self.user_id, payload);
        let bytes = serde_json::to_vec(&envelope)?;
        let subject = nenjo_events::requests_subject(self.user_id, capability);

        self.transport.publish(&subject, &bytes).await?;
        debug!(
            user_id = %self.user_id,
            %capability,
            encoded = %summarize_encoded_command(&command),
            "event bus published encoded command"
        );
        Ok(())
    }

    /// Send a response back to the backend.
    ///
    /// The response is wrapped in an [`Envelope`] and published to
    /// `responses` (local subject, mapped to PLATFORM via account import).
    pub async fn send_response(&self, response: Response) -> Result<(), EventBusError> {
        let response_label = response.to_string();
        let Some(response) = self
            .codec
            .encode_response(response)
            .await
            .map_err(|error| EventBusError::Codec(error.to_string()))?
        else {
            trace!(user_id = %self.user_id, response = %response_label, "response dropped by codec");
            return Ok(());
        };
        let payload = serde_json::to_value(&response)?;
        let envelope = Envelope::new(self.user_id, payload);
        let bytes = serde_json::to_vec(&envelope)?;
        let subject = nenjo_events::response_subject(self.user_id, &response);

        self.transport.publish(&subject, &bytes).await?;
        match response {
            Response::WorkerPong => {
                trace!(user_id = %self.user_id, response = "worker.pong", "event bus published encoded response");
            }
            Response::WorkerHeartbeat { .. } => {
                trace!(
                    user_id = %self.user_id,
                    encoded = %summarize_encoded_response(&response),
                    "event bus published encoded response"
                );
            }
            response => {
                debug!(
                    user_id = %self.user_id,
                    encoded = %summarize_encoded_response(&response),
                    "event bus published encoded response"
                );
            }
        }
        Ok(())
    }

    /// Receive the next command from the bus.
    ///
    /// Returns `None` when the transport stream ends. The returned
    /// [`ReceivedCommand`] must be acknowledged after processing.
    pub async fn recv_command(&mut self) -> Result<Option<ReceivedCommand>, EventBusError> {
        loop {
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

            let command: Command =
                serde_json::from_value(envelope.payload.clone()).map_err(|e| {
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

            let Some(command) = self
                .codec
                .decode_command(command)
                .await
                .map_err(|error| EventBusError::Codec(error.to_string()))?
            else {
                trace!(user_id = %self.user_id, "received command dropped by codec");
                msg.ack().await?;
                continue;
            };

            return Ok(Some(ReceivedCommand {
                command,
                envelope,
                msg,
            }));
        }
    }

    /// Receive the next response from the bus.
    ///
    /// Returns `None` when the transport stream ends. The returned
    /// [`ReceivedResponse`] must be acknowledged after processing.
    pub async fn recv_response(&mut self) -> Result<Option<ReceivedResponse>, EventBusError> {
        loop {
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

            let response: Response =
                serde_json::from_value(envelope.payload.clone()).map_err(|e| {
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

            let Some(response) = self
                .codec
                .decode_response(response)
                .await
                .map_err(|error| EventBusError::Codec(error.to_string()))?
            else {
                trace!(user_id = %self.user_id, "received response dropped by codec");
                msg.ack().await?;
                continue;
            };

            return Ok(Some(ReceivedResponse {
                response,
                envelope,
                msg,
            }));
        }
    }
}

fn summarize_encoded_command(command: &Command) -> String {
    match command {
        Command::ChatMessage {
            encrypted_content,
            session_id,
            agent_id,
            project_id,
            ..
        } => format!(
            "chat.message(encrypted={}, session_id={}, agent_id={}, project_id={})",
            encrypted_content.is_some(),
            session_id,
            agent_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            project_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        other => other.to_string(),
    }
}

fn summarize_encoded_response(response: &Response) -> String {
    match response {
        Response::AgentResponse {
            session_id,
            payload,
        } => format!(
            "agent_response(session_id={}, {})",
            session_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            summarize_encoded_stream_event(payload)
        ),
        Response::TaskStepEvent {
            event_type,
            step_name,
            payload,
            encrypted_payload,
            ..
        } => format!(
            "task.step_event(event_type={event_type}, step={}, payload={}, encrypted={})",
            step_name,
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        other => other.to_string(),
    }
}

fn summarize_encoded_stream_event(event: &StreamEvent) -> String {
    match event {
        StreamEvent::Done {
            payload,
            encrypted_payload,
            project_id,
            agent_id,
            session_id,
        } => format!(
            "done(payload={}, encrypted={}, project_id={}, agent_id={}, session_id={})",
            payload.is_some(),
            encrypted_payload.is_some(),
            project_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            agent_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            session_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        StreamEvent::ToolCalls {
            payload,
            encrypted_payload,
            ..
        } => format!(
            "tool_calls(payload={}, encrypted={})",
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        StreamEvent::ToolCompleted {
            payload,
            encrypted_payload,
            success,
            ..
        } => format!(
            "tool_completed(success={success}, payload={}, encrypted={})",
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        StreamEvent::AbilityActivated {
            payload,
            encrypted_payload,
            ..
        } => format!(
            "ability_activated(payload={}, encrypted={})",
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        StreamEvent::AbilityCompleted {
            payload,
            encrypted_payload,
            success,
            ..
        } => format!(
            "ability_completed(success={success}, payload={}, encrypted={})",
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        StreamEvent::Error {
            payload,
            encrypted_payload,
            ..
        } => format!(
            "error(payload={}, encrypted={})",
            payload.is_some(),
            encrypted_payload.is_some()
        ),
        other => other.to_string(),
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
    codec: Arc<dyn EventCodec>,
    /// Which subject to subscribe to. Defaults to `requests.*`.
    subscribe_subject: Option<String>,
}

impl<T: Transport> std::fmt::Debug for EventBusBuilder<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBusBuilder")
            .field("user_id", &self.user_id)
            .field("has_codec", &true)
            .field("subscribe_subject", &self.subscribe_subject)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> EventBusBuilder<T> {
    fn new() -> Self {
        Self {
            transport: None,
            user_id: None,
            codec: Arc::new(NoopEventCodec),
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

    /// Set the codec used to transform commands and responses at the bus boundary.
    pub fn with_codec<C>(mut self, codec: C) -> Self
    where
        C: EventCodec,
    {
        self.codec = Arc::new(codec);
        self
    }

    /// Override the subject to subscribe to.
    ///
    /// By default, the bus subscribes to `requests.*` (for harnesses
    /// receiving commands). Use this to override with a specific subject.
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
            codec: self.codec,
            rx,
        })
    }
}
