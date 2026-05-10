# AGENTS.md — Nenjo SDK Development Guide

This file provides guidance for agentic coding agents operating in this repository.

## Project Overview

Rust monorepo with a core SDK, platform worker runtime, transport contracts, and supporting security/session crates.

| Crate | Description |
|-------|-------------|
| `nenjo` | Core SDK: provider builder, agent turn loop, memory, manifests, abilities, domains, councils, routines |
| `nenjo-models` | LLM provider trait and implementations (OpenAI, Anthropic, Gemini, OpenRouter, Ollama, OpenAI-compatible APIs) |
| `nenjo-xml` | XML serialization and MiniJinja template rendering for structured prompt context |
| `nenjo-events` | Typed command, response, stream, resource, and capability contracts for worker-to-platform messaging |
| `nenjo-eventbus` | Transport-agnostic event bus with NATS JetStream support |
| `nenjo-secure-envelope` | Secure envelope layer over event transport plus encrypted payload helpers |
| `nenjo-crypto-auth` | Worker enrollment, wrapped key state, and secure-envelope key provider primitives |
| `nenjo-platform` | Platform-backed manifest contracts, REST client, MCP tool contract, local backend, access policy helpers |
| `nenjo-sessions` | Shared session contracts for runtime, worker, and future session services |
| `nenjo-worker` | Platform worker implementation behind `nenjo run` |
| `nenjo-cli` | CLI package in `bin/` that builds the `nenjo` binary |
| `nenjo-integration-tests` | Provider-backed integration tests in `testing/integrations/` |

---

## Separation Boundaries

Keep ownership clear. Do not solve boundary issues by adding broad cross-crate dependencies.

| Layer | Crates | Boundary |
|-------|--------|----------|
| Core SDK | `nenjo`, `nenjo-models`, `nenjo-xml` | Embeddable agent runtime and tool API. Should not depend on platform worker/runtime crates. |
| Platform contracts | `nenjo-events`, `nenjo-sessions` | Transport- and storage-neutral event/session types and traits. |
| Platform transport/security | `nenjo-eventbus`, `nenjo-secure-envelope`, `nenjo-crypto-auth` | Event delivery, secure envelopes, enrollment, and key access. |
| Manifest bridge | `nenjo-platform` | Platform REST/MCP manifest operations, local/platform synchronization, access-policy checks. |
| Worker runtime | `nenjo-worker` | Harness composition, event-loop wiring, handlers, runtime factories, worker-specific session implementations. |
| CLI | `nenjo-cli` in `bin/` | Argument parsing and process entrypoint; delegates runtime work to `nenjo-worker`. |

### Placement Rules

- Core agent behavior, manifests, prompt context, memory, and runner APIs belong in `crates/nenjo`.
- New LLM integrations belong in `crates/models`; worker factory wiring belongs in `crates/worker/src/providers/`.
- Tool trait/API changes belong in `crates/nenjo`; concrete runtime tools and worker-specific tool assembly belong in `crates/worker/src/tools/`.
- Event shape changes start in `crates/events`. Transport behavior belongs in `crates/eventbus`.
- Encrypted event wrapping and payload helpers belong in `crates/secure-envelope`; enrollment and key-provider state belongs in `crates/crypto-auth`.
- Platform REST routes, manifest MCP tool specs, bootstrap/write-through DTOs, and manifest access policy belong in `crates/platform`.
- Shared session traits and DTOs belong in `crates/sessions`; concrete worker implementations belong in `crates/worker/src/harness/session/`.
- CLI flag parsing belongs in `bin/src/main.rs` or worker-owned `RunArgs`; runtime behavior belongs in `crates/worker`.

---

## Build / Lint / Test Commands

### Using `just` (recommended)

```bash
just build           # Build entire workspace
just build-release   # Build release binary
just test            # Run all tests
just test-crate <x>  # Run tests for specific crate
just lint            # Clippy lints (fails on warnings)
just lint-fix        # Auto-fix clippy warnings
just fmt             # Format all code
just fmt-check       # Check formatting without modifying
just pr              # lint-fix + fmt before PR
just check           # Type-check without building
just run             # Run CLI
just dev             # Watch mode for development
```

### Single Test Commands

```bash
# Run a specific test function
cargo test -p nenjo --lib test_function_name

# Run tests in a specific file
cargo test -p nenjo --test agents

# Run tests for nenjo with output
cargo test -p nenjo --lib --test agents --test memory -- --nocapture

# Run integration tests (requires API key)
OPENROUTER_API_KEY=sk-or-... cargo test -p nenjo-integration-tests -- --nocapture
```

