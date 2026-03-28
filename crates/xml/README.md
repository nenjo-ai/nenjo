# nenjo-xml

XML serialization and template rendering for structured LLM prompt context. Used by both the **backend** (prompt preview in the UI) and the **worker** (runtime prompt generation) to produce identical output from the same templates and data.

## Motivation

LLM agents need structured prompts. Nenjo prompts are XML-heavy — context blocks, tool descriptions, task metadata, and agent instructions are all serialized as XML fragments and composed into final prompt strings. This crate exists to:

1. **Guarantee parity between preview and execution.** The backend renders a prompt preview in the UI so users can see exactly what the LLM will receive. The worker renders the same prompt at execution time. Both import `nenjo-prompts` and call the same `render_template()` with the same `RenderContext`. One crate, one output.

2. **Decouple prompt rendering from agent internals.** The crate defines minimal render-only types (`RenderAgent`, `RenderRoutine`, etc.) that carry only the fields needed for rendering. Both the backend and worker construct these from their own internal types. No database models, no runtime state — just data in, XML/text out.

3. **Make context blocks user-customizable.** Every context block (agents, routines, skills, memory, project, task, etc.) has a template stored in the database. Users can edit the template that wraps the rendered data. The `ContextRenderer` applies `{{items}}` substitution and MiniJinja rendering so users control what the LLM sees.

4. **Provide a safe, ergonomic XML API.** Prompts contain a lot of XML. Hand-writing XML strings is error-prone (unescaped `<`, `&`, `"`). The `XmlBuilder` and `XmlTag` APIs handle escaping automatically while staying readable.

## Architecture

```
                  ┌──────────────────────────────────────────┐
                  │              nenjo-prompts                │
                  │                                          │
                  │  xml/          Template engine + XML     │
                  │  ├─ builder    XmlBuilder (fluent API)   │
                  │  ├─ tag        XmlTag (struct-based)     │
                  │  ├─ traits     ToXml                      │
                  │  ├─ escape     Entity escaping           │
                  │  ├─ parse      Tag extraction            │
                  │  └─ error      XmlError                  │
                  │                                          │
                  │  types         RenderContext + Render*    │
                  │  template      MiniJinja rendering       │
                  │  context/      Context block system      │
                  │  ├─ renderer   ContextRenderer           │
                  │  └─ structs    ProjectContext, TaskCtx…   │
                  └──────────────────────────────────────────┘
                         ▲                    ▲
                         │                    │
                    ┌────┘                    └────┐
                    │                              │
              ┌─────────┐                   ┌──────────┐
              │ backend  │                   │  worker   │
              │ (preview)│                   │ (runtime) │
              └─────────┘                   └──────────┘
```

Three-layer composition:

1. **Render types** — Minimal structs (`RenderAgent`, `RenderSkill`, etc.) that implement `ToXml`. These are the raw data.
2. **Context structs** — Typed projections (`ProjectContext`, `TaskContext`, `GateContext`) that extract subsets of `RenderContext` and render them as structured XML.
3. **ContextRenderer** — Applies database-stored templates with `{{items}}` markers and MiniJinja variables to produce the final context block strings.

The output of layer 3 goes into `RenderContext.context_blocks`, which is then consumed by `render_template()` for the final prompt string.

## Quick Start

### Rendering a template with variables

```rust
use nenjo_prompts::{RenderContext, template::render_template};

let mut ctx = RenderContext::default();
ctx.agent_name = "coder".into();
ctx.task_title = "Fix login bug".into();
ctx.project_name = "MyApp".into();

let template = r#"
You are {{ agent.name }}, working on {{ project.name }}.

{% if task.title %}
Current task: {{ task.title }}
{% endif %}
"#;

let prompt = render_template(template, &ctx);
// Output:
// You are coder, working on MyApp.
//
// Current task: Fix login bug
```

### Building XML with XmlBuilder

