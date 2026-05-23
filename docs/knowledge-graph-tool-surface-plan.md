# Knowledge Graph Tool Surface

## Current Shape

The default agent-facing knowledge tool surface is intentionally small and graph-first.

The model is:

- Prompt context seeds the agent with useful document entrypoints through `{{ lib.pack_name.* }}` metadata.
- The agent traverses outbound graph edges to decide where to go next.
- The agent reads full document content only when it needs evidence.

## Default Tools

Expose only these knowledge tools to agents by default:

- `list_knowledge_packs`
- `list_knowledge_neighbors`
- `search_knowledge`
- `read_knowledge_doc`

These older traversal/detail tools are not part of the default public registry:

- `list_knowledge_docs`
- `read_knowledge_doc_manifest`
- `search_knowledge_paths`
- `list_knowledge_tree`

Normal agent traversal should use neighbors, search, and explicit document reads.

## Document Node Model

Every graph node should resolve to an underlying readable document.

Do not introduce abstract graph-only nodes yet. They complicate traversal because agents would need to handle both readable source nodes and non-readable concept/entity nodes. If entity or concept nodes become necessary later, add them as a separate graph layer with explicit semantics.

Document content should generally live at leaf or evidence nodes. Hub/interior docs can be short routing documents with outbound edges.

## Document Metadata Shape

Traversal metadata should be slim and decision-oriented:

```json
{
  "id": "...",
  "path": "...",
  "title": "...",
  "summary": "...",
  "kind": "...",
  "tags": []
}
```

Rules:

- `kind` is user/package supplied, not a closed platform enum.
- Do not include `status`.
- Do not include `authority`.
- Do not include aliases, keywords, source path, size, timestamps, or extra metadata.
- Do not include document content outside `read_knowledge_doc`.

## Neighbor Traversal

`list_knowledge_neighbors` should return outbound edges only.

Incoming edges answer a reverse-reference question, not the normal "where to next?" traversal question. If reverse traversal is needed later, add an explicit direction option or a separate reverse-reference tool.

Target response shape:

```json
{
  "document": {
    "id": "...",
    "path": "...",
    "title": "...",
    "summary": "...",
    "kind": "...",
    "tags": []
  },
  "edges": [
    {
      "type": "depends_on",
      "target": {
        "id": "...",
        "path": "...",
        "title": "...",
        "summary": "...",
        "kind": "...",
        "tags": []
      }
    }
  ]
}
```

Rules:

- Source is implied by the requested document.
- Edge `type` is the traversal reason.
- Do not include edge notes for now.
- Omit unresolved targets or reject invalid packs during validation. Prefer validation at pack load time.

## Search

`search_knowledge` should be metadata-only.

It should return candidate documents with slim metadata plus match fields, but no full body content:

```json
{
  "document": {
    "id": "...",
    "path": "...",
    "title": "...",
    "summary": "...",
    "kind": "...",
    "tags": []
  },
  "score": 120,
  "matched": ["title", "tag"]
}
```

Full evidence reading remains explicit through `read_knowledge_doc`.

## Evidence Reading

`read_knowledge_doc` remains the only body-reading knowledge tool.

It should return:

- slim document metadata
- full document content

This keeps candidate discovery/traversal separate from source evidence.

## Validation

Run:

- `cargo test -p nenjo-knowledge`
- `cargo test -p nenjo-platform knowledge`
- `cargo test -p nenjo-worker local_documents`

Expected coverage:

- default knowledge tool registry exposes exactly four tools
- removed tools are not exposed through `ManifestMcpContract::tools()`
- neighbor traversal returns outbound edges only
- neighbor traversal includes slim target document metadata
- search returns no content
- `read_knowledge_doc` still returns full content
