# Resource Dependency Order

## Purpose
Defines the recommended creation order when building Nenjo resources to avoid circular dependencies and invalid states.

## Order
1. Context Blocks & Template Variables
2. Agents (with prompt config and memory profile)
3. Abilities, Domains, Scopes, MCP Servers
4. Councils
5. Routines (with steps referencing above)
6. Projects → Tasks → Executions

## Rules
- Never wire downstream resources before upstream ones exist
- Always verify upstream resources before using them
- If order is unsafe, correct it explicitly before writing platform resources or
  SDK manifests
- In platform chat, describe the fields and platform actions needed to create or
  update resources. Only switch to manifest-file instructions when the user asks
  for SDK, local files, import/export, or code-level authoring.
