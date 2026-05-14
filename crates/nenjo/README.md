# nenjo

Agent orchestration SDK for building agentic AI workflows with tool use, memory, and multi-agent delegation.

## Features

- **Provider-based architecture** — pluggable LLM providers, tool factories, and memory backends
- **Turn loop engine** — automatic tool call execution, context compaction, and streaming
- **Multi-agent delegation** — agents delegate subtasks to other agents with cycle detection and depth limiting
- **Knowledge packs** — provider-level document packs with reusable search, read, tree, and graph-neighbor tools
- **Persistent memory** — 3-tier scoped memory (project, core, shared) with automatic prompt injection
- **Abilities & domains** — structured sub-execution modes and domain-specific interaction sessions
- **Routine orchestration** — DAG-based step execution with gates, councils, and cron scheduling
- **Streaming API** — real-time event streaming via channels for responsive UIs

## Quick start

```rust
use nenjo::Provider;

let provider = Provider::builder()
    .with_loader(my_manifest_loader)
    .with_model_factory(my_model_factory)
    .with_tool_factory(my_tool_factory)
    .with_knowledge_pack("docs:app", my_knowledge_pack)
    .build()
    .await?;

let runner = provider
    .agent_by_name("coder")
    .await?
    .build()
    .await?;

// Simple API
let output = runner.chat("Hello").await?;

// Streaming API
let mut handle = runner.chat_stream("Hello").await?;
while let Some(event) = handle.recv().await {
    // Process TurnEvent variants
}
let output = handle.output().await?;
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