### Full CI Commands

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

---

## Code Style Guidelines

### Formatting (rustfmt.toml)

- **Edition:** 2024
- **Max width:** 100 characters
- **Field init shorthand:** enabled
- **Try shorthand:** enabled (`?` operator)

Run `cargo fmt --all` before committing.

### Clippy

- **MSRV:** 1.85
- **Toolchain:** 1.94.0
- All clippy warnings are treated as errors (`-D warnings` in CI)

Run `cargo clippy --workspace --all-targets -- -D warnings` before committing.

### Imports

Organize imports in this order, separated by blank lines:

```rust
// std library
use std::collections::HashSet;
use std::sync::Arc;

// External crates (alphabetical)
use anyhow::Result;
use tokio::sync::mpsc;
use tracing::debug;
use uuid::Uuid;

// Internal crates
use nenjo_models::ModelProvider;
use nenjo::Tool;

// Local modules
use crate::agents::builder::AgentBuilder;
use crate::config::AgentConfig;
```

**Rule:** Never use inline `use` statements in function bodies. All imports go at the top of the file.

### Section Separators

Use this pattern to divide code into sections:

```rust
// ---------------------------------------------------------------------------
// Factory traits
// ---------------------------------------------------------------------------
```

### Naming Conventions

| Item | Convention | Example |
|------|------------|---------|
| Modules | snake_case | `memory/markdown.rs` |
| Structs | PascalCase | `AgentBuilder`, `TurnOutput` |
| Enum variants | PascalCase | `TaskType::Chat`, `AgentError::Execution` |
| Functions | snake_case | `build_agent()`, `resolve_model()` |
| Variables | snake_case | `model_factory`, `agent_config` |
| Constants | SCREAMING_SNAKE | `MAX_DEPTH`, `DEFAULT_TIMEOUT` |
| Type aliases | PascalCase | `ContextRenderer`, `MemoryScope` |
| Manifest types | Type-first | `AgentManifest`, `ModelManifest` (not `ManifestAgent`) |

### Error Handling

Use `thiserror` for custom error enums and `anyhow::Result` for fallible operations:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("agent not found: {0}")]
    AgentNotFound(String),

    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}

pub async fn agent_by_name(&self, name: &str) -> Result<AgentBuilder, ProviderError> {
    // ...
}
```

### Struct and Enum Derives

Always include:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Foo { ... }
```

Use `#[serde(default)]` on optional struct fields that should default when deserializing.

### Documentation

- Add module-level doc comments (`//!`) explaining purpose
- Document public API with doc comments (`///`)
- Do not add inline comments explaining obvious code
- Prefer crate README updates when adding or moving crate-level responsibilities

### Async/Await

- Use `async-trait` for async trait methods
- Always use `#[async_trait::async_trait]` on trait implementations

### Traits for Dependency Injection

```rust
pub trait ModelProviderFactory: Send + Sync {
    fn create(&self, provider_name: &str) -> Result<Arc<dyn ModelProvider>>;
}

#[async_trait::async_trait]
pub trait ToolFactory: Send + Sync {
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>>;
}
```

### Clone Patterns

- Use `Arc<T>` for shared ownership
- Use `.clone()` freely on Arcs
- Tools are `Vec<Arc<dyn Tool>>` to enable sharing between parent and sub-executions
- `AgentInstance` derives `Clone` for domain expansion and ability sub-executions

### Tests

- Place tests in `#[cfg(test)]` modules within source files
- Integration tests go in `tests/` directories per crate
- Use `#[tokio::test]` for async tests
- Mock implementations for testing should be defined within the test module

---

## Architecture Patterns

### Provider Builder Pattern

```rust
let provider = Provider::builder()
    .with_loader(my_manifest_loader)    // ManifestLoader
    .with_model_factory(my_factory)     // ModelProviderFactory
    .with_tool_factory(my_tool_factory) // ToolFactory
    .with_memory(memory)                // Optional Memory backend
    .build()
    .await?;

let runner = provider.agent_by_name("coder")?.build();
let output = runner.chat("task").await?;
```

### Turn Loop

The core LLM loop in `agents/runner/turn_loop.rs` handles:

- Provider calls
- Tool call parsing and execution
- Context compaction
- Parallel tool execution

### Memory System

- `Memory` trait defines the interface
- `MarkdownMemory` is the file-based backend
- Three-tier scoping: project, core, shared
- Memory tools auto-added when memory is configured

