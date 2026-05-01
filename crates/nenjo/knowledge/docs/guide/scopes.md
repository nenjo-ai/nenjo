# Platform Scopes — Permission Layer

## Purpose
Platform Scopes are the **permission layer** of the Nenjo platform. They determine exactly which resources and actions an agent, ability, domain, or API key is allowed to access. Scopes enforce the principle of least privilege and are a critical part of building secure, auditable agentic systems.

## What Scopes Control

Scopes govern access to all major resource families in the platform:

- **Agents** — Create, read, update, delete, assign
- **Abilities** — Assign, read, execute
- **Domains** — Activate, read, configure
- **Projects** — Create, read, update, manage tasks and documents
- **Routines** — Create, read, update, execute
- **Councils** — Create, read, update, participate
- **Models** — Access to specific models
- **Context Blocks** — Read, create, update
- **Tasks & Executions** — Full lifecycle control

## Scope Format

Scopes follow a consistent `resource:action` pattern:

| Example              | Meaning                              |
|----------------------|--------------------------------------|
| `projects:read`      | Can view projects                    |
| `projects:write`     | Can create and modify projects       |
| `agents:read`        | Can view agents                      |
| `agents:write`       | Can create and modify agents         |
| `context_blocks:read`| Can read context blocks              |
| `routines:execute`   | Can trigger and run routines         |

Some scopes may include wildcards or more granular actions depending on the resource.

## Why Scopes Matter

Scopes directly impact:

- Which tools and capabilities are exposed to an agent at runtime
- What data an agent can read or modify
- What a delegated ability or domain session is allowed to do
- Overall security posture and auditability of the system

## How Scopes Are Assigned

Scopes can be assigned to:

- **Agents** — Define the agent’s baseline permissions
- **Abilities** — Grant temporary, narrow permissions during execution
- **Domains** — Expand permissions when a user-activated mode is active
- **API Keys** — Control what external systems or users can do

## Key Relationships (Canonical)

- `part_of` → Security and permission model of the platform
- `governs` → What agents, abilities, and domains can do
- `defines` → permission boundaries for execution
- `references` → Agents, Abilities, and Domains that use them

## Common Patterns

- **Minimal Agent** — Only the scopes strictly needed for its role
- **Specialist Ability** — Very narrow scopes (e.g., `tasks:read`, `context_blocks:read`)
- **Elevated Domain** — Broad scopes activated only when user explicitly enables the domain (e.g., production deployment scopes)
- **Bootstrap Agent (Nenji)** — Broad scopes to help users create resources

## Agent Guidance

**Reference this block when:**
- Designing permission models for agents or abilities
- Implementing least-privilege access
- Troubleshooting “permission denied” errors
- Auditing what an agent or domain is allowed to do

## Pitfalls to Avoid

- Over-assigning scopes (especially `write` and `execute` permissions)
- Giving production agents overly broad scopes by default
- Using the same scopes for agents and domains (domains should usually be more restricted until activated)
- Forgetting that abilities inherit filtered versions of the caller’s scopes

## Best Practices

- Follow the **principle of least privilege** — start with minimal scopes and add only when needed
- Use Domains for temporary elevation rather than giving broad scopes to agents permanently
- Make scopes visible and auditable in the UI
- Regularly review and prune unused or overly broad scopes
- Document why each scope is granted to an agent or ability