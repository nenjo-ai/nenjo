# Abilities — Reusable Specialist Execution Modes

## Purpose
Abilities are reusable specialist execution modes that are exposed to an agent as callable tools. They are the primary mechanism for giving an agent a narrow, high-signal capability without turning that capability into a full standalone agent or routine.

## What an Ability Is

An ability is a focused, self-contained unit of behavior that:

- Is assigned to one or more agents
- Appears at runtime as a tool the agent can call using its `tool_name`
- The calling agent decides whether to use it based on the `activation_condition`, current task, and user request
- Runs as a **nested specialist execution** under the calling agent

Abilities are the right unit for **specialization**, not for long-running ownership or complex workflows.

## When to Use an Ability

Use an ability when you need:

- A narrow, well-defined capability with a clear input/output contract
- Reusable specialist behavior that multiple agents might need
- Focused execution that should inherit the caller’s context but use its own developer guidance
- Something that should feel like a “tool” to the calling agent

## When to Avoid an Ability

Do **not** use an ability when:

- The capability requires long-running ownership or complex state management (use an Agent instead)
- The behavior should only activate via explicit user command (use a Domain instead)
- The work is better expressed as a full routine with multiple steps and gates

## Core Fields

- `name` — Internal identifier
- `tool_name` — The name the calling agent uses to invoke it
- `path` — Unique path for referencing (e.g., `nenjo.abilities.code_review`)
- `display_name` — Human-readable name
- `description` — Clear explanation of what the ability does
- `activation_condition` — When the agent should consider using this ability
- `prompt_config.developer_prompt` — Specialized guidance used only while the ability is running
- `platform_scopes` — Permissions granted to the ability during execution, assigned through an admin/platform-controlled path
- `mcp_server_ids` — External tools available to the ability

## Runtime Behavior

When an ability is invoked:

1. It runs as a **nested execution** under the calling agent
2. It **inherits** the caller’s task, project context, user request, and memory state
3. It **keeps** the caller’s system-level framing and overall identity
4. It **replaces** the caller’s developer guidance with its own `developer_prompt`
5. It receives its **own filtered permissions** (scopes + MCP servers)
6. It returns control to the calling agent after completion

This design allows abilities to be powerful specialists while remaining tightly controlled by the parent agent.

## Key Relationships (Canonical)

- `part_of` → Agent’s capability surface
- `references` → Platform Scopes and MCP servers
- `defines` → its own developer_prompt and activation_condition
- `related_to` → Domains (for comparison of activation model)

## Common Patterns

- **Code Review Ability** — Analyzes code for quality, security, and style
- **Data Extraction Ability** — Pulls structured data from unstructured text
- **Risk Assessment Ability** — Evaluates specific risk dimensions
- **Format Conversion Ability** — Transforms data between formats
- **Validation Ability** — Checks compliance against a specific policy

## Agent Guidance

**Reference this block when:**
- Designing or assigning capabilities to agents
- Deciding between an Ability, a full Agent, or a Domain
- Explaining how specialization works in Nenjo
- Troubleshooting why an agent is or isn’t using a particular capability

## Pitfalls to Avoid

- Making abilities too broad (they should be narrow and focused)
- Treating ability scope assignment as an agent-side write; agents may recommend required scopes, but a user/admin must assign them
- Using abilities for long-running or stateful work (use Agents instead)
- Confusing Abilities with Domains (Abilities = agent-invoked tools, Domains = user-activated modes)
- Creating too many similar abilities instead of one well-designed one

## Best Practices

- Keep abilities focused with a clear input/output contract
- Write strong `activation_condition` guidance so agents know when to use them
- Recommend only the minimum scopes and MCP servers they actually need, and tell the user/admin to assign platform scopes through the controlled scope-management path
- Version abilities carefully — changes affect all agents that use them
- Document the expected output format clearly (especially for downstream gates)
