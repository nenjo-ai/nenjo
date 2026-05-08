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
use crate::transport::{AckHandle, Message, MessageSource, Transport};

// ---------------------------------------------------------------------------
// Configuration defaults
// ---------------------------------------------------------------------------

const DEFAULT_URL: &str = "nats://localhost:4222";
const DEFAULT_STREAM_NAME: &str = "AGENT_WORK_REQUESTS";
const DEFAULT_BROADCAST_STREAM_NAME: &str = "AGENT_BROADCAST_REQUESTS";
const DEFAULT_STREAM_SUBJECTS: &[&str] = &["work_requests.*", "worker_requests.*.*"];
const DEFAULT_BROADCAST_STREAM_SUBJECTS: &[&str] = &["broadcast_requests.*"];
const DEFAULT_WORK_CONSUMER_SUBJECT: &str = "work_requests.*";
const DEFAULT_BROADCAST_CONSUMER_SUBJECTS: &[&str] = &[
    "broadcast_requests.manifest",
    "broadcast_requests.repo",
    "broadcast_requests.ping",
];
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
    broadcast_stream_name: Option<String>,
    manage_streams: bool,
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
        let broadcast_stream_name = self.broadcast_stream_name.clone();
        let max_deliver = self.max_deliver;
        let ack_wait = self.ack_wait;
        let buffer = self.message_buffer;
        let worker_id = self.worker_id;
        let jetstream = self.jetstream.clone();
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(buffer);
            if subject == "work_requests.>" {
                spawn_consumer(ConsumerSpawnSpec {
                    jetstream: jetstream.clone(),
                    stream_name: stream_name.clone(),
                    subject: DEFAULT_WORK_CONSUMER_SUBJECT.to_string(),
                    consumer_name: "worker-requests".to_string(),
                    require_stream_handle: self.manage_streams,
                    max_deliver,
                    ack_wait,
                    tx: tx.clone(),
                })
                .await?;

                if let Some(broadcast_stream_name) = broadcast_stream_name {
                    for broadcast_subject in DEFAULT_BROADCAST_CONSUMER_SUBJECTS {
                        spawn_consumer(ConsumerSpawnSpec {
                            jetstream: jetstream.clone(),
                            stream_name: broadcast_stream_name.clone(),
                            subject: (*broadcast_subject).to_string(),
                            consumer_name: format!(
                                "worker-broadcast-requests-{worker_id}-{}",
                                broadcast_subject.replace('.', "-")
                            ),
                            require_stream_handle: self.manage_streams,
                            max_deliver,
                            ack_wait,
                            tx: tx.clone(),
                        })
                        .await?;
                    }
                }
            } else {
                let work_queue_consumer = subject.replace('.', "-");
                spawn_consumer(ConsumerSpawnSpec {
                    jetstream,
                    stream_name,
                    subject,
                    consumer_name: work_queue_consumer,
                    require_stream_handle: self.manage_streams,
                    max_deliver,
                    ack_wait,
                    tx,
                })
                .await?;
            }

            Ok(rx)
        })
    }
}

struct ConsumerSpawnSpec {
    jetstream: jetstream::Context,
    stream_name: String,
    subject: String,
    consumer_name: String,
    require_stream_handle: bool,
    max_deliver: i64,
    ack_wait: Duration,
    tx: mpsc::Sender<Message>,
}

