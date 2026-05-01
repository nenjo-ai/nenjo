# Manifest Contract Principles

This document describes the manifest contract as it exists in the codebase today.

It covers:

- the canonical runtime manifest model in `nenjo`
- the stable shared manifest MCP contract in `nenjo-platform`
- how the worker reads, writes, and exposes manifest resources

It is intentionally implementation-aligned, not aspirational.

## Scope

The manifest contract serves three audiences:

- the runtime, which executes from `nenjo::manifest::*`
- platform-facing SDK code, which uses `nenjo-platform` manifest contract types
- agents, which access manifest resources through worker-exposed manifest tools

The same conceptual resources appear at all three layers, but the shapes are not identical.

## Ownership

### `nenjo`

`nenjo` owns the canonical runtime model.

Examples:

- `AgentManifest`
- `AbilityManifest`
- `DomainManifest`
- `RoutineManifest`
- `CouncilManifest`
- `ProjectManifest`
- `ModelManifest`
- `ContextBlockManifest`
- `McpServerManifest`

This layer is runtime-oriented. It models the data the provider and runner need to execute.

### `nenjo-platform`

`nenjo-platform` owns the stable manifest tool contract:

- tool names
- parameter schemas
- result documents
- read/write separation
- prompt/content subresource boundaries
- access policy behavior

This is the main compatibility surface for worker tool exposure and SDK clients.

### Worker

The worker owns:

- bootstrap and local manifest caching
- local reads from the cached manifest
- authoritative writes through the platform backend
- external MCP reconciliation for user-configured MCP servers

The worker should not contain Nenjo-specific one-off MCP client behavior. Generic external MCP integration remains valid and supported.

## Canonical Rule: Runtime Read, Platform Write

Inside the worker:

- reads come from the local manifest cache
- writes go to the platform first
- successful writes update the local manifest immediately

This is implemented through:

- `LocalManifestStore` in `crates/nenjo/src/manifest/local.rs`
- `PlatformManifestBackend` in `nenjo-platform`
- worker manifest handlers that apply incremental cache updates

This gives:

- fast local reads
- one authoritative validator and normalizer
- deterministic runtime state after successful mutations

## Naming Convention

The implemented tool contract uses snake_case names, not dotted names.

Examples:

- `list_agents`
- `get_agent`
- `update_agent`
- `get_agent_prompt`
- `update_agent_prompt`

This naming is the current stable contract because it is what `ManifestMcpContract::tools()` publishes and what the worker dispatches.

Do not document or rely on dotted names like `agents.list` unless the implementation changes.

## Actual Tool Registry

The current manifest tool registry is defined in `crates/nenjo-platform/src/manifest_mcp/tools.rs`.

Current resources and operations:

- agents: `list`, `get`, `get_prompt`, `update`, `update_prompt`, `delete`
- abilities: `list`, `get`, `get_prompt`, `create`, `update`, `update_prompt`, `delete`
- domains: `list`, `get`, `get_prompt`, `create`, `update`, `update_prompt`, `delete`
- projects: `list`, `get`, `create`, `update`, `delete`
- project documents: `list`, `get`, `get_content`, `create`, `update_content`, `delete`
- routines: `list`, `get`, `create`, `update`, `delete`
- models: `list`, `get`, `create`, `update`, `delete`
- councils: `list`, `get`, `create`, `update`, `add_member`, `update_member`, `remove_member`, `delete`
- context blocks: `list`, `get`, `get_content`, `create`, `update`, `update_content`, `delete`

Important asymmetries that are real, not accidental:

- agents do not currently expose `create_agent`
- domains use `get_domain_prompt` and `update_domain_prompt`, but those operate on the domain prompt/manifest document shape
- councils need explicit member actions instead of pretending membership is just a flat patch field
- project documents and context blocks use explicit content subresources

## Read Semantics

The contract separates summary reads from content-bearing reads.

### `list_*`

Top-level list tools return summaries only.

Examples:

- `list_agents` returns `AgentSummary`
- `list_domains` returns `DomainSummary`
- `list_projects` returns `ProjectSummary`

These tools take no parameters. Their schema is the empty object.

### `get_*`

Top-level get tools return the main operational document for the resource, but not sensitive prompt/content fields by default.

Examples:

- `get_agent` returns `AgentDocument`
- `get_ability` returns `AbilityDocument`
- `get_project` returns `ProjectDocument`

### Explicit subresource reads

Sensitive or content-heavy fields are exposed through explicit subresource tools.

Current subresource reads:

- `get_agent_prompt`
- `get_ability_prompt`
- `get_domain_prompt`
- `get_project_document_content`
- `get_context_block_content`

This is the implemented rule for prompts and large content.

## Sensitive Content Rule

The current implementation treats these areas as explicit subresources rather than ordinary fields on list/get:

