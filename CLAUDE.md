# Nenjo SDK — CLAUDE.md

## Workspace Overview

Rust monorepo at `nenjo/` inside `nenjo-mono`. The workspace contains:

```
crates/
├── nenjo/          — The SDK. Agent execution engine, provider, memory, manifest.
├── models/         — LLM provider trait (ModelProvider) + implementations (OpenAI, Anthropic, Gemini, etc.)
├── tools/          — Tool trait + built-in tool implementations (shell, file, git, search, web, memory)
├── xml/            — Enables creating xml tags with rust structs: template engine (MiniJinja)
├── events/         — NATS event types (Command, Response, Envelope) for harness ↔ backend messaging
├── eventbus/       — Event bus abstraction
├── api-client/     — Typed HTTP client for Nenjo backend API. Implements ManifestLoader.
├── harness/        — Nenjo platform orchestration (NATS, routing, routines, cron, worker). Uses nenjo crate.
├── runner/         — CLI runner
bin/                — nenjo CLI binary
testing/
└── integrations/   — Integration tests with real LLM (OpenRouter). Requires OPENROUTER_API_KEY.
```

## The `nenjo` Crate (Core SDK)

### Purpose

The `nenjo` crate IS the engine. It owns the agent turn loop, prompt building, memory, abilities, domains, and the full execution pipeline

### Architecture

```
Provider::builder()
    .with_loader(NenjoClient::new(url, key))    // ManifestLoader → fetches agents/models/routines
    .with_loader(LocalManifestLoader::new("."))  // loads .nenjo/context/*.md
    .with_model_factory(factory)                 // ProviderFactory → creates LLM providers
    .with_tool_factory(tools)                    // ToolFactory → creates tools per agent
    .with_memory(MarkdownMemory::new("./mem"))   // Memory trait → persistent agent knowledge
    .with_agent_config(config)                   // AgentConfig → turn loop settings
    .build().await?
    → Provider

provider.agent_by_name("coder")?               // looks up agent in manifest
    → AgentBuilder                              // pre-filled from manifest data

builder.build()
    → AgentRunner                               // ready to execute

runner.chat("Hello").await?                     // simple API → TurnOutput
runner.chat_stream("Hello").await?              // streaming API → ExecutionHandle
runner.domain_expansion("prd")?                 // activate a domain → new AgentRunner
```

### Module Map

#### `provider/` — Provider + ProviderBuilder
- `Provider` — root object, holds manifest + factories + memory + config
- `ProviderBuilder` — async builder, accepts loaders/factories/memory/config
- `ProviderFactory` trait — maps model_provider string → `Arc<dyn ModelProvider>`
- `ToolFactory` trait — creates tools for an agent from manifest data
- `from_manifest()` / `from_manifest_with_memory()` — direct construction

#### `agents/` — Agent building blocks

- **`instance.rs`** — `AgentInstance` struct. Holds provider, tools, prompt config, prompt context, memory_xml, documents_xml. Has `build_prompts(&task)` for prompt rendering. Derives `Clone`.
- **`builder.rs`** — `AgentBuilder`. Pre-filled by Provider, allows overrides (`.with_tool()`, `.with_memory()`, `.with_config()`). `build()` → `AgentRunner`.
- **`prompts.rs`** — `PromptConfig` (system_prompt, developer_prompt, templates, memory_profile), `PromptContext` (available agents/routines/skills/abilities/domains), render conversion functions (`render_agent()`, `render_ability()`, etc.)
- **`abilities.rs`** — `UseAbilityTool`. Runs a sub-execution with the ability's prompt and filtered tools. Auto-added when agent has abilities. No recursion (use_ability filtered from sub-execution tools).
- **`runner/mod.rs`** — `AgentRunner`. Wraps `Arc<AgentInstance>`. Two APIs:
  - Simple: `chat()`, `task()` → `TurnOutput`
  - Streaming: `chat_stream()`, `task_stream()` → `ExecutionHandle` (events via mpsc channel, spawned task)
  - `domain_expansion(name)` → new AgentRunner with domain activated
- **`runner/turn_loop.rs`** — The core LLM loop. `run(agent, messages, events_tx)`. Calls provider → parses tool calls → executes tools → repeats. Context compaction, tool argument truncation, parallel tool execution.
- **`runner/types.rs`** — `TurnEvent` (ToolCallStart, ToolCallEnd, Done), `TurnOutput`, `TurnLoopConfig`, `ToolCall`.

