# nenjo-harness

`nenjo-harness` is the platform-facing runtime wrapper around a typed
`nenjo::Provider`. It owns command handlers, session runtime calls, manifest
updates, execution trace hooks, response bridging, and preview formatting.

The harness does not own model provider construction. Model factories, tool
factories, memory, event transport, auth, persistence, and process lifecycle are
provided by the host.

## Typed Runtime Assembly

`Harness` keeps concrete runtime types through its builder:

```rust
let harness = nenjo_harness::Harness::builder(provider)
    .with_session_runtime(session_runtime)
    .with_execution_trace_runtime(trace_runtime)
    .with_manifest_client(client)
    .with_manifest_store(manifest_store)
    .with_mcp_runtime(mcp_runtime)
    .build();
```

Each `with_*` method transitions the builder to the concrete type passed in.
Omitted integrations use no-op defaults.

## Dynamic Dispatch

Production harness code is generic over provider and runtime traits. The only
remaining `dyn nenjo::ModelProvider` references are test fakes that satisfy the
current `nenjo` provider factory API.

Tool execution remains dynamic in `nenjo`, which is intentional for open-ended
tool sets.
