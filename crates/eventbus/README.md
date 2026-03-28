# nenjo-eventbus

Transport-agnostic event bus for routing agentic AI workflow commands over NATS or custom transports.

## Features

- **Transport-agnostic** — implement the `Transport` trait for any message broker
- **NATS JetStream** — production-ready implementation with at-least-once delivery (enable the `nats` feature)
- **Typed interface** — sends and receives strongly-typed `Command` and `Response` enums
- **Acknowledgment support** — pluggable ack handles prevent message loss

## Installation

```toml
[dependencies]
nenjo-eventbus = "0.1"

# With NATS support:
nenjo-eventbus = { version = "0.1", features = ["nats"] }
```

## Quick start

```rust,ignore
use nenjo_eventbus::{EventBus, Transport};
use nenjo_eventbus::nats::NatsTransport;

let transport = NatsTransport::builder()
    .url("nats://localhost:4222")
    .token("my-api-key")
    .build()
    .await?;

let mut bus = EventBus::builder()
    .user_id(user_id)
    .transport(transport)
    .build()
    .await?;

// Receive commands
while let Some(received) = bus.recv_command().await? {
    println!("{:?}", received.command);
    received.ack().await?;
}
```

## Custom transports

Implement the `Transport` trait to plug in any message broker:

```rust,ignore
use nenjo_eventbus::{Transport, Message, AckHandle, NoOpAck};

struct MyTransport;

impl Transport for MyTransport {
    // implement publish, subscribe, worker_id
}
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
