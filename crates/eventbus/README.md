# nenjo-eventbus

Transport-agnostic event bus for routing Nenjo envelopes over NATS or custom transports.

## Features

- **Transport-agnostic** — implement the `Transport` trait for any message broker
- **NATS JetStream** — production-ready implementation with at-least-once delivery (enable the `nats` feature)
- **Envelope interface** — sends and receives raw `nenjo-events` envelopes
- **Split IO lanes** — clone an outbound publisher so publishing cannot stall inbound receive polling
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
use nenjo_eventbus::{EventBus, Subscription};
use nenjo_eventbus::nats::NatsTransport;
use nenjo_events::Envelope;

let transport = NatsTransport::builder()
    .urls(vec!["nats://localhost:4222".to_string()])
    .token("my-api-key")
    .build()
    .await?;

let mut bus = EventBus::builder()
    .transport(transport)
    .subscription(Subscription::worker_commands(worker_id, capabilities))
    .build()
    .await?;

// Send directly, or clone bus.publisher() for an outbound lane.
let envelope = Envelope::new(user_id, serde_json::json!({ "type": "ping" }));
bus.send_envelope("work_requests.chat", &envelope).await?;

let publisher = bus.publisher();
tokio::spawn(async move {
    let _ = publisher.send_envelope("work_requests.chat", &envelope).await;
});

while let Some(received) = bus.recv_envelope().await? {
    println!("{:?}", received.envelope);
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