```rust
use nenjo_prompts::XmlBuilder;

let xml = XmlBuilder::pretty(2)
    .tag("task")
        .attr("id", "TASK-42")
        .attr("status", "open")
        .tag("title").content("Fix SSO login").close()
        .tag("description").content("Users can't log in via SAML.").close()
    .close()
    .build();

// <task id="TASK-42" status="open">
//   <title>Fix SSO login</title>
//   <description>Users can&apos;t log in via SAML.</description>
// </task>
```

### Quick XML helpers

```rust
use nenjo_prompts::{tag, content, wrap};

// Escaped text content
let name = tag("name", "Alice & Bob");
// <name>Alice &amp; Bob</name>

// Raw XML injection (no escaping)
let inner = tag("child", "value");
let outer = content("parent", &inner);
// <parent><child>value</child></parent>

// Alias for tag()
let ctx = wrap("context", "You are helpful.");
// <context>You are helpful.</context>
```

### Implementing ToXml for custom types

```rust
use nenjo_prompts::{ToXml, XmlBuilder};

struct CodeFile {
    path: String,
    language: String,
}

impl ToXml for CodeFile {
    fn to_xml(&self) -> String {
        XmlBuilder::new()
            .tag("file")
                .attr("path", &self.path)
                .attr("lang", &self.language)
            .close()
            .build()
    }
}
```

### Rendering context blocks

```rust
use nenjo_prompts::context::renderer::{ContextRenderer, ContextData};
use nenjo_prompts::types::{RenderContextBlock, RenderAgent, RenderContext};

// Context block templates come from the database
let blocks = vec![
    RenderContextBlock {
        name: "available_agents".into(),
        path: "nenjo".into(),
        template: "<available_agents>\n{{items}}\n</available_agents>".into(),
    },
];

let renderer = ContextRenderer::from_blocks(&blocks);
let rendered_map = renderer.render_all(&context_data);
// Returns: {"nenjo.available_agents" => "<available_agents>\n  <agent .../>...\n</available_agents>"}

// Merge into RenderContext for template rendering
let mut ctx = RenderContext::default();
ctx.context_blocks = rendered_map;

// Access in templates:
// {{ context.nenjo.available_agents }}  → the rendered block
// {{ context.nenjo }}                   → all nenjo.* blocks concatenated
```

## Detailed Behavior

### Template Engine (MiniJinja)

