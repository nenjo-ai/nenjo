# Agents — Primary Behavioral Units of Nenjo

**Path:** `nenjo.guide.agents`  
**Kind:** Guide  
**Status:** stable

## Purpose
Agents are the fundamental behavioral units of the Nenjo platform. An agent defines **how work is interpreted**, what role it performs, what runtime context matters, and which capabilities it can access. Every execution in Nenjo ultimately runs through an agent (or a routine composed of agents).

## What an Agent Owns
An agent owns the complete definition of its behavior and surface:

- Behavioral identity and role
- Prompt configuration (system + developer prompts + mode-specific templates)
- Memory profile (core, project, shared focus)
- Model assignment
- Platform scopes (permissions)
- Assigned abilities (specialist tools)
- Assigned domains (user-activated modes)
- Assigned MCP servers (external tools)
- Optional heartbeat schedule

## Runtime Modes

The same agent can operate in different runtime semantics. The **role stays constant**, but the prompt template and surrounding context change:

| Mode              | Description                                                                 | Primary Template Used      |
|-------------------|-----------------------------------------------------------------------------|----------------------------|
| **chat**          | Conversational interaction with a user                                      | `templates.chat`           |
| **task**          | Execution of a project task with full context                               | `templates.task`           |
| **gate**          | Evaluation of evidence against acceptance criteria                          | `templates.gate`           |
| **cron**          | Scheduled/recurring execution                                               | `templates.cron`           |
| **heartbeat**     | Periodic autonomous checks and maintenance                                  | `templates.heartbeat`      |
| **council**       | Participation as a member or leader in a multi-agent council                | `templates.task` (adapted) |

**Key Insight**: Runtime mode determines which template and context variables are injected. The agent’s core identity and memory profile remain consistent across modes.

## Prompt Configuration

Agent prompts are split into stable, versionable parts. Prompt content is stored as a dedicated subresource — metadata updates (name, scopes, abilities) and prompt updates are treated as **separate operations**.

### Core Prompt Components

- **`system_prompt`** — The foundational, immutable identity and principles of the agent. Rarely changes.
- **`developer_prompt`** — High-signal guidance that shapes behavior, reasoning style, and tool usage. Updated more frequently than system prompt.
- **`prompt_config.task`** — Specialized wrapper used when executing project tasks (includes `{{ task }}`, acceptance criteria, etc.).
- **`prompt_config.chat`** — Lightweight wrapper for direct user conversation (usually just `{{ chat.message }}`).
- **`prompt_config.gate`** — Used when the agent must evaluate evidence and emit a structured verdict.
- **`prompt_config.cron`** — Sparse, execution-focused template for scheduled jobs.
- **`prompt_config.heartbeat`** — Operational template for recurring autonomous work.

**Prompt Locking**: Agents support prompt locking. A locked prompt can still be read but cannot be mutated until unlocked. This is useful for production agents where behavior must remain stable.

## Memory Profile

Every agent has a `memory_profile` that controls **what** it remembers and **where** it stores that knowledge. This is one of the most powerful features for building reliable, long-running agents.

### The Three Memory Scopes

| Scope            | What It Stores                                      | Typical Use Case                              | Example Focus |
|------------------|-----------------------------------------------------|-----------------------------------------------|---------------|
| **core_focus**   | Long-term, cross-project knowledge                  | Personality, methodology, core principles     | "Always follow Nenjo methodology and prefer explicit gates" |
| **project_focus**| Knowledge specific to one project                   | Task history, decisions, documents, context   | "Remember all acceptance criteria and dependency decisions for this project" |
| **shared_focus** | Knowledge shared across projects or with other agents | Reusable patterns, team conventions           | "Reusable code review patterns and council delegation strategies" |

**Best Practice**: Start simple with only `project_focus`. Expand to `core_focus` and `shared_focus` as the agent’s responsibilities grow.

## Assigned Capability Surface

Agents gain power through explicit assignments (not by default):

- **Platform Scopes** — Fine-grained permissions (`projects:read`, `agents:write`, `context_blocks:read`, etc.)
- **Abilities** — Reusable specialist execution modes (narrow, high-signal tools)
- **Domains** — User-activated execution modes (elevated permissions + prompt addons)
- **MCP Servers** — External tool surfaces (stdio or HTTP)

**Design Principle**: Give agents the *minimum* capabilities required. Over-assigning scopes or abilities increases risk and reduces predictability.

## Key Relationships (Canonical)

- `part_of` → `nenjo.domain.nenjo`
- `references` → `nenjo.kind.memory` (via memory_profile)
- `references` → Abilities, Domains, Scopes, and MCP servers
- `defines` → its own prompt configuration and runtime behavior

## Agent Guidance

**Reference this block when:**
- Designing or configuring a new agent
- Explaining agent architecture to users or stakeholders
- Troubleshooting memory issues, prompt behavior, or capability problems
- Choosing between a single powerful agent vs multiple specialized agents

## Common Patterns

- **Bootstrap Agent (Nenji)**: Heavy on `core_focus`, broad scopes, many abilities
- **Specialist Agent**: Narrow `project_focus`, specific abilities, minimal scopes
- **Council Member**: Strong `shared_focus`, clear delegation strategy awareness

## Pitfalls to Avoid

- Overloading `core_focus` with project-specific data (causes leakage)
- Forgetting to update `project_focus` after major requirement changes
- Giving production agents overly broad scopes
- Treating prompt updates and metadata updates as the same operation