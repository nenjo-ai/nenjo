//! NATS JetStream transport implementation.
//!
//! Requires the `nats` feature flag:
//!
//! ```toml
//! event-bus = { path = "../event-bus", features = ["nats"] }
//! ```

use std::time::Duration;

use async_nats::jetstream::{self, consumer::pull, stream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::error::EventBusError;
use crate::transport::{AckHandle, Message, Transport};

// ---------------------------------------------------------------------------
// Configuration defaults
// ---------------------------------------------------------------------------

const DEFAULT_URL: &str = "nats://localhost:4222";
const DEFAULT_STREAM_NAME: &str = "AGENT_EVENTS";
const DEFAULT_STREAM_SUBJECTS: &[&str] = &["requests.>", "responses.>"];
const DEFAULT_MAX_AGE_SECS: u64 = 86_400; // 24 hours
const DEFAULT_MAX_DELIVER: i64 = 3;
const DEFAULT_ACK_WAIT_SECS: u64 = 10;
const DEFAULT_MESSAGE_BUFFER: usize = 256;

// ---------------------------------------------------------------------------
// NatsTransport
// ---------------------------------------------------------------------------

/// Production transport backed by NATS JetStream.
///
/// Provides at-least-once delivery with pull consumers, durable subscriptions,
/// and two-stage publish acknowledgment.
pub struct NatsTransport {
    jetstream: jetstream::Context,
    stream_name: String,
    max_deliver: i64,
    ack_wait: Duration,
    message_buffer: usize,
    worker_id: uuid::Uuid,
}

impl Transport for NatsTransport {
    fn worker_id(&self) -> uuid::Uuid {
        self.worker_id
    }

    fn publish(
        &self,
        subject: &str,
        payload: &[u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EventBusError>> + Send + '_>>
    {
        let subject = subject.to_string();
        let payload = bytes::Bytes::from(payload.to_vec());
        Box::pin(async move {
            let ack_future = self
                .jetstream
                .publish(subject, payload)
                .await
                .map_err(|e| EventBusError::Transport(format!("publish failed: {e}")))?;

            ack_future
                .await
                .map_err(|e| EventBusError::Transport(format!("publish ack failed: {e}")))?;

            Ok(())
        })
    }

    fn subscribe(
        &self,
        subject: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<mpsc::Receiver<Message>, EventBusError>>
                + Send
                + '_,
        >,
    > {
        let subject = subject.to_string();
        let stream_name = self.stream_name.clone();
        let max_deliver = self.max_deliver;
        let ack_wait = self.ack_wait;
        let buffer = self.message_buffer;
        Box::pin(async move {
            // Consumer naming convention (per-user account subjects):
            //   worker-requests  (shared by all workers in this user's account —
            //                     NATS round-robins messages between active
            //                     pull subscribers on a WorkQueue stream)
            //   worker-responses (for response consumers)
            //
            // Since each user has their own NATS account, the consumer name
            // doesn't need user_id — the account provides the namespace.
            let consumer_name = if subject.starts_with("requests.") {
                "worker-requests".to_string()
            } else if subject == "responses" || subject.starts_with("responses.") {
                "worker-responses".to_string()
            } else {
                subject.replace('.', "-")
            };

            let stream = self
                .jetstream
                .get_stream(&stream_name)
                .await
                .map_err(|e| EventBusError::Transport(format!("get stream failed: {e}")))?;

            let consumer_config = pull::Config {
                durable_name: Some(consumer_name.clone()),
                filter_subject: subject.clone(),
                max_deliver,
                ack_wait,
                ..Default::default()
            };

            let consumer = stream
                .get_or_create_consumer(&consumer_name, consumer_config.clone())
                .await
                .map_err(|e| {
                    EventBusError::Transport(format!(
                        "create consumer '{consumer_name}' failed: {e}"
                    ))
                })?;

            let mut messages = consumer
                .messages()
                .await
                .map_err(|e| EventBusError::Transport(format!("start consumer failed: {e}")))?;

            let (tx, rx) = mpsc::channel(buffer);

            tokio::spawn(async move {
                use futures_util::StreamExt;

                loop {
                    while let Some(msg_result) = messages.next().await {
                        match msg_result {
                            Ok(msg) => {
                                let payload = msg.payload.to_vec();
                                let ack_handle = NatsAckHandle(Some(msg));
                                let message = Message::new(payload, ack_handle);
                                if tx.send(message).await.is_err() {
                                    debug!("subscriber channel closed, stopping consumer");
                                    return;
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "error receiving NATS message");
                            }
                        }
                    }

                    // Consumer stream ended — attempt to reconnect with backoff.
                    warn!(subject = %subject, "NATS consumer stream ended, attempting to reconnect");

                    let mut delay = Duration::from_secs(1);
                    let max_delay = Duration::from_secs(30);

                    loop {
                        tokio::time::sleep(delay).await;

                        // Re-fetch the consumer and restart its message stream.
                        let reconnected = async {
                            let c = stream
                                .get_or_create_consumer(&consumer_name, consumer_config.clone())
                                .await
                                .map_err(|e| e.to_string())?;
                            c.messages().await.map_err(|e| e.to_string())
                        }
                        .await;

                        match reconnected {
                            Ok(new_messages) => {
                                info!(subject = %subject, "NATS consumer reconnected");
                                messages = new_messages;
                                break;
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    retry_in = ?delay,
                                    subject = %subject,
                                    "Failed to reconnect NATS consumer, retrying"
                                );
                                delay = std::cmp::min(delay.saturating_mul(2), max_delay);
                            }
                        }
                    }
                }
            });

            Ok(rx)
        })
    }
}

// ---------------------------------------------------------------------------
// Ack handle
// ---------------------------------------------------------------------------

struct NatsAckHandle(Option<jetstream::Message>);

impl AckHandle for NatsAckHandle {
    fn ack(
        mut self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EventBusError>> + Send>>
    {
        Box::pin(async move {
            if let Some(msg) = self.0.take() {
                msg.ack()
                    .await
                    .map_err(|e| EventBusError::Transport(format!("ack failed: {e}")))?;
            }
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`NatsTransport`].
///
/// # Example
///
/// ```ignore
/// let transport = NatsTransport::builder()
///     .url("tls://nats.example.com:4222")
///     .token("my-api-key")
///     .build()
///     .await?;
/// ```
pub struct NatsTransportBuilder {
    url: String,
    token: Option<String>,
    stream_name: String,
    stream_subjects: Vec<String>,
    max_age: Duration,
    max_deliver: i64,
    ack_wait: Duration,
    message_buffer: usize,
    worker_id: uuid::Uuid,
}

impl NatsTransport {
    /// Create a builder for configuring a NATS transport.
    pub fn builder() -> NatsTransportBuilder {
        NatsTransportBuilder {
            url: DEFAULT_URL.to_string(),
            token: None,
            stream_name: DEFAULT_STREAM_NAME.to_string(),
            stream_subjects: DEFAULT_STREAM_SUBJECTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_age: Duration::from_secs(DEFAULT_MAX_AGE_SECS),
            max_deliver: DEFAULT_MAX_DELIVER,
            ack_wait: Duration::from_secs(DEFAULT_ACK_WAIT_SECS),
            message_buffer: DEFAULT_MESSAGE_BUFFER,
            worker_id: uuid::Uuid::new_v4(),
        }
    }

    /// The unique instance ID for this worker.
    pub fn worker_id(&self) -> uuid::Uuid {
        self.worker_id
    }
}

impl NatsTransportBuilder {
    /// Set the NATS server URL (e.g., `nats://localhost:4222` or `tls://nats.example.com`).
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Set the auth token (used for token-based or auth-callout authentication).
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Override the JetStream stream name (default: `AGENT_EVENTS`).
    pub fn stream_name(mut self, name: impl Into<String>) -> Self {
        self.stream_name = name.into();
        self
    }

    /// Override the JetStream stream subjects (default: `["agent.>"]`).
    pub fn stream_subjects(mut self, subjects: Vec<String>) -> Self {
        self.stream_subjects = subjects;
        self
    }

    /// Set the maximum age for messages in the stream (default: 24h).
    pub fn max_age(mut self, duration: Duration) -> Self {
        self.max_age = duration;
        self
    }

    /// Set the maximum delivery attempts per message (default: 3).
    pub fn max_deliver(mut self, n: i64) -> Self {
        self.max_deliver = n;
        self
    }

    /// Set the ack wait timeout (default: 10s).
    pub fn ack_wait(mut self, duration: Duration) -> Self {
        self.ack_wait = duration;
        self
    }

    /// Set the mpsc channel buffer size for received messages (default: 256).
    pub fn message_buffer(mut self, size: usize) -> Self {
        self.message_buffer = size;
        self
    }

    /// Override the worker instance ID (default: random UUID).
    ///
    /// Each worker instance needs a unique ID for consumer naming when
    /// multiple workers share a deliver group.
    pub fn worker_id(mut self, id: uuid::Uuid) -> Self {
        self.worker_id = id;
        self
    }

    /// Connect to NATS and create/verify the JetStream stream.
    pub async fn build(self) -> Result<NatsTransport, EventBusError> {
        // Enforce TLS for non-local connections.
        let is_local = self.url.contains("localhost")
            || self.url.contains("127.0.0.1")
            || self.url.contains("[::1]");

        if !is_local && !self.url.starts_with("tls://") {
            return Err(EventBusError::Builder(
                "non-local NATS connections require tls:// scheme".into(),
            ));
        }

        // Build connect options with automatic reconnection.
        let token = self
            .token
            .ok_or_else(|| EventBusError::Builder("NATS token is required".into()))?;
        let opts = async_nats::ConnectOptions::with_token(token);

        let opts = opts
            .retry_on_initial_connect()
            .connection_timeout(Duration::from_secs(10))
            .reconnect_delay_callback(|attempts| {
                // Exponential backoff: 1s, 2s, 4s, 8s, capped at 30s
                let base = Duration::from_secs(1);
                let delay = base.saturating_mul(2u32.saturating_pow(attempts as u32));
                std::cmp::min(delay, Duration::from_secs(30))
            })
            .event_callback(|event| async move {
                match event {
                    async_nats::Event::Connected => {
                        info!("NATS connected");
                    }
                    async_nats::Event::Disconnected => {
                        warn!("NATS disconnected, will attempt reconnection");
                    }
                    async_nats::Event::ServerError(e) => {
                        warn!(error = %e, "NATS server error");
                    }
                    _ => {}
                }
            });

        let client = opts
            .connect(&self.url)
            .await
            .map_err(|e| EventBusError::Transport(format!("NATS connect failed: {e}")))?;

        info!(url = %self.url, "connected to NATS");

        let jetstream = jetstream::new(client);

        // Ensure the stream exists.
        let stream_config = stream::Config {
            name: self.stream_name.clone(),
            subjects: self.stream_subjects,
            retention: stream::RetentionPolicy::WorkQueue,
            max_age: self.max_age,
            storage: stream::StorageType::Memory,
            ..Default::default()
        };

        jetstream
            .get_or_create_stream(stream_config)
            .await
            .map_err(|e| EventBusError::Transport(format!("stream setup failed: {e}")))?;

        info!(stream = %self.stream_name, "JetStream stream ready");

        Ok(NatsTransport {
            jetstream,
            stream_name: self.stream_name,
            max_deliver: self.max_deliver,
            ack_wait: self.ack_wait,
            message_buffer: self.message_buffer,
            worker_id: self.worker_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults() {
        let builder = NatsTransport::builder();
        assert_eq!(builder.url, "nats://localhost:4222");
        assert_eq!(builder.stream_name, "AGENT_EVENTS");
        assert_eq!(builder.max_deliver, 3);
        assert_ne!(builder.worker_id, uuid::Uuid::nil());
    }

    #[test]
    fn builder_overrides() {
        let builder = NatsTransport::builder()
            .url("tls://nats.prod.example.com")
            .token("secret")
            .stream_name("CUSTOM_STREAM")
            .max_deliver(5)
            .ack_wait(Duration::from_secs(30));

        assert_eq!(builder.url, "tls://nats.prod.example.com");
        assert_eq!(builder.token.as_deref(), Some("secret"));
        assert_eq!(builder.stream_name, "CUSTOM_STREAM");
        assert_eq!(builder.max_deliver, 5);
        assert_eq!(builder.ack_wait, Duration::from_secs(30));
    }
}
