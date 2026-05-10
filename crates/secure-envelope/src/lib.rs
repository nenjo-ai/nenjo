//! Secure envelope layer sitting between raw event transport and the worker harness.
//!
//! This crate owns:
//! - the secure wrapper over `nenjo-eventbus`
//! - the envelope codec trait and decode-failure model
//! - shared encrypted payload helpers
//! - the default secure envelope codec used by the worker

mod codec;
mod content;

use std::error::Error as StdError;
use std::sync::Arc;

use async_trait::async_trait;
use nenjo_eventbus::{EventBus, EventBusError, ReceivedEnvelope, Transport};
use nenjo_events::{Command, Envelope, Response};
use tracing::{trace, warn};
use uuid::Uuid;

pub use codec::SecureEnvelopeCodec;
pub use content::{decrypt_text, encrypt_text, encrypt_text_for_scope};

pub type CodecError = Box<dyn StdError + Send + Sync + 'static>;
pub type CodecResult<T> = Result<Option<T>, CodecError>;

/// Context provided to envelope codecs for actor-scoped decrypt/encrypt decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecContext {
    /// Authenticated actor for the inbound or outbound envelope being transformed.
    pub actor_user_id: Uuid,
}

impl CodecContext {
    /// Build a codec context for the given actor.
    pub fn for_actor(actor_user_id: Uuid) -> Self {
        Self { actor_user_id }
    }
}

/// Structured decode failure that is safe to surface back to the initiating actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodingError {
    /// Stable machine-readable error code.
    pub code: &'static str,
    /// Sanitized message safe to return to the initiating actor.
    pub message: String,
    /// Session associated with the failed command when available.
    pub session_id: Option<Uuid>,
    /// Project associated with the failed command when available.
    pub project_id: Option<Uuid>,
    /// Agent associated with the failed command when available.
    pub agent_id: Option<Uuid>,
}

/// Result of decoding an inbound command envelope.
#[derive(Debug, Clone)]
pub enum DecodeCommandResult {
    /// Successfully decoded command ready for harness routing.
    Command(Box<Command>),
    /// Envelope should be acknowledged and silently discarded.
    Drop,
    /// Envelope failed in a user-visible way and should become a terminal client error.
    ClientError(DecodingError),
}

/// Trait implemented by secure envelope codecs that transform command/response payloads.
#[async_trait]
pub trait EnvelopeCodec: Send + Sync + 'static {
    /// Transform an outbound command before it is wrapped into an envelope.
    async fn encode_command(&self, command: Command) -> CodecResult<Command>;
    /// Decode an inbound command using the supplied actor context.
    async fn decode_command(
        &self,
        ctx: &CodecContext,
        command: Command,
    ) -> Result<DecodeCommandResult, CodecError>;
    /// Transform an outbound response before it is wrapped into an envelope.
    async fn encode_response(
        &self,
        ctx: &CodecContext,
        response: Response,
    ) -> CodecResult<Response>;
    /// Decode an inbound response envelope payload.
    async fn decode_response(&self, response: Response) -> CodecResult<Response>;
}

/// Secure wrapper over the raw event bus that applies an [`EnvelopeCodec`].
pub struct SecureEnvelopeBus<T: Transport> {
    raw: EventBus<T>,
    codec: Arc<dyn EnvelopeCodec>,
}

impl<T: Transport> std::fmt::Debug for SecureEnvelopeBus<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecureEnvelopeBus").finish_non_exhaustive()
    }
}

impl<T: Transport> SecureEnvelopeBus<T> {
    /// Wrap a raw [`EventBus`] with the provided secure envelope codec.
    pub fn new<C>(raw: EventBus<T>, codec: C) -> Self
    where
        C: EnvelopeCodec,
    {
        Self {
            raw,
            codec: Arc::new(codec),
        }
    }

    /// Access the underlying raw transport for worker metadata such as `worker_id`.
    pub fn transport(&self) -> &T {
        self.raw.transport()
    }

    /// Encode and send a command envelope on behalf of the given actor.
    pub async fn send_command_for(
        &self,
        actor_user_id: Uuid,
        command: Command,
    ) -> Result<(), EventBusError> {
        let capability = command.capability();
        let Some(command) = self
            .codec
            .encode_command(command)
            .await
            .map_err(|error| EventBusError::Codec(error.to_string()))?
        else {
            trace!(actor_user_id = %actor_user_id, %capability, "command dropped by secure envelope codec");
            return Ok(());
        };

        let payload = serde_json::to_value(&command)?;
        let envelope = Envelope::new(actor_user_id, payload);
        let subject = nenjo_events::requests_subject(capability);
        self.raw.send_envelope(&subject, &envelope).await
    }

