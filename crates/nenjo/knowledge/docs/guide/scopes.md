# Platform Scopes

## Purpose

Platform scopes are exact permission strings. They control which platform
resource families an agent, ability, domain, API key, or organization member
may read or mutate.

Agent-facing creation and metadata-update tools do not assign platform scopes
to agents, abilities, or domains. Scopes may be read for troubleshooting, but
changing them is an admin or platform-controlled operation outside the normal
agent MCP surface.

Do not invent scope names. Use only the canonical scope strings in this
document unless live platform state or source code proves a newer scope exists.
If a requested permission is not listed here, say that no canonical platform
scope is documented for it.

## Scope Semantics

Scopes use `resource:action`.

- `:read` allows read/list/get style access for that resource family.
- `:write` allows create/update/delete/mutation style access and implies
  `:read` for the same resource family.
- Empty API-key scopes mean full API-key access.
- Runtime resource `platform_scopes` should be explicit, but agent-facing MCP
  creation and metadata-update tools do not accept `platform_scopes` for
  agents, abilities, or domains.
- There are no documented wildcard scopes.
- There is no separate documented `:execute` action. Execution-like operations
  are currently authorized through the relevant resource family's read/write
  scope.

## Canonical Agent/API Resource Scopes

These are the platform resource scopes used by agents, abilities, domains, MCP
tool filtering, and API keys.

| Scope | Meaning |
|---|---|
| `agents:read` | Read agent metadata, assignments, and prompt-related surfaces exposed by read tools. |
| `agents:write` | Create, update, assign, unassign, reset, delete, lock, or otherwise mutate agents. |
| `abilities:read` | Read abilities and ability prompt/config surfaces. |
| `abilities:write` | Create, update, delete, or otherwise mutate abilities. |
| `domains:read` | Read domains and domain prompt/config surfaces. |
| `domains:write` | Create, update, delete, enter/exit sessions, or otherwise mutate domains. |
| `projects:read` | Read projects, tasks, documents, dependencies, executions, and project-scoped state. |
| `projects:write` | Create, update, delete, or otherwise mutate projects, tasks, documents, dependencies, executions, and project-scoped state. |
| `routines:read` | Read routines and routine configuration. |
| `routines:write` | Create, update, delete, or otherwise mutate routines. |
| `councils:read` | Read councils and council configuration. |
| `councils:write` | Create, update, delete, or otherwise mutate councils. |
| `context_blocks:read` | Read context blocks and prompt-context assets. |
| `context_blocks:write` | Create, update, delete, or otherwise mutate context blocks. |
| `mcp_servers:read` | Read external MCP server configuration and assignments. |
| `mcp_servers:write` | Create, update, delete, assign, or otherwise mutate MCP server configuration. |
| `chat:read` | Read chat sessions, messages, commands, stream metadata, and notifications where exposed. |
| `chat:write` | Create, update, delete, send, stream, or otherwise mutate chat-related resources. |
| `models:read` | Read model records and model assignment options. |
| `models:write` | Create, update, delete, or otherwise mutate model records. |

## Organization/User Scopes

These scopes are used for human organization membership and management API
authorization. They are not normally assigned to runtime agents unless a
specific platform tool explicitly supports that administrative surface.

| Scope | Meaning |
|---|---|
| `org:read` | Read organization metadata. |
| `org:write` | Mutate organization metadata. |
| `org_members:read` | Read organization members and member permissions. |
| `org_members:write` | Mutate organization members and member permissions. |
| `org_invites:read` | Read organization invitations. |
| `org_invites:write` | Create, resend, revoke, accept, or otherwise mutate organization invitations. |
| `org_billing:read` | Read billing-related organization state. |
| `org_billing:write` | Mutate billing-related organization state. |
| `workers:read` | Read worker enrollment, status, runtime, and capability information. |
| `workers:approve` | Read or set worker approval keys and approve or reject pending workers. |
| `workers:write` | Mutate worker enrollment/runtime administration other than approval-key operations. |
| `api_keys:read` | Read API keys and API-key metadata. |
| `api_keys:write` | Create, update, revoke, or otherwise mutate API keys. |

## Resource Mapping Notes

- Tasks, documents, dependencies, execution streams, and executions are
  project-scoped; use `projects:read` or `projects:write`.
- Agent prompt updates are agent mutations; use `agents:write`.
- Ability prompt updates are ability mutations; use `abilities:write`.
- Domain prompt/config updates are domain mutations; use `domains:write`.
- Model selection usually needs `models:read`; changing model records needs
  `models:write`.
- External MCP server assignment usually involves both the parent resource
  scope, such as `agents:write`, and `mcp_servers:read` or `mcp_servers:write`
  depending on whether the server itself is being modified.

## Agent Guidance

Before recommending platform scopes for an agent, ability, or domain:

1. Read this document.
2. Select the minimum exact scopes required.
3. Prefer read-only scopes unless the resource must mutate that resource
   family.
4. Use domains for temporary elevation instead of permanently broadening an
   agent through admin-managed scope changes.
5. If a permission does not map cleanly to one of the canonical scopes above,
   call out the uncertainty instead of inventing a new scope.

Do not include `platform_scopes` when creating or updating agents, abilities,
or domains through MCP tools. If a user asks to change platform permissions,
explain that this requires the user or an admin to assign scopes through the
platform scope-management path. Agents assigning scopes to themselves or to
other executable resources is a security boundary violation.

## Common Patterns

- Read-only platform guide: `agents:read`, `projects:read`, `routines:read`,
  `domains:read`, `abilities:read`, `models:read`,
  `mcp_servers:read`.
- Agent builder runtime access: `agents:write`, `models:read`,
  `abilities:read`, `domains:read`, `context_blocks:read`,
  `mcp_servers:read`. This allows metadata/prompt work, not assigning platform
  scopes to executable resources.
- Workflow builder: `routines:write`, `projects:read`, `agents:read`,
  `domains:read`
- Project work manager: `projects:write`, `routines:read`, `agents:read`,
  `models:read`.
