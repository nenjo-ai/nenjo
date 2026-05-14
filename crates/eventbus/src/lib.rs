//! # nenjo-eventbus
//!
//! Transport-agnostic event bus for the Nenjo agent platform.
//!
//! This crate provides [`EventBus`], a raw envelope transport for sending and
//! receiving agent events. The underlying transport is pluggable via the
//! [`Transport`] trait — enable the `nats` feature for a production-ready
//! NATS JetStream implementation. For high-throughput workers, clone an
//! [`EventBusPublisher`] from the bus and run outbound publishes separately
//! from the inbound receive loop.
//!
//! ## Quick start (NATS)
//!
//! ```ignore
//! use nenjo_eventbus::{EventBus, EventBusBuilder, Subscription};
//! use nenjo_eventbus::nats::NatsTransport;
//! use nenjo_events::Envelope;
//!
//! let transport = NatsTransport::builder()
//!     .urls(vec!["nats://localhost:4222".to_string()])
//!     .token("my-api-key")
//!     .build()
//!     .await?;
//!
//! let bus = EventBus::builder()
//!     .transport(transport)
//!     .subscription(Subscription::worker_commands(worker_id, capabilities))
//!     .build()
//!     .await?;
//!
//! // Send a raw envelope directly, or clone bus.publisher() for an outbound lane.
//! let envelope = Envelope::new(user_id, serde_json::json!({ "type": "ping" }));
//! bus.send_envelope("requests.chat", &envelope).await?;
//!
//! // Receive envelopes
//! while let Some(received) = bus.recv_envelope().await? {
//!     println!("{:?}", received.envelope);
//!     received.ack().await?;
//! }
//! ```

mod bus;
mod error;
mod transport;

pub use bus::{EventBus, EventBusBuilder, EventBusPublisher, ReceivedEnvelope};
pub use error::EventBusError;
pub use transport::{AckHandle, Message, MessageSource, NoOpAck, Subscription, Transport};

// Re-export event types for convenience.
pub use nenjo_events::Envelope;

#[cfg(feature = "nats")]
pub mod nats;
