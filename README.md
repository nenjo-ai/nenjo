# Nenjo

> **Beta** — Nenjo is under active development. APIs may change between releases.

An open-source Rust SDK and platform worker for building portable, provider-agnostic agentic AI workflows.

Nenjo gives you a programmable agent engine with tool use, persistent memory, multi-agent delegation, routine orchestration, and platform worker integration, while keeping the core SDK decoupled from any single LLM provider or transport.

## Install

```bash
curl -fsSL https://nenjo.ai/install | bash
```

## Crates

| Crate | Description |
|-------|-------------|
| [`nenjo`](crates/nenjo) | Core SDK: provider builder, agent turn loop, memory, manifests, abilities, domains, councils, and routines |
| [`nenjo-tool-api`](crates/tool-api) | Shared tool traits, specs, categories, calls, results, and SDK-level tool security inputs |
| [`nenjo-knowledge`](crates/knowledge) | Knowledge pack primitives, reusable knowledge tools, and optional embedded Nenjo documentation |
| [`nenjo-models`](crates/models) | LLM provider trait and implementations for OpenAI, Anthropic, Gemini, OpenRouter, Ollama, and OpenAI-compatible APIs |
| [`nenjo-xml`](crates/xml) | XML serialization and MiniJinja template rendering for structured prompt context |
| [`nenjo-events`](crates/events) | Typed command, response, stream, resource, and capability contracts for worker-to-platform messaging |
| [`nenjo-eventbus`](crates/eventbus) | Transport-agnostic event bus with NATS JetStream support |
| [`nenjo-secure-envelope`](crates/secure-envelope) | Secure envelope layer over the event bus, including encrypted payload helpers and codec traits |
| [`nenjo-crypto-auth`](crates/crypto-auth) | Worker enrollment, wrapped key state, and secure-envelope key provider primitives |
| [`nenjo-platform`](crates/platform) | Platform-backed manifest contracts, REST client, MCP tool contract, local backend, and access-policy helpers |
| [`nenjo-sessions`](crates/sessions) | Shared session contracts for runtime, worker, and future session services |
| [`nenjo-harness`](crates/harness) | Platform command handlers, active execution/session registries, event bridging, and trace/session runtime hooks around a provider |
| [`nenjo-worker`](crates/worker) | Platform worker implementation behind `nenjo run` |
| [`nenjo-cli`](bin) | CLI package that builds the `nenjo` binary |
| [`nenjo-integration-tests`](testing/integrations) | Integration test crate for provider-backed SDK flows |

## Separation Boundaries

Nenjo is split so the SDK can be embedded without pulling in the platform worker:

| Layer | Crates | Owns |
|-------|--------|------|
| Core SDK | `nenjo`, `nenjo-tool-api`, `nenjo-models`, `nenjo-xml`, `nenjo-knowledge` | Agent execution, prompts, tool contracts, models, memory, manifests, knowledge packs |
| Platform contracts | `nenjo-events`, `nenjo-sessions` | Transport-neutral wire and session types |
| Platform transport | `nenjo-eventbus`, `nenjo-secure-envelope`, `nenjo-crypto-auth` | Event delivery, secure envelopes, worker enrollment and keys |
| Manifest bridge | `nenjo-platform` | Platform REST/MCP manifest operations and local/platform synchronization |
| Harness | `nenjo-harness` | Platform command handlers, provider swapping, active execution registries, session writes, traces, and event bridging |
| Worker and CLI | `nenjo-worker`, `nenjo-cli` | Process lifecycle, config/bootstrap, event loop, provider/tool factories, concrete runtime tools, local stores, `nenjo run` |

## CLI Worker

The `nenjo` binary is built by the `nenjo-cli` package. Its `run` command starts the `nenjo-worker` runtime, connects to the platform event bus, and processes chat messages, tasks, cron schedules, repository events, crypto enrollment, and manifest updates.

```bash
# Start the worker (uses NENJO_API_KEY from env or ~/.nenjo/config.toml)
nenjo run

# With verbose logging
nenjo run --log-level "info,nenjo=debug"
```

The worker is resilient to service outages. Startup and the event loop use exponential backoff so the process can recover when platform services or NATS become available again.

### Worker Capabilities

Workers can be scoped to handle only specific workloads:

