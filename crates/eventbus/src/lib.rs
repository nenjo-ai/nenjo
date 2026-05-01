//! # nenjo-eventbus
//!
//! Transport-agnostic event bus for the Nenjo agent platform.
//!
//! This crate provides [`EventBus`], a raw envelope transport for sending and
//! receiving agent events. The underlying transport is pluggable via the
//! [`Transport`] trait — enable the `nats` feature for a production-ready
//! NATS JetStream implementation.
//!
//! ## Quick start (NATS)
//!
//! ```ignore
//! use nenjo_eventbus::{EventBus, EventBusBuilder};
//! use nenjo_eventbus::nats::NatsTransport;
//! use nenjo_events::Envelope;
//!
//! let transport = NatsTransport::builder()
//!     .url("nats://localhost:4222")
//!     .token("my-api-key")
//!     .build()
//!     .await?;
//!
//! let bus = EventBus::builder()
//!     .transport(transport)
//!     .build()
//!     .await?;
//!
//! // Send a raw envelope
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

pub use bus::{EventBus, EventBusBuilder, ReceivedEnvelope};
pub use error::EventBusError;
pub use transport::{AckHandle, Message, NoOpAck, Transport};

// Re-export event types for convenience.
pub use nenjo_events::Envelope;

#[cfg(feature = "nats")]
pub mod nats;
