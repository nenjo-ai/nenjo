# nenjo-events

Typed event definitions for agent-to-platform messaging in the Nenjo orchestration framework.

## Overview

This crate defines every event that flows between the Nenjo backend and agent harnesses. It is transport-agnostic — the types serialize to/from JSON and can be used with NATS, WebSockets, or any message transport.

## Event directions

| Direction | Type |
|-----------|------|
| Backend → Harness | `Command` |
| Harness → Backend | `Response` |
| Real-time streaming | `StreamEvent` |
| Wire wrapper | `Envelope` |

## Usage

```rust
use nenjo_events::{Command, Response, StreamEvent, Envelope};

// Commands are tagged enums
let cmd = Command::ChatMessage {
    content: "Hello".into(),
    session_id: uuid::Uuid::new_v4(),
    // ...
};

// Serialize to JSON
let json = serde_json::to_string(&cmd)?;
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
