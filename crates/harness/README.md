# nenjo-harness

`nenjo-harness` is the developer-facing runtime wrapper around a typed
`Provider`. It runs chat, task, cron, and heartbeat requests while handling
session runtime calls, transcripts, execution trace hooks, and scheduling.

The crate re-exports the main Nenjo provider assembly types, so most embedded
apps can import `nenjo_harness` alone for both provider construction and harness
execution.

## Typed Runtime Assembly

`Harness` keeps concrete runtime types through its builder:

```rust
use nenjo_harness::{Harness, Provider};

let provider = Provider::builder()
    .with_manifest(manifest)
    .with_model_factory(model_factory)
    .with_tool_factory(tool_factory)
    .build()
    .await?;

let harness = Harness::builder(provider)
    .with_session_runtime(session_runtime)
    .build();
```

Each `with_*` method transitions the builder to the concrete type passed in.
Omitted integrations use no-op defaults. Platform manifest synchronization,
transport response routing, and worker lifecycle wiring live in `nenjo-worker`.

For local or embedded apps, enable `local-runtime` to use the built-in
filesystem-backed session runtime:

```toml
nenjo-harness = { version = "...", features = ["local-runtime"] }
```

```rust
use nenjo_harness::{FileSessionRuntime, FileSessionStores, Harness};

let stores = FileSessionStores::new(".nenjo/state");
let sessions = FileSessionRuntime::new(stores);
let harness = Harness::builder(provider)
    .with_session_runtime(sessions)
    .build();
```

Session services are grouped behind `harness.sessions()`, including trace
services:

```rust
let sessions = harness.sessions();
let traces = sessions.traces();
```

## Requests

Harness execution APIs take builder-style request values. Required arguments go
in `new`; optional context is added with `with_*` methods:

```rust
use std::time::Duration;

use nenjo_harness::{ChatRequest, CronRequest, HeartbeatRequest, TaskRequest};

let output = harness
    .chat(ChatRequest::new(session_id, "coder", "Fix the failing test")
        .with_project(project_id))
    .await?;

let task_output = harness
    .task(TaskRequest::new(task_id, project_id, "Fix login", "Repair OAuth callback")
        .with_agent("coder")
        .with_slug("fix-login"))
    .await?;

let mut cron = harness
    .cron(CronRequest::new(routine_id, "0 */6 * * * *")
        .with_project(project_id))
    .await?;

let mut heartbeat = harness
    .heartbeat(HeartbeatRequest::new(agent_id, Duration::from_secs(300)))
    .await?;
```