| Capability | Handles |
|------------|---------|
| `chat` | Chat messages, domain sessions, session management |
| `task` | Task execution, pause, resume, cancel |
| `cron` | Cron schedule enable, disable, trigger |
| `manifest` | Resource change notifications for agents, models, routines, projects, and related manifests |
| `repo` | Repository sync and unsync |

Run multiple workers with different capabilities to distribute load across machines or isolate workloads.

## Platform Integration

Platform-connected workers compose several crates:

- `nenjo-events` defines typed commands, responses, stream events, capabilities, and encrypted payload metadata.
- `nenjo-eventbus` transports those events over NATS JetStream or another transport implementation.
- `nenjo-secure-envelope` wraps event transport with encryption-aware encode/decode behavior.
- `nenjo-crypto-auth` manages worker enrollment and content-key access for encrypted payloads.
- `nenjo-platform` fetches bootstrap manifests, persists manifest mutations, exposes manifest MCP tools, and classifies sensitive payload content.
- `nenjo-sessions` defines shared session storage and coordination traits used by runtime implementations.
- `nenjo-harness` handles platform commands around an assembled provider and records session/trace state through runtime traits.
- `nenjo-worker` owns concrete config, bootstrap, event loop, native tools, local project knowledge sync, and file-backed session/artifact storage.

## Key Features

- **Provider-agnostic** — swap LLM providers without changing application code
- **Turn loop engine** — automatic tool call execution, parallel tool dispatch, context compaction, and streaming
- **Multi-agent delegation** — agents delegate subtasks to other agents with cycle detection and depth limiting
- **Persistent memory and artifacts** — project, core, and shared memory scopes plus project/workspace artifact indexes with automatic prompt injection
- **Routine orchestration** — DAG-based multi-step execution with gates, councils, lambdas, and cron scheduling
- **Platform worker runtime** — capability-scoped event handling, secure envelopes, manifest sync, and session coordination
- **Knowledge packs** — built-in, project, and custom documentation packs with metadata-first search and graph traversal tools
- **Transport-agnostic messaging** — pluggable event bus with a production-ready NATS JetStream implementation

## Embedding The SDK

Use the core SDK directly when you want to run agents in your own application without the platform worker:

```rust
use nenjo::Provider;

// Build a provider with your manifest, model factory, and tools.
let provider = Provider::builder()
    .with_loader(my_manifest_loader)
    .with_model_factory(my_model_factory)
    .with_tool_factory(my_tool_factory)
    .build()
    .await?;

// Look up an agent by name and run it.
let runner = provider
    .agent_by_name("coder")
    .await?
    .build()
    .await?;

let output = runner.chat("Refactor the auth module").await?;
println!("{}", output.text);

let mut handle = runner.chat_stream("Refactor the auth module").await?;
while let Some(event) = handle.recv().await {
    match event {
        nenjo::TurnEvent::ToolCallStart { calls } => {
            for call in calls {
                println!("calling {}...", call.tool_name);
            }
        }
        nenjo::TurnEvent::Done { .. } => break,
        _ => {}
    }
}
let output = handle.output().await?;
```

You can also start from an empty provider and describe the agent at runtime.
This is useful for tests, embedded flows, or callers that keep their own agent
catalog:

```rust
use nenjo::Provider;

let runner = Provider::builder()
    .build()
    .await?
    .new_agent()
    .with_agent_manifest(agent_manifest)
    .with_model_provider(model_manifest, model_provider)
    .build()
    .await?;
```

## Architecture

```
Provider::builder()
    .with_loader(loader)          // ManifestLoader — fetches agents, models, routines
    .with_model_factory(factory)  // ModelProviderFactory — creates LLM providers
    .with_tool_factory(tools)     // ToolFactory — creates tools per agent
    .with_knowledge_packs([KnowledgePackEntry::new("docs:app", docs_pack)])
    .with_memory(memory)          // Memory + artifacts — persistent agent state
    .build().await?
    -> Provider

provider.agent_by_name("coder").await? -> AgentBuilder -> .build().await? -> AgentRunner
provider.new_agent()                  -> AgentBuilder -> .build().await? -> AgentRunner
runner.chat("task").await?       -> TurnOutput { text, messages, tokens, tool_calls }
runner.chat_stream("task").await -> ExecutionHandle { recv(), output() }

nenjo run -> nenjo-cli -> nenjo-worker
          -> secure envelope event bus
          -> nenjo-harness handlers
          -> Provider / AgentRunner
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