- agent prompt config
- ability prompt config
- domain prompt config
- context block template content
- project document content

That rule is enforced by the `Document` and `PromptDocument` split in `nenjo-platform/src/manifest_mcp/types.rs`.

Examples:

- `AgentDocument` is prompt-free
- `AgentPromptDocument` includes `prompt_config`
- `ContextBlockDocument` is content-free
- `ContextBlockContentDocument` includes `template`

## Update Semantics

Ordinary updates are patch-style.

Implemented semantics:

- omitted field means unchanged
- `Option<Option<T>>` means nullable field with explicit clear support
- prompt/content subresources are updated through dedicated tools, not ordinary metadata updates

Examples:

- `AgentUpdateDocument`
- `AbilityUpdateDocument`
- `DomainUpdateDocument`
- `ContextBlockUpdateDocument`

Prompt/content updates use dedicated request types such as:

- `AgentPromptUpdateParams`
- `AbilityPromptUpdateParams`
- `DomainPromptUpdateParams`
- `ContextBlockContentUpdateParams`
- `ProjectDocumentContentUpdateParams`

## Resource Taxonomy

The current codebase effectively splits resources into four groups.

### Composition resources

These shape the runtime graph directly:

- agents
- projects
- routines
- councils

These use the most stable document-style operations.

### Capability resources

These influence runtime behavior and can carry scopes, prompts, or activation logic:

- abilities
- domains

These are still resource-style CRUD surfaces, but they also have explicit prompt subresources.

### Catalog resources

These are reusable configuration rows:

- models
- mcp servers in the runtime manifest

Note: `mcp_servers` are part of the runtime manifest model, but they are not currently exposed in the manifest MCP tool registry.

### Content resources

These hold large or sensitive payloads:

- context blocks
- project documents

These use explicit content subresources.

## Domain Contract Note

The runtime canonical shape is `nenjo::manifest::DomainManifest`.

The platform contract exposes:

- `DomainDocument` for normal reads and writes
- `DomainPromptDocument` for prompt-bearing reads and writes

In the results layer:

- `DomainManifestGetResult` is currently an alias of `DomainPromptGetResult`
- `DomainManifestMutationResult` is currently an alias of `DomainPromptMutationResult`

The tool names remain:

- `get_domain_prompt`
- `update_domain_prompt`

That is the current implementation, even though the underlying backend methods still use `domains_get_manifest` and `domains_update_manifest`.

## Worker Exposure Rule

The worker exposes manifest tools based on the caller agent's `platform_scopes`.

This is implemented through:

- `ManifestAccessPolicy`
- `PlatformManifestBackend::with_access_policy(...)`
- `HarnessToolFactory`

The worker currently grants tool availability by scope group:

- agents
- abilities
- domains
- projects
- routines
- models
- councils
- context blocks

The worker also filters returned manifest resources through the same policy on the backend path.

## External MCP Rule

External MCP integrations are still supported.

The worker keeps:

- `ExternalMcpPool`
- external stdio/HTTP MCP discovery
- per-agent filtering by assigned `mcp_server_ids`
- optional scope filtering for external MCP tool exposure

What should not exist is a separate Nenjo-specific special-case MCP client path that bypasses the shared manifest contract.

## Bootstrap Rule

Bootstrap now treats encrypted prompt-bearing resources as fail-closed.

Current worker behavior:

- bootstrap requires an enrolled ACK before decrypting encrypted agent prompt payloads
- bootstrap does not silently replace undecryptable prompt configs with defaults
- if the worker cannot decrypt bootstrap prompt payloads yet, bootstrap should stop rather than load partial prompt state

This matches the security boundary used by `WorkerAgentPromptPayloadEncoder` and the worker bootstrap manifest hydrator.

## Local Cache Rule

The worker caches the manifest in a mixed flat-file and tree layout:

- flat JSON arrays for top-level collections like `agents.json`, `projects.json`, `models.json`
- tree directories for `abilities`, `domains`, and `context_blocks`
- separate workspace document sync state via `_manifest.json` inside project workspaces

This cache is an implementation detail, but the runtime depends on it for local reads.

## Principles To Keep

These are the principles the implementation currently follows and should preserve:

- runtime models live in `nenjo`
- shared manifest contract types live in `nenjo-platform`
- local reads and authoritative platform writes stay separate
- prompts and heavy content use explicit subresource tools
- top-level list operations stay compact
- agent-facing exposure is scope-gated
- external MCP remains generic, not Nenjo-special-cased

## Principles Not To Assume

Do not assume any of the following unless the implementation changes:

- dotted tool names like `agents.list`
- perfectly symmetric CRUD across every resource
- that every runtime manifest resource is exposed as a manifest MCP tool
- that prompts are safe to include in ordinary `get` results
- that bootstrap can safely degrade encrypted prompt state to defaults
