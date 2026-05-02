# Domains — User-Activated Execution Modes

## Purpose
Domains are **user-activated execution modes** that allow a user to explicitly place an agent into a different operating state with modified instructions, expanded permissions, and additional specialist capabilities. They are designed for elevated, sensitive, or mode-switching scenarios where the agent needs temporary access to different behavior and tools.

## What Makes a Domain Different from an Ability

| Aspect              | **Ability**                          | **Domain**                              |
|---------------------|--------------------------------------|-----------------------------------------|
| **Activation**      | Invoked by the agent as a tool       | Activated explicitly by the user        |
| **Scope**           | Narrow, focused capability           | Broader mode change with new instructions |
| **Permission Model**| Filtered scopes for the ability      | Expanded platform scopes for the session |
| **Use Case**        | Specialist tasks                     | Elevated, sensitive, or strategic modes |

**Key Distinction**: Abilities are **agent-invoked tools**. Domains are **user-activated modes**. If a capability should only run after explicit user approval or command, it should usually be a Domain, not an Ability.

## When to Use a Domain

Use a Domain when you need:

- Elevated permissions or sensitive operations (e.g., production deployments, financial actions, legal reviews)
- A temporary shift in agent behavior or personality
- Access to a different set of tools or abilities for a specific session
- Clear auditability of when and why a mode change occurred

## When to Avoid a Domain

Do **not** use a Domain when:

- The capability is narrow and can be safely invoked by the agent itself (use an Ability instead)
- The behavior should be always available (add it to the agent permanently)
- You need complex multi-step workflows (use a Routine instead)

## Core Fields

- `name` — Internal identifier
- `path` — Unique reference path
- `display_name` — Human-readable name shown to users
- `description` — Explains what the domain does and when to use it
- `command` — The user command that activates the domain (e.g., `/compliance-mode`)
- `platform_scopes` — Additional or expanded permissions granted while active
- `ability_ids` — Specialist abilities made available in this mode
- `mcp_server_ids` — External tools available in this mode
- `prompt_config.developer_prompt_addon` — Additional guidance appended to the agent’s developer prompt while the domain is active

## Runtime Behavior

When a user activates a domain:

1. The agent receives the `developer_prompt_addon` which modifies its behavior and reasoning style
2. The agent gains access to the additional `platform_scopes`, `abilities`, and `mcp_server_ids` defined by the domain
3. A **domain session** is created and tracked for auditability
4. The expanded capabilities remain active until the user explicitly deactivates the domain or the session ends
5. All actions taken while the domain is active are associated with that domain session

## Key Relationships (Canonical)

- `part_of` → `nenjo.domain.nenjo`
- `governs` → expanded scopes, abilities, and MCP servers during the session
- `defines` → developer_prompt_addon and activation command
- `references` → Abilities and MCP servers made available in the domain

## Common Patterns

- **Compliance Mode** — Expanded regulatory scopes + specialized compliance abilities
- **Production Mode** — Elevated deployment permissions with stricter audit logging
- **Legal Review Mode** — Access to legal analysis abilities and sensitive document tools
- **Debug Mode** — Additional diagnostic abilities and relaxed output constraints
- **Executive Summary Mode** — Different reasoning style focused on high-level synthesis

## Agent Guidance

**Reference this block when:**
- Designing user-facing mode switches
- Deciding between an Ability and a Domain
- Explaining elevated or sensitive operations to users
- Implementing audit or compliance requirements

## Pitfalls to Avoid

- Using Domains for narrow capabilities that an agent could safely invoke itself
- Granting overly broad scopes in a domain (still follow least privilege)
- Forgetting to track domain sessions for audit purposes
- Creating too many overlapping domains instead of composing them cleanly
- Making domain activation too easy for high-risk operations

## Best Practices

- Use clear, memorable `command` names (e.g., `/compliance`, `/exec-mode`)
- Document exactly what changes when the domain is activated
- Keep domain sessions visible in the UI and logs
- Prefer composition (multiple focused domains) over monolithic mega-domains
- Always include a way for users to explicitly deactivate a domain