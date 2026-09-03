# nenjo-xml

Generic XML serialization and MiniJinja rendering used by Nenjo prompt and
runtime-context assembly.

The crate deliberately has no knowledge of agent manifests, sessions, tasks,
or privileged template namespaces. Callers provide a flat
`HashMap<String, String>` when they need template rendering; dotted keys are
converted into nested MiniJinja access.

## Template rendering

```rust
use std::collections::HashMap;

use nenjo_xml::template::render_template;

let vars = HashMap::from([
    ("profile.name".to_string(), "coder".to_string()),
    ("document.title".to_string(), "Fix login bug".to_string()),
]);

let rendered = render_template(
    "{{ profile.name }}: {{ document.title }}",
    &vars,
);

assert_eq!(rendered, "coder: Fix login bug");
```

Available entry points include:

- `render_template` for chainable-undefined rendering with fallback to the
  original template on syntax errors;
- `try_render_template` for fallible rendering;
- `try_render_template_strict` when undefined values must be rejected;
- `render_template_with_named_templates` for MiniJinja includes.

Auto-escaping is disabled because prompt fragments commonly contain XML.
Backslash-escaped Jinja delimiters, such as `\{{ example }}`, remain literal.

## Nenjo's static prompt boundary

The XML crate accepts any caller-provided keys, but Nenjo's authored system
prompts, developer prompts, and context blocks expose only:

- declared package arguments under `args.*`;
- resolved context-block selectors, including `context.*` and package paths;
- resolved knowledge indexes under `pkg.*`, `lib.*`, or `local.*`.

Agent identity and live chat, task, project, routine, gate, Git, memory,
artifact, and clock state are not template variables. The agent runtime emits
those values as typed session or turn context and rejects their selectors in
static prompts.

## XML serialization

Serde-compatible values can be serialized directly:

```rust
use nenjo_xml::{to_xml, to_xml_pretty};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename = "profile")]
struct Profile<'a> {
    #[serde(rename = "@name")]
    name: &'a str,
    description: &'a str,
}

let profile = Profile {
    name: "coder",
    description: "Writes code",
};

assert!(to_xml(&profile).contains("name=\"coder\""));
assert!(to_xml_pretty(&profile, 2).contains("<description>Writes code</description>"));
```

The crate also exports `xml_escape`, `xml_unescape`, `render_items`, and
`metadata_json_to_xml`. Parsing helpers for targeted XML inspection live under
`nenjo_xml::xml::parse`.
