# Knowledge Pack Prompt Selection Catalog

## Purpose

Catalog how knowledge packs become selectable from prompts through template
variables, and how those prompt variables relate to the pack selector passed to
knowledge tools.

Prompt injection here means template expansion such as:

```text
{{ pkg.nenjo.core.knowledge }}
{{ lib.product_docs }}
```

The injected value is not the full knowledge content. It is compact XML
metadata: pack summary, usage guidance, document index entries, and compact
outbound edge hints. Agents still use knowledge tools to search, inspect
metadata, traverse graph neighbors, and read selected documents.

## Runtime Flow

1. Worker builds the provider.
2. Provider registers knowledge packs through `with_knowledge_packs`.
3. Each registered pack has a stable `KnowledgeRef`.
4. `knowledge_pack_prompt_vars(knowledge_ref, pack)` derives prompt variable keys.
5. The derived variables are merged into `RenderContextVars.knowledge_vars`.
6. Prompt templates render those variables with MiniJinja.
7. Agent uses the visible selector from the injected XML when calling knowledge
   tools.

Primary implementation points:

- `crates/worker/src/assembly.rs`: registers platform and package knowledge packs.
- `crates/nenjo/src/provider/builder.rs`: merges pack prompt variables.
- `crates/knowledge/src/tools.rs`: derives prompt variable names and values.
- `crates/xml/src/template.rs`: renders dotted prompt variables.

## Registered Pack Sources

### Nenjo Package Knowledge

Package knowledge is registered under the `pkg` namespace regardless of whether
the package was installed from a registry, git source, or local override:

```text
selector = pkg:nenjo.core:knowledge
```

Prompt variable:

```text
{{ pkg.nenjo.core.knowledge }}
```

Tool selector:

```json
{ "pack": "pkg:nenjo.core:knowledge" }
```

### Platform Uploaded Packs

Worker syncs platform-uploaded packs into:

```text
<workspace>/.nenjo/library/platform/<slug>
```

Worker registers each pack as:

```text
selector = lib:<slug>
```

Prompt variable:

```text
{{ lib.<normalized-slug> }}
```

Examples:

```text
lib:product-docs  -> {{ lib.product_docs }}
```

Tool selector:

```json
{ "pack": "lib:product-docs" }
```

There is no bare `lib` selector. Use the explicit pack slug.

## Document-Level Prompt Variables

For each pack, Nenjo also derives document metadata variables below the pack
variable.

The general form is:

```text
{{ <pack-var>.<doc-path-without-md> }}
```

Examples:

```text
{{ pkg.nenjo.core.knowledge.reference.template_vars }}
{{ lib.product_docs.guides.agents }}
```

Document variables inject compact `<knowledge_doc>` metadata, not full document
content. Full content still requires `read_knowledge_doc`.

Pack-level index entries include compact outbound relationships:

```xml
<related type="depends_on" target="library://product-docs/setup.md"/>
```

Relationship notes and full target metadata are intentionally omitted from the
prompt index. Use `list_knowledge_neighbors` to expand graph neighbors.

Document path segments are normalized:

- lowercase
- non-alphanumeric runs become `_`
- leading and trailing `_` are trimmed
- `.md` suffix is removed

## Selection Semantics

Prompt variables select knowledge by exposing an index and the canonical pack
selector to the agent. They do not bind tool calls automatically.

Correct agent pattern:

1. Prompt includes the relevant pack index, such as `{{ pkg.nenjo.core.knowledge }}`.
2. Agent sees the pack selector and document summaries in rendered XML.
3. Agent calls `search_knowledge` with the selector to get candidate document
   metadata.
4. Agent optionally calls `list_knowledge_neighbors` to follow outbound graph
   edges.
5. Agent calls `read_knowledge_doc` for final source documents.

Example:

```json
{
  "pack": "pkg:nenjo.core:knowledge",
  "query": "agent abilities and MCP assignment"
}
```

## Canonical Selector To Prompt Variable Mapping

| Pack selector | Prompt variable | Status |
| --- | --- | --- |
| `lib:<slug>` | `{{ lib.<slug> }}` | Implemented |
| `pkg:<pkg>:knowledge` | `{{ pkg.<pkg>.knowledge }}` | Implemented |
| `local:<slug>` | `{{ local.<slug> }}` | Planned local directory namespace |

## Guardrails

- Do not inject full document bodies through prompt variables.
- `pkg.*` is a prompt/template namespace, not a general resource import
  namespace. In v1 it is used for package-installed context blocks and package
  knowledge variables.
- Agents, abilities, domains, routines, MCP servers, and other runtime
  resources are resolved through package modules/imports and their installed
  manifests, not by writing `{{ pkg.* }}` prompt selectors for those resources.
- Package-authored prompts should use `pkg.*` only when referencing packaged
  context blocks such as `{{ pkg.nenjo.core.methodology }}` or packaged
  knowledge such as `{{ pkg.nenjo.core.knowledge }}`.
- Treat prompt variables as retrieval hints, not authority. Tool calls must use
  the canonical pack selector.
- Prefer metadata-first retrieval before reading full documents.

## Test Coverage

`KnowledgeRef` has coverage for:

- `lib:<slug>`
- `pkg:<pkg>:knowledge`
- `local:<slug>`