    /// Encode and send a response envelope on behalf of the given actor.
    pub async fn send_response_for(
        &self,
        org_id: Uuid,
        actor_user_id: Uuid,
        response: Response,
    ) -> Result<(), EventBusError> {
        let response_label = response.to_string();
        let ctx = CodecContext::for_actor(actor_user_id);
        let Some(response) = self
            .codec
            .encode_response(&ctx, response)
            .await
            .map_err(|error| EventBusError::Codec(error.to_string()))?
        else {
            trace!(actor_user_id = %actor_user_id, response = %response_label, "response dropped by secure envelope codec");
            return Ok(());
        };

        let payload = serde_json::to_value(&response)?;
        let envelope = Envelope::new(actor_user_id, payload);
        let subject = nenjo_events::response_subject(org_id, actor_user_id, &response);
        self.raw.send_envelope(&subject, &envelope).await
    }

    /// Encode and send a worker-level system response routed by org.
    ///
    /// This is intended for cleartext responses such as worker presence updates.
    /// Actor-encrypted responses should use [`Self::send_response_for`].
    pub async fn send_system_response(
        &self,
        org_id: Uuid,
        response: Response,
    ) -> Result<(), EventBusError> {
        let response_label = response.to_string();
        let ctx = CodecContext::for_actor(org_id);
        let Some(response) = self
            .codec
            .encode_response(&ctx, response)
            .await
            .map_err(|error| EventBusError::Codec(error.to_string()))?
        else {
            trace!(response = %response_label, "system response dropped by secure envelope codec");
            return Ok(());
        };

        let payload = serde_json::to_value(&response)?;
        let envelope = Envelope::new(org_id, payload);
        let subject = nenjo_events::responses_subject(org_id);
        self.raw.send_envelope(&subject, &envelope).await
    }

    /// Receive, decode, and classify the next inbound command envelope.
    ///
    /// This either yields a decoded command or a structured client-visible
    /// decode failure. Both must be acknowledged by the caller after handling.
    pub async fn recv_command(&mut self) -> Result<Option<ReceivedInput>, EventBusError> {
        loop {
            let received = match self.raw.recv_envelope().await? {
                Some(received) => received,
                None => return Ok(None),
            };

            let envelope = received.envelope.clone();
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

            let ctx = CodecContext::for_actor(envelope.user_id);
            match self
                .codec
                .decode_command(&ctx, command)
                .await
                .map_err(|error| EventBusError::Codec(error.to_string()))?
            {
                DecodeCommandResult::Command(command) => {
                    return Ok(Some(ReceivedInput::Command(Box::new(ReceivedCommand {
                        command: *command,
                        envelope,
                        received,
                    }))));
                }
                DecodeCommandResult::Drop => {
                    trace!(actor_user_id = %envelope.user_id, "received command dropped by secure envelope codec");
                    received.ack().await?;
                    continue;
                }
                DecodeCommandResult::ClientError(failure) => {
                    return Ok(Some(ReceivedInput::DecodeFailure(Box::new(
                        ReceivedDecodeFailure {
                            failure,
                            envelope,
                            received,
                        },
                    ))));
                }
            }
        }
    }
}

/// Decoded command plus its original envelope/ack handle.
pub struct ReceivedCommand {
    /// Decoded command ready for harness routing.
    pub command: Command,
    /// Original transport envelope carrying actor metadata.
    pub envelope: Envelope,
    received: ReceivedEnvelope,
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
    pub fn source(&self) -> Option<&nenjo_eventbus::MessageSource> {
        self.received.msg.source.as_ref()
    }

    /// Acknowledge the underlying transport envelope.
    pub async fn ack(self) -> Result<(), EventBusError> {
        self.received.ack().await
    }
}

/// Inbound secure-envelope item delivered to the harness.
pub enum ReceivedInput {
    Command(Box<ReceivedCommand>),
    DecodeFailure(Box<ReceivedDecodeFailure>),
}

/// User-safe decode failure plus its original envelope/ack handle.
pub struct ReceivedDecodeFailure {
    /// Structured failure details suitable for surfacing back to the actor.
    pub failure: DecodingError,
    /// Original transport envelope carrying actor metadata.
    pub envelope: Envelope,
    received: ReceivedEnvelope,
}

impl ReceivedDecodeFailure {
    pub fn source(&self) -> Option<&nenjo_eventbus::MessageSource> {
        self.received.msg.source.as_ref()
    }

    /// Acknowledge the underlying transport envelope.
    pub async fn ack(self) -> Result<(), EventBusError> {
        self.received.ack().await
    }
}
