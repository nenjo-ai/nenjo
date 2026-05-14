//! NATS JetStream transport implementation.
//!
//! Requires the `nats` feature flag:
//!
//! ```toml
//! event-bus = { path = "../event-bus", features = ["nats"] }
//! ```

use std::time::Duration;

use async_nats::jetstream::{self, consumer::pull, stream};
use nenjo_events::{
    BROADCAST_REQUESTS_STREAM_NAME, BROADCAST_REQUESTS_STREAM_SUBJECTS, Capability,
    REQUESTS_STREAM_NAME, REQUESTS_STREAM_SUBJECTS, broadcast_requests_subject, requests_subject,
    worker_requests_subject,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::error::EventBusError;
use crate::transport::{AckHandle, Message, MessageSource, Transport};

// ---------------------------------------------------------------------------
// Configuration defaults
// ---------------------------------------------------------------------------

const DEFAULT_URL: &str = "nats://localhost:4222";
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
    max_deliver: i64,
    ack_wait: Duration,
    message_buffer: usize,
    worker_id: uuid::Uuid,
}

#[async_trait::async_trait]
impl Transport for NatsTransport {
    fn worker_id(&self) -> uuid::Uuid {
        self.worker_id
    }

    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), EventBusError> {
        let subject = subject.to_string();
        let payload = bytes::Bytes::from(payload.to_vec());
        let ack_future = self
            .jetstream
            .publish(subject, payload)
            .await
            .map_err(|e| EventBusError::Transport(format!("publish failed: {e}")))?;

        ack_future
            .await
            .map_err(|e| EventBusError::Transport(format!("publish ack failed: {e}")))?;

        Ok(())
    }

    async fn subscribe(
        &self,
        subscription: crate::Subscription,
    ) -> Result<mpsc::Receiver<Message>, EventBusError> {
        match subscription {
            crate::Subscription::Subject(subject) => self.subscribe_subject(&subject).await,
            crate::Subscription::WorkerCommands {
                worker_id,
                capabilities,
            } => {
                self.subscribe_worker_commands(worker_id, capabilities)
                    .await
            }
        }
    }

    async fn subscribe_subject(
        &self,
        subject: &str,
    ) -> Result<mpsc::Receiver<Message>, EventBusError> {
        let subject = subject.to_string();
        let stream_name = self.stream_name.clone();
        let max_deliver = self.max_deliver;
        let ack_wait = self.ack_wait;
        let buffer = self.message_buffer;
        let jetstream = self.jetstream.clone();

        let (tx, rx) = mpsc::channel(buffer);
        let work_queue_consumer = subject.replace('.', "-");
        spawn_consumer(ConsumerSpawnSpec {
            jetstream,
            stream_name,
            subject,
            consumer_name: work_queue_consumer,
            max_deliver,
            ack_wait,
            tx,
        })
        .await?;

        Ok(rx)
    }
}

impl NatsTransport {
    async fn subscribe_worker_commands(
        &self,
        worker_id: uuid::Uuid,
        capabilities: Vec<Capability>,
    ) -> Result<mpsc::Receiver<Message>, EventBusError> {
        let capabilities = Capability::effective_worker_subscriptions(&capabilities);
        let (tx, rx) = mpsc::channel(self.message_buffer);

        for capability in capabilities
            .iter()
            .copied()
            .filter(Capability::is_work_lane)
        {
            let subject = requests_subject(capability);
            spawn_consumer(ConsumerSpawnSpec {
                jetstream: self.jetstream.clone(),
                stream_name: self.stream_name.clone(),
                subject,
                consumer_name: format!("work-requests-{capability}"),
                max_deliver: self.max_deliver,
                ack_wait: self.ack_wait,
                tx: tx.clone(),
            })
            .await?;
        }

        for capability in capabilities.iter().copied() {
            let subject = worker_requests_subject(worker_id, capability);
            spawn_consumer(ConsumerSpawnSpec {
                jetstream: self.jetstream.clone(),
                stream_name: self.stream_name.clone(),
                subject,
                consumer_name: format!("worker-requests-{worker_id}-{capability}"),
                max_deliver: self.max_deliver,
                ack_wait: self.ack_wait,
                tx: tx.clone(),
            })
            .await?;
        }

        let Some(broadcast_stream_name) = self.broadcast_stream_name.clone() else {
            return Ok(rx);
        };

        for capability in capabilities
            .iter()
            .copied()
            .filter(Capability::is_broadcast_lane)
        {
            let subject = broadcast_requests_subject(capability);
            spawn_consumer(ConsumerSpawnSpec {
                jetstream: self.jetstream.clone(),
                stream_name: broadcast_stream_name.clone(),
                subject,
                consumer_name: format!("broadcast-requests-{worker_id}-{capability}"),
                max_deliver: self.max_deliver,
                ack_wait: self.ack_wait,
                tx: tx.clone(),
            })
            .await?;
        }

        Ok(rx)
    }
}

