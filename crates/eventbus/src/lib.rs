//! # nenjo-eventbus
//!
//! Transport-agnostic event bus for the Nenjo agent platform.
//!
//! This crate provides [`EventBus`], a typed interface for sending and
//! receiving agent events. The underlying transport is pluggable via the
//! [`Transport`] trait — enable the `nats` feature for a production-ready
//! NATS JetStream implementation.
//!
//! ## Quick start (NATS)
//!
//! ```ignore
//! use nenjo_eventbus::{EventBus, EventBusBuilder};
//! use nenjo_eventbus::nats::NatsTransport;
//!
//! let transport = NatsTransport::builder()
//!     .url("nats://localhost:4222")
//!     .token("my-api-key")
//!     .build()
//!     .await?;
//!
//! let bus = EventBus::builder()
//!     .user_id(user_id)
//!     .transport(transport)
//!     .build()
//!     .await?;
//!
//! // Send a command
//! bus.send(Command::ChatMessage { ... }).await?;
//!
//! // Receive responses
//! while let Some(response) = bus.recv().await? {
//!     println!("{response:?}");
//! }
//! ```

mod bus;
mod error;
mod transport;

pub use bus::{EventBus, EventBusBuilder, ReceivedCommand, ReceivedResponse};
pub use error::EventBusError;
pub use transport::{AckHandle, Message, NoOpAck, Transport};

// Re-export event types for convenience.
pub use nenjo_events::{Command, Envelope, Response, StreamEvent};

#[cfg(feature = "nats")]
pub mod nats;