#### `manifest.rs` — Manifest types
- `Manifest` — full catalog: agents, models, routines, skills, domains, abilities, context blocks, etc.
- `ManifestLoader` trait — async, returns `Manifest`. Multiple loaders merge in order.
- `LocalManifestLoader` — scans `.nenjo/context/*.md`, each file becomes a context block.
- `Manifest::merge()` — additive, context blocks use last-write-wins on name collision.
- Type-first naming: `AgentManifest`, `ModelManifest`, `RoutineManifest`, `ProjectManifest`, etc.
- `ManifestResponse` — the raw API response, converts to `Manifest` via `From`.

#### `memory/` — Persistent agent memory
- `Memory` trait — store, search, delete, delete_stale, summaries (get/upsert/list)
- `MarkdownMemory` — file-based backend, YAML frontmatter + fact text, keyword search
- `MemoryScope` — 3-tier namespacing: project (per-agent-per-project), core (cross-project), shared (all agents in project)
- `memory/tools.rs` — `MemoryStoreTool`, `MemoryRecallTool`, `MemoryForgetTool`. Auto-added by Provider when memory is configured.
- `memory/prompt.rs` — `build_memory_xml()` loads summaries from all 3 tiers → `<memory>` XML for prompt injection
- Memory is a context block (`"memory"`) — users can customize the template wrapping

#### `config.rs` — AgentConfig
- `max_tool_iterations`, `parallel_tools`, `max_context_tokens`, `max_history_messages`, `max_delegation_depth`, `compact_context`, `tool_dispatcher`
- Set on Provider (applies to all agents), overridable per-agent via AgentBuilder

#### `types.rs` — Shared types
- `TaskType` enum: Task, Chat, Gate, CouncilSubtask
- `StepResult`, `RenderContext`, `ActiveDomain`, `DomainSessionManifest`, `DomainToolConfig`, etc.
- These are the execution-level types used by the turn loop and prompt building

### Key Design Decisions

1. **No Engine trait** — the nenjo crate IS the engine. Provider + AgentRunner + turn loop are concrete, not trait-delegated.
2. **Tools are `Vec<Arc<dyn Tool>>`** — enables sharing between parent and ability sub-executions (Arc is Clone, Box is not).
3. **AgentInstance is Clone** — needed for domain_expansion and ability sub-executions which clone and modify the instance.
4. **Turn loop events_tx is `Option`** — `None` for ability sub-executions (no event emission), `Some` for the runner (streams to ExecutionHandle).
5. **Memory is pre-computed** — runner loads memory XML async before prompt building, sets it on a cloned instance. Prompt building is sync.
6. **Documents XML on instance** — pre-computed in `AgentRunner::new()` (sync, reads manifest from disk).
7. **Context blocks are user-customizable** — memory, memory_profile, agents, routines, skills, abilities, domains, project, task, gate, cron, MCP are all context blocks with `{{items}}` templates.
8. **Context window on ModelProvider** — `context_window(model) -> Option<usize>` on the trait. Each provider returns the correct value. Turn loop applies 80% safety margin. Fallback: 100K.
9. **Imports always at top** — never inline `use` statements in function bodies.
10. **Type-first naming for manifests** — `AgentManifest` not `ManifestAgent`.

### Crate Dependencies (nenjo)

```
nenjo
├── nenjo-models    — ModelProvider trait + LLM implementations
├── nenjo-tools     — Tool trait + built-in tools + SecurityPolicy
├── nenjo-xml   — Template rendering, context blocks, XML builder
├── nenjo-events    — StreamEvent for NATS/WebSocket (re-exported)
├── async-trait, tokio, uuid, serde, serde_json, anyhow, tracing, chrono, futures-util
```

### Tests

- `crates/nenjo/tests/agents.rs` — 5 tests: runner chat, history, custom tools, tool factory, prompt building
- `crates/nenjo/tests/memory.rs` — 10 tests: provider with/without memory, memory tools round-trip, store/recall/forget, summaries, XML injection
- `crates/nenjo/src/` — 51 lib unit tests (provider, turn loop, memory markdown, types)
- `testing/integrations/tests/agents.rs` — Real LLM tests (OpenRouter): tool call round-trip, memory store+recall+forget, use_ability, domain_expansion, error cases

