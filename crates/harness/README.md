# nenjo-harness

`nenjo-harness` is the developer-facing runtime wrapper around a typed
`Provider`. It runs chat and tasks while handling session runtime calls,
transcripts, execution trace hooks, the durable task inbox, and task schedules.

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
use nenjo_harness::{ChatRequest, TaskRequest};

let output = harness
    .chat(ChatRequest::new("coder", "Fix the failing test")
        .with_session(session_id)
        .with_project("website"))
    .await?;

let task_output = harness
    .task(TaskRequest::new("Fix login", "Repair OAuth callback")
        .with_task_id(task_id)
        .with_project("website")
        .with_agent("coder")
        .with_slug("fix-login"))
    .await?;
```

Recurring work is not a separate execution kind. Hosts install a
`TaskSchedule` into `TaskRuntime`; when it becomes due, the runtime creates the
same durable inbox submission used by manual tasks. An agent-assigned schedule
is therefore just a scheduled task with `TaskExecutionTarget::Agent`.

The canonical `TaskScheduleDefinition` supports interval, daily, weekly,
monthly, yearly, and advanced cron recurrence in an IANA timezone, plus end
date and occurrence-count boundaries. Calendar recurrences preserve their
local wall-clock time across daylight-saving changes. The runtime stores the
materialized occurrence count with its inbox state so finite schedules remain
correct during offline operation and restart recovery.

`TaskRuntime::cancel` is the single local cancellation boundary. It atomically
marks queued or running receipts Cancelled, emits that durable state before
signalling a running `TaskExecutor`, and remembers bounded early cancellation
intents so a later transport delivery cannot start cancelled work. Every
receipt transition increments a persisted revision; restart recovery requeues
interrupted work with a newer revision and an explicit recovery marker.