struct ConsumerSpawnSpec {
    jetstream: jetstream::Context,
    stream_name: String,
    subject: String,
    consumer_name: String,
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

    let stream = jetstream
        .get_stream(&stream_name)
        .await
        .map_err(|e| EventBusError::Transport(format!("get stream failed: {e}")))?;

    let consumer = stream
        .get_or_create_consumer(&consumer_name, consumer_config.clone())
        .await
        .map_err(|e| {
            EventBusError::Transport(format!("create consumer '{consumer_name}' failed: {e}"))
        })?;

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
                    let stream = jetstream
                        .get_stream(&stream_name)
                        .await
                        .map_err(|e| e.to_string())?;
                    let c = stream
                        .get_or_create_consumer(&consumer_name, consumer_config.clone())
                        .await
                        .map_err(|e| e.to_string())?;
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

#[async_trait::async_trait]
impl AckHandle for NatsAckHandle {
    async fn ack(mut self: Box<Self>) -> Result<(), EventBusError> {
        if let Some(msg) = self.0.take() {
            msg.ack()
                .await
                .map_err(|e| EventBusError::Transport(format!("ack failed: {e}")))?;
        }
        Ok(())
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
///     .urls(vec!["tls://nats.example.com:4222".to_string()])
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
}

impl NatsTransport {
    /// Create a builder for configuring a NATS transport.
    pub fn builder() -> NatsTransportBuilder {
        NatsTransportBuilder {
            urls: vec![DEFAULT_URL.to_string()],
            token: None,
            stream_name: REQUESTS_STREAM_NAME.to_string(),
            stream_subjects: REQUESTS_STREAM_SUBJECTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            broadcast_stream_name: Some(BROADCAST_REQUESTS_STREAM_NAME.to_string()),
            broadcast_stream_subjects: BROADCAST_REQUESTS_STREAM_SUBJECTS
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

    /// Connect to NATS and create/verify the JetStream streams.
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

        // Ensure the work-queue stream exists in the connected account.
        let stream_config = stream::Config {
            name: self.stream_name.clone(),
            subjects: self.stream_subjects,
            retention: stream::RetentionPolicy::WorkQueue,
            max_age: self.max_age,
            storage: stream::StorageType::Memory,
            ..Default::default()
        };

        ensure_stream_available(&jetstream, stream_config, "stream").await?;

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

            ensure_stream_available(&jetstream, broadcast_stream_config, "broadcast stream")
                .await?;

            debug!(stream = %broadcast_stream_name, "Broadcast JetStream stream ready");
        }

        Ok(NatsTransport {
            jetstream,
            stream_name: self.stream_name,
            broadcast_stream_name: self.broadcast_stream_name,
            max_deliver: self.max_deliver,
            ack_wait: self.ack_wait,
            message_buffer: self.message_buffer,
            worker_id: self.worker_id,
        })
    }
}

async fn ensure_stream_available(
    jetstream: &jetstream::Context,
    stream_config: stream::Config,
    label: &str,
) -> Result<(), EventBusError> {
    let stream_name = stream_config.name.clone();
    if jetstream.get_stream(&stream_name).await.is_ok() {
        return Ok(());
    }

    match jetstream.create_stream(stream_config).await {
        Ok(_) => Ok(()),
        Err(create_error) => {
            let create_error = create_error.to_string();
            if create_error.contains("stream name already in use")
                || create_error.contains("stream already exists")
            {
                jetstream
                    .get_stream(&stream_name)
                    .await
                    .map(|_| ())
                    .map_err(|e| {
                        EventBusError::Transport(format!(
                            "{label} setup raced with existing stream '{stream_name}': {e}"
                        ))
                    })
            } else {
                Err(EventBusError::Transport(format!(
                    "{label} setup failed: {create_error}"
                )))
            }
        }
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
        assert_eq!(builder.max_deliver, 3);
        assert_ne!(builder.worker_id, uuid::Uuid::nil());
    }

    #[test]
    fn builder_overrides() {
        let builder = NatsTransport::builder()
            .urls(vec!["tls://nats.prod.example.com".to_string()])
            .token("secret")
            .stream_name("CUSTOM_STREAM")
            .max_deliver(5)
            .ack_wait(Duration::from_secs(30));

        assert_eq!(builder.urls, vec!["tls://nats.prod.example.com"]);
        assert_eq!(builder.token.as_deref(), Some("secret"));
        assert_eq!(builder.stream_name, "CUSTOM_STREAM");
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