### What's NOT in nenjo (stays in harness)

- NATS event loop, WebSocket streaming
- Routing (maps chat/task requests to agents)
- Routine executor (DAG orchestration)
- Cron manager
- Harness setup, bootstrap sync
- External MCP client
- Document sync (S3)

### Multi-Worker Architecture

Workers connect to NATS with capability-based subject routing. Each user can run multiple workers with different API keys, each scoped to specific capabilities.

#### Capabilities

```
Chat       — chat.message, chat.domain_enter/exit, chat.cancel, chat.session_delete
Task       — task.execute, execution.cancel/pause/resume
Cron       — cron.enable, cron.disable, cron.trigger
Manifest   — manifest.changed
Repo       — repo.sync, repo.unsync
```

#### Subject Pattern

```
agent.requests.<user_id>.<capability>   — backend → worker (capability-routed)
agent.responses.<user_id>               — worker → backend (flat, all workers share)
```

Workers subscribe to `agent.requests.<user_id>.*` (wildcard). The backend publishes to the capability-specific subject (e.g. `agent.requests.<uid>.chat`). NATS JetStream WorkQueue retention with a shared consumer (`worker-v2-<user_id>`) round-robins messages across active workers.

#### Exclusive Capabilities

`Chat` and `Cron` are **exclusive** — only one worker per user may hold each. These capabilities involve stateful in-memory sessions (domain sessions, cron schedulers) that cannot be shared. Enforcement happens at the NATS auth callout: if another worker already holds an exclusive capability, the connection is denied.

#### Worker Identity

Workers use `api_key_id` (stable UUID from the database) as their identity, not an ephemeral per-process UUID. This flows through:

1. Backend bootstrap response → `Manifest.api_key_id`
2. Cached in `~/.nenjo/data/auth.json` (alongside `user_id`)
3. Set as `NatsTransport::worker_id`
4. Sent in `WorkerHeartbeat` and `WorkerRegistered` responses
5. Used as the Redis presence key: `worker:connected:<user_id>:<api_key_id>`

#### Worker Presence

Redis keys track which workers are connected:
- `worker:connected:<user_id>` — legacy key, any worker present
- `worker:connected:<user_id>:<api_key_id>` — per-worker metadata (capabilities, version)
- TTL: 90s, refreshed by heartbeats every 30s
- Auth callout writes presence immediately on successful auth (closes race window)
- API key revocation cleans up presence immediately

#### API Key Capabilities

API keys can be scoped to specific capabilities via the `capabilities` column (`TEXT[]`). Empty = all capabilities. The auth callout reads capabilities from the key and scopes NATS subscribe permissions accordingly.

#### Application Profiles

| Application     | Capabilities                         |
|-----------------|--------------------------------------|
| Full runner     | `[]` (all)                           |
| Terminal app    | `["manifest", "task"]`               |
| SDK (minimal)   | `["manifest"]`                       |
| CI worker       | `["task", "manifest"]`               |

#### Key Files

- `crates/events/src/capability.rs` — `Capability` enum, `Command::capability()`, `from_command_type()`
- `crates/events/src/subject.rs` — `requests_subject(user_id, cap)`, `requests_subject_all(user_id)`
- `crates/events/src/response.rs` — `WorkerHeartbeat`, `WorkerRegistered` with worker_id, capabilities, version
- `crates/eventbus/src/nats.rs` — shared consumer `worker-v2-<user_id>`, transport `worker_id`
- `crates/eventbus/src/transport.rs` — `Transport::worker_id()` trait method
- `crates/harness/src/harness.rs` — `resolved_capabilities()`, registration/heartbeat with version
- `crates/harness/src/config/schema.rs` — `capabilities: Vec<Capability>` config field
- `crates/runner/src/lib.rs` — `--capabilities` CLI arg, `api_key_id` flow to transport

### Running Tests

```bash
# Unit + integration tests (no API key needed)
cargo test -p nenjo --lib --test agents --test memory

# All workspace tests
cargo test -p nenjo-models --lib -p nenjo --lib --test agents --test memory

# Real LLM integration tests (needs API key)
OPENROUTER_API_KEY=sk-or-... cargo test -p nenjo-integration-tests -- --nocapture
```