### Context Blocks

User-customizable prompt sections. All context blocks use `{{template_var}}` templates:

- `memory`, `memory_profile`, `agents`, `routines`, `skills`, `abilities`
- `domains`, `project`, `task`, `gate`, `cron`, `MCP`

### Platform Worker Flow

`nenjo run` starts in `bin/src/main.rs`, then delegates to `nenjo-worker`. The worker composes:

- `nenjo-eventbus` for raw event transport
- `nenjo-secure-envelope` and `nenjo-crypto-auth` for encrypted event/content handling
- `nenjo-platform` for manifest bootstrap, REST-backed project APIs, and manifest MCP operations
- `nenjo-sessions` traits with worker-local session store/coordinator/content implementations
- `nenjo` provider/runner APIs for actual agent execution

---

## Key Files

| Path | Purpose |
|------|---------|
| `crates/nenjo/src/provider/mod.rs` | Provider + factory traits |
| `crates/nenjo/src/agents/runner/mod.rs` | AgentRunner execution API |
| `crates/nenjo/src/agents/runner/turn_loop.rs` | Core LLM loop |
| `crates/nenjo/src/manifest.rs` | Manifest types and loader traits |
| `crates/nenjo/src/memory/mod.rs` | Memory trait and backends |
| `crates/models/src/lib.rs` | Model provider trait and provider exports |
| `crates/nenjo/src/tools.rs` | Tool trait/types used by the SDK |
| `crates/events/src/capability.rs` | Capability enum for multi-worker routing |
| `crates/eventbus/src/lib.rs` | Event bus abstraction and transport traits |
| `crates/eventbus/src/nats.rs` | NATS JetStream transport implementation |
| `crates/secure-envelope/src/lib.rs` | Secure event envelope layer and encrypted content helpers |
| `crates/crypto-auth/src/lib.rs` | Worker enrollment and envelope key provider exports |
| `crates/platform/src/lib.rs` | Platform manifest client, MCP contract, backend, and policy exports |
| `crates/platform/src/client.rs` | Platform REST client |
| `crates/platform/src/manifest_mcp/` | Manifest MCP tool specs, params, results, and backend traits |
| `crates/sessions/src/lib.rs` | Shared session contracts |
| `crates/worker/src/lib.rs` | Worker runtime entry exports |
| `crates/worker/src/handlers/` | Platform event handlers |
| `crates/worker/src/providers/` | Worker model provider registry/factory |
| `crates/worker/src/tools/` | Worker concrete runtime tools and tool assembly |
| `bin/src/main.rs` | CLI entrypoint for the `nenjo` binary |

---

## Common Tasks

### Adding a New LLM Provider

1. Implement the `ModelProvider` trait in `crates/models/src/`.
2. Export the provider from `crates/models/src/lib.rs`.
3. Add worker factory wiring in `crates/worker/src/providers/`.
4. Add focused unit tests in `crates/models` and worker wiring tests when practical.

### Adding a Built-in Tool

1. Add or update the SDK tool API in `crates/nenjo/src/tools.rs` only when the shared trait or types need to change.
2. Implement concrete runtime tool behavior in `crates/worker/src/tools/`.
3. Add worker-specific assembly or gating in `crates/worker/src/tools/mod.rs`.
4. Keep platform REST-backed tool specs in `crates/platform/src/rest/` when the tool is a platform API wrapper.

### Adding a Platform Event

1. Add or update typed event contracts in `crates/events`.
2. Update transport or envelope behavior only if the wire handling changes.
3. Add worker handling in `crates/worker/src/handlers/`.
4. Keep SDK execution changes in `crates/nenjo` if the event ultimately invokes agent behavior.

### Adding a Platform Manifest Operation

1. Add REST DTO/client behavior in `crates/platform/src/client.rs` or a focused platform module.
2. Add MCP params/results/tool specs in `crates/platform/src/manifest_mcp/` when the operation is tool-visible.
3. Implement local/platform backend behavior in `crates/platform`.
4. Wire worker usage in `crates/worker` only after the platform crate exposes the reusable operation.

### Adding a Context Block

1. Add the field to `PromptContext` in `crates/nenjo/src/agents/prompts.rs`.
2. Add `{{context_block_name}}` to the appropriate prompt config/template.
3. Populate the field in `Provider::build_prompt_context()`.
4. Update platform manifest docs or DTOs only if the context block has a platform-managed manifest surface.