async fn spawn_consumer(spec: ConsumerSpawnSpec) -> Result<(), EventBusError> {
    let ConsumerSpawnSpec {
        jetstream,
        stream_name,
        subject,
        consumer_name,
        require_stream_handle,
        max_deliver,
        ack_wait,
        tx,
    } = spec;

    let consumer_config = pull::Config {
        durable_name: Some(consumer_name.clone()),
        filter_subject: subject.clone(),
        max_deliver,
        ack_wait,
        ..Default::default()
    };

    let consumer = if require_stream_handle {
        let stream = jetstream
            .get_stream(&stream_name)
            .await
            .map_err(|e| EventBusError::Transport(format!("get stream failed: {e}")))?;

        stream
            .get_or_create_consumer(&consumer_name, consumer_config.clone())
            .await
            .map_err(|e| {
                EventBusError::Transport(format!("create consumer '{consumer_name}' failed: {e}"))
            })?
    } else {
        jetstream
            .create_consumer_on_stream(consumer_config.clone(), stream_name.clone())
            .await
            .map_err(|e| {
                EventBusError::Transport(format!("create consumer '{consumer_name}' failed: {e}"))
            })?
    };

    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| EventBusError::Transport(format!("start consumer failed: {e}")))?;

    tokio::spawn(async move {
        use futures_util::StreamExt;

        loop {
            while let Some(msg_result) = messages.next().await {
                match msg_result {
                    Ok(msg) => {
                        let delivered_subject = msg.message.subject.to_string();
                        let payload = msg.payload.to_vec();
                        let ack_handle = NatsAckHandle(Some(msg));
                        let message =
                            Message::new(payload, ack_handle).with_source(MessageSource {
                                stream: Some(stream_name.clone()),
                                consumer: Some(consumer_name.clone()),
                                filter_subject: Some(subject.clone()),
                                subject: Some(delivered_subject),
                            });
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

            warn!(
                stream = %stream_name,
                subject = %subject,
                "NATS consumer stream ended, attempting to reconnect"
            );

            let mut delay = Duration::from_secs(1);
            let max_delay = Duration::from_secs(30);

            loop {
                tokio::time::sleep(delay).await;

                let reconnected = async {
                    let c = if require_stream_handle {
                        let stream = jetstream
                            .get_stream(&stream_name)
                            .await
                            .map_err(|e| e.to_string())?;
                        stream
                            .get_or_create_consumer(&consumer_name, consumer_config.clone())
                            .await
                            .map_err(|e| e.to_string())?
                    } else {
                        jetstream
                            .create_consumer_on_stream(consumer_config.clone(), stream_name.clone())
                            .await
                            .map_err(|e| e.to_string())?
                    };
                    c.messages().await.map_err(|e| e.to_string())
                }
                .await;

                match reconnected {
                    Ok(new_messages) => {
                        info!(
                            stream = %stream_name,
                            subject = %subject,
                            "NATS consumer reconnected"
                        );
                        messages = new_messages;
                        break;
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            retry_in = ?delay,
                            stream = %stream_name,
                            subject = %subject,
                            "Failed to reconnect NATS consumer, retrying"
                        );
                        delay = std::cmp::min(delay.saturating_mul(2), max_delay);
                    }
                }
            }
        }
    });

    Ok(())
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
    urls: Vec<String>,
    token: Option<String>,
    stream_name: String,
    stream_subjects: Vec<String>,
    broadcast_stream_name: Option<String>,
    broadcast_stream_subjects: Vec<String>,
    max_age: Duration,
    max_deliver: i64,
    ack_wait: Duration,
    message_buffer: usize,
    worker_id: uuid::Uuid,
    manage_streams: bool,
}

impl NatsTransport {
    /// Create a builder for configuring a NATS transport.
    pub fn builder() -> NatsTransportBuilder {
        NatsTransportBuilder {
            urls: vec![DEFAULT_URL.to_string()],
            token: None,
            stream_name: DEFAULT_STREAM_NAME.to_string(),
            stream_subjects: DEFAULT_STREAM_SUBJECTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            broadcast_stream_name: Some(DEFAULT_BROADCAST_STREAM_NAME.to_string()),
            broadcast_stream_subjects: DEFAULT_BROADCAST_STREAM_SUBJECTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_age: Duration::from_secs(DEFAULT_MAX_AGE_SECS),
            max_deliver: DEFAULT_MAX_DELIVER,
            ack_wait: Duration::from_secs(DEFAULT_ACK_WAIT_SECS),
            message_buffer: DEFAULT_MESSAGE_BUFFER,
            worker_id: uuid::Uuid::new_v4(),
            manage_streams: true,
        }
    }

    /// The unique instance ID for this worker.
    pub fn worker_id(&self) -> uuid::Uuid {
        self.worker_id
    }
}

impl NatsTransportBuilder {
    /// Set the NATS server URL pool used for initial connection and reconnects.
    pub fn urls(mut self, urls: Vec<String>) -> Self {
        self.urls = urls;
        self
    }

    /// Set the auth token (used for token-based or auth-callout authentication).
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Override the JetStream stream name (default: `AGENT_WORK_REQUESTS`).
    pub fn stream_name(mut self, name: impl Into<String>) -> Self {
        self.stream_name = name.into();
        self
    }

    /// Override the JetStream stream subjects (default: `["work_requests.*", "worker_requests.*.*"]`).
    pub fn stream_subjects(mut self, subjects: Vec<String>) -> Self {
        self.stream_subjects = subjects;
        self
    }

    /// Override the broadcast JetStream stream name.
    pub fn broadcast_stream_name(mut self, name: impl Into<String>) -> Self {
        self.broadcast_stream_name = Some(name.into());
        self
    }

    /// Disable the broadcast request stream subscription.
    pub fn disable_broadcast_stream(mut self) -> Self {
        self.broadcast_stream_name = None;
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

    /// Control whether this client creates/verifies JetStream streams.
    ///
    /// Workers normally keep this enabled so the org account has local streams
    /// over the remapped request subjects. Disable it only when another process
    /// has already created compatible streams in the same account.
    pub fn manage_streams(mut self, manage: bool) -> Self {
        self.manage_streams = manage;
        self
    }

    /// Connect to NATS and optionally create/verify the JetStream stream.
    pub async fn build(self) -> Result<NatsTransport, EventBusError> {
        if self.urls.is_empty() {
            return Err(EventBusError::Builder(
                "at least one NATS URL is required".into(),
            ));
        }

        // Enforce TLS for non-local connections.
        for url in &self.urls {
            let is_local =
                url.contains("localhost") || url.contains("127.0.0.1") || url.contains("[::1]");

            if !is_local && !url.starts_with("tls://") {
                return Err(EventBusError::Builder(
                    "non-local NATS connections require tls:// scheme".into(),
                ));
            }
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
                        debug!("NATS event: connected");
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
            .connect(self.urls.clone())
            .await
            .map_err(|e| EventBusError::Transport(format!("NATS connect failed: {e}")))?;

        debug!(urls = ?self.urls, "Connected to NATS server");

        let jetstream = jetstream::new(client);

        if self.manage_streams {
            // Ensure the work-queue stream exists.
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

            debug!(stream = %self.stream_name, "JetStream stream ready");

            if let Some(ref broadcast_stream_name) = self.broadcast_stream_name {
                let broadcast_stream_config = stream::Config {
                    name: broadcast_stream_name.clone(),
                    subjects: self.broadcast_stream_subjects.clone(),
                    retention: stream::RetentionPolicy::Interest,
                    max_age: self.max_age,
                    storage: stream::StorageType::Memory,
                    ..Default::default()
                };

                jetstream
                    .get_or_create_stream(broadcast_stream_config)
                    .await
                    .map_err(|e| {
                        EventBusError::Transport(format!("broadcast stream setup failed: {e}"))
                    })?;

                debug!(stream = %broadcast_stream_name, "Broadcast JetStream stream ready");
            }
        } else {
            debug!("Skipping JetStream stream setup; using platform-managed streams");
        }

        Ok(NatsTransport {
            jetstream,
            stream_name: self.stream_name,
            broadcast_stream_name: self.broadcast_stream_name,
            manage_streams: self.manage_streams,
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
        assert_eq!(builder.urls, vec!["nats://localhost:4222"]);
        assert_eq!(builder.stream_name, "AGENT_WORK_REQUESTS");
        assert!(builder.manage_streams);
        assert_eq!(builder.max_deliver, 3);
        assert_ne!(builder.worker_id, uuid::Uuid::nil());
    }

    #[test]
    fn builder_overrides() {
        let builder = NatsTransport::builder()
            .urls(vec!["tls://nats.prod.example.com".to_string()])
            .token("secret")
            .stream_name("CUSTOM_STREAM")
            .manage_streams(false)
            .max_deliver(5)
            .ack_wait(Duration::from_secs(30));

        assert_eq!(builder.urls, vec!["tls://nats.prod.example.com"]);
        assert_eq!(builder.token.as_deref(), Some("secret"));
        assert_eq!(builder.stream_name, "CUSTOM_STREAM");
        assert!(!builder.manage_streams);
        assert_eq!(builder.max_deliver, 5);
        assert_eq!(builder.ack_wait, Duration::from_secs(30));
    }

    #[test]
    fn builder_accepts_url_pool() {
        let builder = NatsTransport::builder().urls(vec![
            "tls://nats-a.example.com:4222".to_string(),
            "tls://nats-b.example.com:4222".to_string(),
        ]);

        assert_eq!(builder.urls.len(), 2);
        assert_eq!(builder.urls[0], "tls://nats-a.example.com:4222");
    }
}
