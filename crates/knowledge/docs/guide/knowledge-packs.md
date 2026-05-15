# Knowledge Packs

## Purpose

Knowledge packs are reusable document sets exposed to agents through one
metadata, search, graph, and read contract. They cover built-in Nenjo docs,
project document manifests, filesystem-backed documentation, and future remote
document sets.

Use this guide when designing project knowledge, registering SDK knowledge
packs, or explaining the generic knowledge tools available to agents.

## Core Model

A knowledge pack has:

- a stable pack selector, such as `builtin:nenjo` or a project-specific
  selector;
- pack metadata: `pack_id`, `pack_version`, `schema_version`, `root_uri`, and
  `content_hash`;
- document manifests with id, virtual path, source path, title, summary,
  description, kind, authority, status, tags, aliases, keywords, and graph
  edges;
- lazy document content that is read only after a doc has been selected.

Document graph edges use canonical relationship types:

- `part_of`
- `defines`
- `governs`
- `classifies`
- `references`
- `depends_on`
- `extends`
- `related_to`

## Generic Knowledge Tools

Registered packs expose a consistent set of read-only tools:

- `list_knowledge_packs` lists available packs and document counts.
- `list_knowledge_tree` lists paths under a pack root or prefix.
- `search_knowledge_paths` searches compact metadata and returns no body
  content.
- `search_knowledge_docs` searches and may return matched body content.
- `read_knowledge_doc_manifest` reads metadata for one document by id, virtual
  path, or source path.
- `list_knowledge_neighbors` traverses incoming and outgoing graph edges for a
  selected document.
- `read_knowledge_doc` reads the selected full document content.

Agents should prefer metadata-first retrieval. Search paths or read manifests
before reading full documents, and use neighbors when the user asks how concepts
relate.

## Built-In Nenjo Pack

The built-in Nenjo pack is selected with `pack="builtin:nenjo"` and is exposed
in prompts through `{{ builtin.nenjo }}`. The prompt variable is an index and
usage hint, not a full documentation dump.

Good retrieval pattern:

1. Use `search_knowledge_paths` to find seed docs.
2. Use `read_knowledge_doc_manifest` to inspect compact metadata.
3. Use `list_knowledge_neighbors` when concepts are connected.
4. Use `read_knowledge_doc` for the final source documents.
5. Answer from the docs that were actually read.

## Project Knowledge Packs

Library knowledge syncs into `manifest.json` inside the library pack
directory. The manifest is a library knowledge pack with a `library://.../`
root URI and the same item metadata schema as built-in packs.

Library knowledge is distinct from memory:

- library knowledge is explicit source material managed as items;
- memory is learned agent context;
- artifacts are saved files or generated outputs;
- prompt variables and tools should keep these categories separate.

When project documents change, the worker updates the project knowledge
manifest and graph edges. Agents should use `{{ project.documents }}` as a
compact index, then read selected project documents through available project or
knowledge tools when they need source detail.

## SDK Registration

SDK users register packs while building a provider:

```rust
use nenjo_knowledge::tools::KnowledgePackEntry;

let provider = nenjo::Provider::builder()
    .with_loader(loader)
    .with_model_factory(model_factory)
    .with_knowledge_packs([KnowledgePackEntry::new("docs:app", app_docs)])
    .build()
    .await?;
```

For multiple packs, pass multiple `KnowledgePackEntry` values.

Registered packs add prompt metadata variables and the generic knowledge tools
for agents built by the provider.

## Agent Guidance

Use knowledge packs for explicit documentation and graph retrieval. Do not use
them as a replacement for memory, tool results, or task state. When a user asks
for a design recommendation, retrieve enough docs to ground the answer and name
which documents informed the result.
