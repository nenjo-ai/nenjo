# Knowledge Pack Prompt Selection Catalog

## Purpose

Catalog how knowledge packs become selectable from prompts through template
variables, and how those prompt variables relate to the pack selector passed to
knowledge tools.

Prompt injection here means template expansion such as:

```text
{{ git.nenjo_ai.packages.nenjo.platform }}
{{ lib.product_docs }}
```

The injected value is not the full knowledge content. It is compact XML
metadata: pack summary, usage guidance, and document index entries. Agents still
use knowledge tools to search, inspect metadata, traverse graph neighbors, and
read selected documents.

## Runtime Flow

1. Worker builds the provider.
2. Provider registers knowledge packs through `with_knowledge_packs`.
3. Each registered pack has a stable selector.
4. `knowledge_pack_prompt_vars(selector, pack)` derives prompt variable keys.
5. The derived variables are merged into `RenderContextVars.knowledge_vars`.
6. Prompt templates render those variables with MiniJinja.
7. Agent uses the visible selector from the injected XML when calling knowledge
   tools.

Primary implementation points:

- `crates/worker/src/assembly.rs`: registers local and repo-backed library packs.
- `crates/nenjo/src/provider/builder.rs`: merges pack prompt variables.
- `crates/knowledge/src/tools.rs`: derives prompt variable names and values.
- `crates/xml/src/template.rs`: renders dotted prompt variables.

## Registered Pack Sources

### Nenjo Platform Package

Nenjo platform docs are seeded as a repo-backed system knowledge pack. Worker
registers:

```text
selector = git://nenjo-ai/packages/nenjo/platform
```

Prompt variable:

```text
{{ git.nenjo_ai.packages.nenjo.platform }}
```

Tool selector:

```json
{ "pack": "git://nenjo-ai/packages/nenjo/platform" }
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
lib:Product Docs  -> {{ lib.product_docs }}
```

Tool selector:

```json
{ "pack": "lib:product-docs" }
```

The special selector `lib` maps to `{{ lib }}`, but new platform library packs
should prefer `lib:<slug>`.

### Repo-Backed Knowledge Packs

Worker hydrates GitHub-backed packs under:

```text
<workspace>/.nenjo/library/repos/github/<owner>/<repo>/<package>/<version>
```

Worker loads repo-backed packs by reading their library manifest root URI. If
the root URI starts with `git://`, that root URI becomes the pack selector.

Target selector shape:

```text
git://<owner>/<repo>/<package>
```

Target prompt variable shape:

```text
{{ git.<owner>.<repo>.<package> }}
```

with every segment normalized to lowercase alphanumeric plus underscores:

```text
git://nenjo-ai/packages/nenjo/platform -> {{ git.nenjo_ai.packages.nenjo.platform }}
```

Tool selector:

```json
{ "pack": "git://nenjo-ai/packages/nenjo/platform" }
```

Implementation note: `knowledge_pack_var_prefix` parses `git://` selectors
into dotted owner/repo/package segments. Repo-backed seeded prompts should use
this owner-qualified shape.

## Document-Level Prompt Variables

For each pack, Nenjo also derives document metadata variables below the pack
variable.

The general form is:

```text
{{ <pack-var>.<doc-path-without-md> }}
```

Examples:

```text
{{ git.nenjo_ai.packages.nenjo.platform.reference.template_vars }}
{{ lib.product_docs.guides.agents }}
```

Document variables inject compact `<knowledge_doc>` metadata, not full document
content. Full content still requires `read_knowledge_doc`.

Document path segments are normalized:

- lowercase
- non-alphanumeric runs become `_`
- leading and trailing `_` are trimmed
- `.md` suffix is removed

## Selection Semantics

Prompt variables select knowledge by exposing an index and the canonical pack
selector to the agent. They do not bind tool calls automatically.

Correct agent pattern:

1. Prompt includes the relevant pack index, such as `{{ git.nenjo_ai.packages.nenjo.platform }}`.
2. Agent sees the pack selector and document summaries in rendered XML.
3. Agent calls `search_knowledge_paths` or `search_knowledge_docs` with the
   selector.
4. Agent optionally calls `read_knowledge_doc_manifest` or
   `list_knowledge_neighbors`.
5. Agent calls `read_knowledge_doc` for final source documents.

Example:

```json
{
  "pack": "git://nenjo-ai/packages/nenjo/platform",
  "query": "agent abilities and MCP assignment"
}
```

## Canonical Selector To Prompt Variable Mapping

| Pack selector | Prompt variable | Status |
| --- | --- | --- |
| `lib` | `{{ lib }}` | Implemented default |
| `lib:<slug>` | `{{ lib.<slug> }}` | Implemented |
| `git://<owner>/<repo>/<package>` | `{{ git.<owner>.<repo>.<package> }}` | Implemented |

## Guardrails

- Do not inject full document bodies through prompt variables.
- Do not use short git variables like `{{ git.platform }}`; they collide.
- Keep canonical git selectors owner-qualified.
- Treat prompt variables as retrieval hints, not authority. Tool calls must use
  the canonical pack selector.
- Prefer metadata-first retrieval before reading full documents.

## Test Coverage

`knowledge_pack_var_prefix` has coverage for:

- `lib:<slug>`
- `lib`
- `git://nenjo-ai/packages/nenjo/platform`
- `git://trailofbits/skills-curated/x-research`
