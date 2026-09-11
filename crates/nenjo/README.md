# nenjo

Agent orchestration SDK for building agentic AI workflows with tool use, memory, and multi-agent delegation.

## Features

- **Provider-based architecture** — pluggable LLM providers, tool factories, and memory backends
- **Turn loop engine** — automatic tool call execution, context compaction, and streaming
- **Multi-agent delegation** — agents delegate work to other agents with cycle detection and depth limiting
- **Knowledge packs** — provider-level document packs with reusable search, read, and graph-neighbor tools
- **Persistent memory** — 3-tier scoped memory (project, core, shared) with automatic prompt injection
- **Abilities & domains** — structured sub-execution modes and domain-specific interaction sessions
- **Routine orchestration** — DAG-based step execution with gates, councils, and cron scheduling
- **Streaming API** — real-time event streaming via channels for responsive UIs

## Quick start

```rust
use nenjo::{Buffered, ChatInput, Provider, Streaming};
use nenjo_knowledge::tools::KnowledgePackEntry;

let provider = Provider::builder()
    .with_loader(my_manifest_loader)
    .with_model_factory(my_model_factory)
    .with_tool_factory(my_tool_factory)
    .with_knowledge_packs([KnowledgePackEntry::local("app", my_knowledge_pack)?])
    .build()
    .await?;

let runner = provider
    .agent_by_name("coder")
    .await?
    .build()
    .await?;

// Buffered API
let output = runner
    .chat(ChatInput::new("Hello"), Buffered)
    .await?
    .output()
    .await?;

// Streaming API
let mut handle = runner.chat(ChatInput::new("Hello"), Streaming).await?;
while let Some(event) = handle.recv().await {
    // Process TurnEvent variants
}
let output = handle.output().await?;
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.

## Execution admission

`ProviderBuilder::with_root_admission(AdmissionPool)` shares a bounded root
execution gate across chats and tasks. SDK callers can opt in; workers configure
this through `[execution]`. `AgentConfig` controls runnable immediate children,
a shared root-tree descendant budget, pending children, and admission deadlines.
Nested work inherits its root identity across abilities, delegation, and spawned
agents. Harness wait controls hold no runnable descendant permit.

`nenjo::concurrency::AdmissionPool` provides cancellation-safe bounded queues,
queue deadlines, round-robin service among root executions, and RAII capacity
permits. Worker model transports, PDF rendering, and shell execution use this
primitive. `ResourceCapacityWaiting` and `ResourceCapacityAcquired` turn events
make scheduling waits observable without treating them as model failures.

Implementation boundaries:

- [`concurrency/admission.rs`](src/concurrency/admission.rs) owns fair queue
  rotation, queue deadlines, and permit cleanup, including cancellation after a
  ticket has been granted but before its future resumes.
- [`concurrency/execution.rs`](src/concurrency/execution.rs) carries root identity
  across task spawns and shares one runnable lease among parallel phases of the
  same child. The final phase releases the lease; harness waits bypass it.
- The worker's provider admission wrapper acquires physical request capacity
  inside retry handling, so completed attempts release capacity before backoff.

## Ability execution internals

The public `agents::abilities` path retains the discovery and invocation tools.
Its private modules separate assignment lookup (`registry`), tool dispatch
(`broker`), child construction (`instance`), child communication (`child_tools`),
operation execution (`execution`), and transcript/event forwarding (`events`).
Generic `inspect`, `send_input`, `stop`, and `wait` tools belong to the shared
[`async_ops/controls.rs`](src/agents/async_ops/controls.rs) runtime.

An ability receives its own task input, developer prompt, and tool assignments.
Caller history is attached to its start event as trace evidence. Only a valid
`finish` result completes the child turn; ordinary prose does not. Prompt errors,
execution errors, and explicit finish outcomes share one completion path that
updates operation state and the parent event. Nested tool errors remain
recoverable and do not finish the ability.

Ability tests are grouped by setup, broker behavior, completion, execution, and
shared controls under `agents/abilities/tests`. Admission and execution-scope
tests remain beside their implementations and cover cancellation, queue bounds,
fairness, and permit lifetimes.
