# Context Blocks

## Purpose

Context blocks are reusable prompt-context assets.

They are the main mechanism for storing durable operating knowledge that should be shared across agents, abilities, domains, and workflows without copying the same prompt text everywhere.

## What a Context Block Contains

- `name`
- `path`
- `display_name`
- `description`
- `template`

## How Context Blocks Are Used

Context blocks are referenced in templates by path-like names.

Examples:

- `{{ nenjo.core.methodology }}`
- `{{ nenjo.core.delegation }}`
- `{{ custom.coding.standards }}`

## What Belongs in a Context Block

Strong candidates:

- operating principles
- policies
- business rules
- methodology
- review standards
- delegation patterns
- role conventions
- prompt-writing patterns
- workflow heuristics

Weak candidates:

- one-off user messages
- transient task details
- resource metadata that should live on the resource itself
- duplicated tool documentation
