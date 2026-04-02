# AGENTS.md — Nenjo SDK Development Guide

This file provides guidance for agentic coding agents operating in this repository.

## Project Overview

Rust monorepo with the following crates:

| Crate | Description |
|-------|-------------|
| `nenjo` | Core SDK — agent turn loop, provider, memory, abilities, domains |
| `nenjo-models` | LLM provider trait + implementations (OpenAI, Anthropic, Gemini, etc.) |
| `nenjo-tools` | Tool trait + built-in tools (shell, file, git, search, web, memory) |
| `nenjo-xml` | XML template engine for structured prompt context |
| `nenjo-events` | NATS event types for harness ↔ backend messaging |
| `nenjo-eventbus` | Transport-agnostic event bus with NATS JetStream support |
| `harness` | Platform orchestration (NATS, routing, routines, cron, worker) |
| `runner` | CLI runner binary |

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
use std::sync::Arc;
use std::collections::HashSet;

// External crates (alphabetical)
use anyhow::Result;
use tokio::sync::mpsc;
use tracing::debug;
use uuid::Uuid;

// Internal crates
use nenjo_models::ModelProvider;
use nenjo_tools::Tool;

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
// Error enum with thiserror
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("agent not found: {0}")]
    AgentNotFound(String),

    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}

// Fallible operations use anyhow::Result
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
- **Do NOT add inline comments** explaining obvious code — only add comments when the intent is genuinely unclear

### Async/Await

- Use `async-trait` for async trait methods
- Always use `#[async_trait::async_trait]` on trait implementations

### Traits for Dependency Injection

```rust
// Send + Sync bounds for shared trait objects
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
- Use `.clone()` freely on Arcs (cheap)
- Tools are `Vec<Arc<dyn Tool>>` to enable sharing between parent and sub-executions
- `AgentInstance` derives `Clone` for domain_expansion and ability sub-executions

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
    .with_model_factory(my_factory)    // ModelProviderFactory
    .with_tool_factory(my_tool_factory) // ToolFactory
    .with_memory(memory)               // Optional Memory backend
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

---

## Key Files

| Path | Purpose |
|------|---------|
| `crates/nenjo/src/provider/mod.rs` | Provider + factory traits |
| `crates/nenjo/src/agents/runner/mod.rs` | AgentRunner execution API |
| `crates/nenjo/src/agents/runner/turn_loop.rs` | Core LLM loop |
| `crates/nenjo/src/manifest.rs` | Manifest types and loader |
| `crates/nenjo/src/memory/mod.rs` | Memory trait and backends |
| `crates/events/src/capability.rs` | Capability enum for multi-worker |
| `crates/eventbus/src/nats.rs` | NATS transport implementation |

---

## Common Tasks

### Adding a New LLM Provider

1. Add crate to `Cargo.toml` workspace members
2. Implement `ModelProvider` trait in `crates/models/src/`
3. Add provider name to `crates/models/src/lib.rs` exports
4. Update factory in harness

### Adding a Built-in Tool

1. Implement `Tool` trait in `crates/tools/src/`
2. Export in `crates/tools/src/lib.rs`
3. Add to `ToolFactory` implementation in harness

### Adding a Context Block

1. Add field to `PromptContext` in `agents/prompts.rs`
2. Add `{{context_block_name}}` template to agent prompt config
3. Populate the field in `Provider::build_prompt_context()`