The template engine uses [MiniJinja](https://docs.rs/minijinja) (Jinja2-compatible) with two important settings:

- **Auto-escaping is disabled.** Prompts contain raw XML that must be preserved verbatim. Template variables that contain XML (like context blocks) are injected as-is.
- **Undefined variables render as empty strings.** Templates can reference variables that may not be populated for a given execution type (e.g., `{{ gate.criteria }}` is empty for non-gate tasks). Instead of erroring, undefined variables silently produce `""`. MiniJinja's `Chainable` undefined behavior means `{{ context.some.deep.path }}` also works safely.

**Variable namespaces:**

| Namespace | Variables |
|-----------|-----------|
| `task.*` | `id`, `title`, `description`, `acceptance_criteria`, `tags`, `source`, `status`, `priority`, `type`, `slug`, `complexity` |
| `agent.*` | `id`, `name`, `model`, `description` |
| `project.*` | `id`, `name`, `description`, `working_dir`, `metadata` |
| `routine.*` | `name`, `step` |
| `step.*` | `metadata` |
| `git.*` | `branch`, `target_branch`, `work_dir` |
| `global.*` | `timestamp`, `run_id` |
| `chat.*` | `message` |
| `gate.*` | `criteria`, `previous_output` |
| `subtask.*` | `parent_task`, `description` |
| `ability.*` | `name`, `prompt` |
| `context.*` | Context block tree (see below) |

### Context Block Tree

Context blocks are stored in `RenderContext.context_blocks` as a flat `HashMap<String, String>` with dotted keys (e.g., `"nenjo.available_agents"`). The template engine builds these into a nested tree:

```
context_blocks = {
    "nenjo.available_agents" => "<agents>...</agents>",
    "nenjo.memory"           => "<memory>...</memory>",
    "custom.coding.standards" => "Use snake_case.",
}
```

Produces this template tree:

```
context
├── nenjo
│   ├── available_agents  → "<agents>...</agents>"     (leaf)
│   └── memory            → "<memory>...</memory>"     (leaf)
└── custom
    └── coding
        └── standards     → "Use snake_case."           (leaf)
```

**Group rendering:** Interior nodes render as the concatenation of all their descendant leaf values, joined by `\n\n`. So `{{ context.nenjo }}` produces the concatenation of all `nenjo.*` blocks, and `{{ context.custom }}` produces all `custom.*` blocks.

### Context Renderer

The `ContextRenderer` handles 13 built-in block names with hardcoded rendering logic, plus arbitrary custom blocks that are rendered via MiniJinja:

| Built-in Block | Data Source | Behavior |
|----------------|-------------|----------|
| `available_agents` | `ContextData.agents` | Excludes current agent by ID. Filters agents with empty descriptions. |
| `available_abilities` | `ContextData.abilities` | Pretty-printed `<ability>` tags |
| `available_routines` | `ContextData.routines` | Only active routines (`is_active == true`) |
| `available_skills` | `ContextData.skills` | Filters skills with empty instructions |
| `available_domains` | `ContextData.domains` | Pretty-printed `<domain>` tags |
| `available_mcp_servers` | `ContextData.mcp_servers` + `platform_scopes` | `<mcp_integrations>` with server entries and platform scope |
| `current_project` | `RenderContext` fields + `documents_xml` | `<project>` with metadata, git context, documents |
| `current_task` | `RenderContext` task fields | `<task>` with all fields. Empty if `task_id` is empty. |
| `current_gate` | `RenderContext` gate fields | `<gate_evaluation>` with criteria + previous output |
| `current_cron` | `RenderContext` task + timestamp | `<cron_execution>` wrapping task XML |
| `memory` | `ContextData.memory_xml` (pre-loaded) | Raw XML passthrough |
| `memory_profile` | `ContextData.memory_*_focus` | `<core_focus>` and `<project_focus>` item lists |
| _(any other name)_ | Template + `RenderContext` | Custom block — rendered entirely by MiniJinja |

**Rendering pipeline for built-in blocks:**

1. Match block name to data source, render items as XML via `render_items()` or `ToXml`
2. If data is empty → return empty string (block is omitted from prompt)
3. If template contains `{{items}}` → substitute the rendered XML into the template
4. Run MiniJinja on the result (so templates can use `{% if %}`, `{{ project.name }}`, etc.)

**Dotted key construction:** A block with `path: "custom/coding"` and `name: "standards"` becomes `"custom.coding.standards"`. Slashes in paths are converted to dots.

### XML Module

Two parallel APIs for constructing XML, suited to different use cases:

**`XmlBuilder`** — Fluent/chainable builder. Best for procedural construction where tags are opened and closed in sequence:

```rust
XmlBuilder::new()
    .tag("root")
        .tag("child").attr("key", "val").content("text").close()
    .close()
    .build()
```

- `new()` → compact output, `pretty(indent_size)` → indented output
- `.content()` auto-escapes, `.raw()` does not
- `.build()` panics on unclosed tags, `.try_build()` returns `Result`
- Empty tags render as self-closing: `<br />`

**`XmlTag`** — Struct-based representation. Best for data-driven construction where you build a tree of tags:

```rust
XmlTag::new("root")
    .with_child(XmlTag::new("child").with_attr("key", "val").with_text_content("text"))
    .render_pretty(2)
```

- `render()` → compact, `render_pretty(indent_size)` → indented
- `with_text_content()` escapes, `with_raw_content()` does not
- Implements `Display` (compact rendering)
- `with_child()` appends incrementally; `with_children()` sets all at once

**Utilities:**

- `escape::xml_escape(s)` / `xml_unescape(s)` — Entity encoding (`&`, `<`, `>`, `"`, `'`)
- `parse::extract_tag_content(xml, tag)` — Extract text between `<tag>` and `</tag>`
- `parse::extract_attr(xml, tag, attr)` — Extract attribute value
- `parse::has_tag(xml, tag)` — Check if tag exists
- `parse::extract_all_tag_contents(xml, tag)` — All occurrences
- `parse::extract_raw_inner_xml(xml, tag)` — Inner XML without entity decoding

### RenderContext

A flat struct with all possible template variables. Populated by the caller (backend or worker) from their own domain types. The `Default` impl sets all fields to empty strings and an empty `context_blocks` map.

Key design choice: **flat over nested.** The struct uses `task_title`, `project_name` etc. instead of nested structs. The nesting (`task.title`, `project.name`) is constructed at template render time in `build_context()`. This keeps `RenderContext` easy to construct from any data source without matching a specific struct hierarchy.

## Analysis: Remaining Gaps and Redundancies

### Redundancies

1. **Two XML construction APIs (`XmlBuilder` vs `XmlTag`).** Both produce the same output. `XmlBuilder` is used in all `ToXml` implementations and context struct rendering. `XmlTag` is used by the free functions `tag()`, `content()`, `wrap()` and is available for external callers. The overlap is intentional — builder for procedural, struct for declarative — but it means two pretty-printing implementations, two escaping paths, and two sets of tests covering the same output. Consider whether `XmlTag` could be implemented *on top of* `XmlBuilder` (or vice versa) to reduce surface area.

2. **`wrap()` is an alias for `tag()`.** They are identical functions. The semantic distinction ("wrapping" vs "tagging") is thin. Having both adds API surface without adding capability.

3. **Context structs (`ProjectContext`, `TaskContext`, etc.) duplicate `RenderContext` fields.** `TaskContext` has the same 11 fields as the `task_*` fields on `RenderContext`, and `From<&RenderContext>` just clones them. These intermediate structs exist only to implement `ToXml`, but the same XML could be produced directly from `RenderContext` fields in the renderer.

### Gaps

1. **No `Serialize` on `RenderContext`.** The struct derives only `Debug, Clone, Default`. If the backend wants to send the render context to the frontend for preview/debugging, it would need manual serialization. Adding `Serialize` would be trivial and useful.

2. **`MemoryContext.to_xml_pretty()` is not implemented.** It falls back to `to_xml()` (which just clones the raw XML string). The `_indent_size` parameter is ignored. This is fine if memory XML is always pre-formatted, but it breaks the contract that `to_xml_pretty(2)` should produce indented output.

3. **`McpIntegrationContext` builds inner XML with `XmlBuilder::new()` (compact) but the outer builder may be pretty.** The `render_frame_pretty` for the inner server entries is manual string formatting (`format!("\n  {}", ...)`) rather than using the builder's own indentation. This produces correct-looking output but is fragile if indent size changes.

4. **`render_items()` double-indents.** It calls `to_xml_pretty(2)` on each item, then prepends `"  "` to every line. If an item's pretty-printed XML already has internal indentation, the result is indented relative to nothing (since `render_items` doesn't know its own depth in the final document). This works because the output is always injected into a `{{ items }}` slot that's already inside a wrapper tag, but the coupling is implicit.

5. **No validation that `RenderContext.context_blocks` keys are well-formed dotted paths.** A key like `"..double.dot"` or `""` would produce unexpected tree structure. Since keys come from the database (`path` + `name`), this should be validated at write time, but the prompts crate silently accepts anything.

### Resolved

- ~~`{{items}}` was plain string replacement~~ — now a real MiniJinja variable via `render_template_with_vars`
- ~~Missing `subtask.*` namespace~~ — added `subtask.parent_task` and `subtask.description`
- ~~Inconsistent filtering (ToXml vs renderer)~~ — all filtering now happens in `ContextRenderer.render_block()`; `ToXml` impls always render
- ~~`FromXml` dead API surface~~ — trait removed, `parse` module utilities remain for ad-hoc extraction
- ~~`render_template()` swallows errors~~ — `try_render_template()` and `try_render_template_with_vars()` added for callers that need error propagation
- ~~Legacy top-level aliases~~ — removed in favor of namespaced variables only
