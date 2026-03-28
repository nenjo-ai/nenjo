# Nenjo

An open-source Rust SDK for building portable, provider-agnostic agentic AI workflows.

Nenjo gives you a programmable agent engine with tool use, persistent memory, multi-agent delegation, and routine orchestration — all decoupled from any single LLM provider.

## Crates

| Crate | Description |
|-------|-------------|
| [`nenjo`](crates/nenjo) | Core SDK — agent turn loop, provider abstraction, memory, abilities, domains, and routine orchestration |
| [`nenjo-events`](crates/events) | Typed event definitions for agent-to-platform messaging |
| [`nenjo-eventbus`](crates/eventbus) | Transport-agnostic event bus with NATS JetStream support |
| [`nenjo-models`](crates/models) | LLM provider trait and implementations (OpenAI, Anthropic, Gemini, and more) |
| [`nenjo-tools`](crates/tools) | Tool trait and built-in tool implementations (shell, file, git, search, web, memory) |
| [`nenjo-xml`](crates/xml) | XML template engine for structured prompt context |

## CLI runner

The `nenjo` CLI connects your agents to the Nenjo platform over NATS, processing chat messages, tasks, cron schedules, and manifest updates in real time.

```bash
# Start the worker (uses NENJO_API_KEY from env or config)
nenjo run

# With explicit options
nenjo run --api-key sk-... --log-level debug

# Scoped to specific capabilities
nenjo run --capabilities chat,task,manifest
```

The runner is resilient to outages — startup and the event loop use exponential backoff so the worker automatically recovers when services come back online.

### Worker capabilities

Workers can be scoped to handle only specific workloads:

| Capability | Handles |
|------------|---------|
| `chat` | Chat messages, domain sessions, session management |
| `task` | Task execution, pause/resume/cancel |
| `cron` | Cron schedule enable/disable/trigger |
| `manifest` | Resource change notifications (agents, models, routines, etc.) |
| `repo` | Repository sync/unsync |

Run multiple workers with different capabilities to distribute load across machines or isolate workloads.

## Key features

- **Provider-agnostic** — swap LLM providers (OpenAI, Anthropic, Gemini, OpenRouter, Ollama, or any OpenAI-compatible API) without changing application code
- **Turn loop engine** — automatic tool call execution, parallel tool dispatch, context window compaction, and streaming
- **Multi-agent delegation** — agents delegate subtasks to other agents with cycle detection and depth limiting
- **Persistent memory** — 3-tier scoped memory (project, core, shared) with automatic prompt injection
- **Routine orchestration** — DAG-based multi-step execution with gates, councils, lambdas, and cron scheduling
- **Reliability built in** — exponential backoff retries, rate-limit handling, provider fallback, and model failover
- **Transport-agnostic messaging** — pluggable event bus with a production-ready NATS JetStream implementation

## Quick start

```rust
use nenjo::{Provider, ProviderBuilder};

// Build a provider with your manifest, model factory, and tools
let provider = Provider::builder()
    .with_loader(my_manifest_loader)
    .with_model_factory(my_model_factory)
    .with_tool_factory(my_tool_factory)
    .build()
    .await?;

// Look up an agent by name and run it
let runner = provider.agent_by_name("coder")?.build();

// Simple API — returns when done
let output = runner.chat("Refactor the auth module").await?;
println!("{}", output.text);

// Streaming API — real-time events
let mut handle = runner.chat_stream("Refactor the auth module").await?;
while let Some(event) = handle.recv().await {
    match event {
        nenjo::TurnEvent::ToolCallStart { tool_name, .. } => println!("calling {tool_name}..."),
        nenjo::TurnEvent::Done { .. } => break,
        _ => {}
    }
}
let output = handle.output().await?;
```

## Architecture

```
Provider::builder()
    .with_loader(loader)          // ManifestLoader — fetches agents, models, routines
    .with_model_factory(factory)  // ModelProviderFactory — creates LLM providers
    .with_tool_factory(tools)     // ToolFactory — creates tools per agent
    .with_memory(memory)          // Memory — persistent agent knowledge
    .build().await?
    -> Provider

provider.agent_by_name("coder")? -> AgentBuilder -> .build() -> AgentRunner
runner.chat("task").await?       -> TurnOutput { text, messages, tokens, tool_calls }
runner.chat_stream("task").await -> ExecutionHandle { recv(), output() }
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
