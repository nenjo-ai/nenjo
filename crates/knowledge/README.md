# nenjo-knowledge

Knowledge pack primitives and reusable knowledge tools for Nenjo agents.

This crate is intentionally separate from the core `nenjo` SDK. It owns the shared
metadata/search/read contracts for knowledge packs, plus `nenjo-tool-api` backed tools
that expose packs to agents through a consistent interface.

## Features

- `default` - generic knowledge pack types and reusable knowledge tools.
- `nenjo` - embeds the built-in Nenjo documentation pack and exposes
  `nenjo_knowledge::builtin::nenjo_pack()`.

## Provider Integration

Register packs at provider construction time:

```rust
let provider = nenjo::Provider::builder()
    .with_loader(loader)
    .with_model_factory(model_factory)
    .with_knowledge_pack("docs:app", app_docs)
    .build()
    .await?;
```

For multiple concrete pack types, use `KnowledgePackEntry`:

```rust
use nenjo_knowledge::tools::KnowledgePackEntry;

let provider = nenjo::Provider::builder()
    .with_loader(loader)
    .with_model_factory(model_factory)
    .with_knowledge_packs([
        KnowledgePackEntry::new("docs:app", app_docs),
        KnowledgePackEntry::new("docs:runbook", runbook_docs),
    ])
    .build()
    .await?;
```

Registered packs automatically add the generic knowledge tools and prompt
metadata variables for all agents built by the provider.
