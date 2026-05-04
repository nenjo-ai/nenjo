# Context Blocks — Reusable Lego Blocks of Agent Behavior

**Path:** `nenjo.guide.context_blocks`  
**Kind:** Guide  
**Status:** stable

## Purpose
Context Blocks are the primary mechanism for storing **durable, reusable operating behavior** in Nenjo. They act as modular, composable “Lego blocks” of agent behavior — allowing you to define principles, methodologies, patterns, and rules once and reference them across agents, abilities, domains, routines, and workflows without duplicating prompt text.

## What a Context Block Is

A Context Block is a reusable prompt asset that contains:

- A clear purpose and description
- A template (the actual content injected into prompts)
- A unique path-based name for referencing (e.g., `{{ nenjo.core.methodology }}`)

They are the **glue** that makes the behavioral layer powerful and maintainable. Instead of copying the same guidance into every agent prompt, you define it once as a Context Block and reference it everywhere.

## Strong Use Cases (Good Lego Blocks)

Context Blocks work best for:

- Operating principles and core methodology
- Delegation patterns and role conventions
- Review standards and quality criteria
- Prompt-writing patterns and reasoning styles
- Workflow heuristics and operating frameworks

## Weak Use Cases (Avoid These)

Do **not** put the following in Context Blocks:

- One-off user messages or transient instructions
- Task-specific details (these belong in the Task itself)
- Resource metadata that should live on the resource (e.g., agent descriptions)
- Duplicated tool documentation (use Abilities instead)
- Information that can be classified as project or business knowledge

## Core Fields

- `name` — Internal identifier
- `path` — Unique path-based reference (e.g., `nenjo.core.methodology`)
- `display_name` — Human-readable name
- `description` — Explains what the block contains and when to use it
- `template` — The actual content that gets injected into prompts

## How Context Blocks Are Used

Context Blocks are referenced in agent prompts and templates using path-like syntax:
```text
{{ nenjo.core.methodology }}
{{ nenjo.core.delegation }}
{{ custom.coding.standards }}
{{ nenjo.patterns.gated_pipeline }}
```

When a prompt is rendered, the system automatically expands these references, injecting the block’s content at that location. This creates clean, maintainable, and composable prompts.

## Key Relationships (Canonical)

- `part_of` → Knowledge layer of the Nenjo platform
- `references` → Template Variables and prompt templates
- `defines` → reusable behavior patterns and principles
- `related_to` → Agents, Abilities, Domains, and Routines that use them

## Common Patterns

- **Core Methodology Block** — Shared principles every agent should follow
- **Delegation Pattern Block** — How agents should delegate work to abilities or councils
- **Review Standards Block** — Consistent quality and acceptance criteria
- **Domain-Specific Guidance** — Specialized instructions for a particular vertical
- **Prompt Engineering Patterns** — Reusable techniques for better reasoning

## Agent Guidance

**Reference this block when:**
- Designing or refactoring agent prompts
- Creating reusable guidance that multiple agents or domains should share
- Building composable, maintainable prompt systems
- Explaining how to avoid prompt duplication and bloat

## Pitfalls to Avoid

- Putting transient or one-off instructions into Context Blocks
- Creating too many small, overlapping blocks (prefer fewer, well-scoped blocks)
- Forgetting to version Context Blocks when their content changes significantly
- Using Context Blocks for things that should be Abilities or Domains

## Best Practices

- Treat Context Blocks as **first-class knowledge assets** — version and review them carefully
- Use clear, hierarchical path naming (e.g., `nenjo.core.*`, `nenjo.patterns.*`, `custom.*`)
- Keep blocks focused and relatively short
- Combine multiple blocks in a single prompt for powerful composition
- Make Context Blocks visible and searchable in the UI so teams can discover and reuse them