# Slug Internal Plumbing Plan

## Goal

Remove UUID-based manifest resource plumbing from `nenjo` provider internals and
the harness execution path. UUIDs may remain where they identify runtime records,
platform protocol objects, leases, event IDs, or persisted platform entities, but
they must not be used to select SDK manifest resources or bounce through
`id -> slug -> provider` conversion inside the provider/harness runtime.

The clean shape is:

- SDK/provider/routine/council execution uses `Slug` for manifest resources.
- Harness requests use slug refs for agent, project, routine, council, and model
  resources.
- Worker/platform adapters translate platform UUID protocol fields to slugs once
  at the boundary, then call slug-native harness APIs.
- Session/runtime correlation IDs remain UUIDs because they are not manifest
  resource selectors.

## Current Problem Areas

### Provider And Manifest

- `Manifest::agent_slug_for_id`
- `Manifest::routine_slug_for_id`
- `Manifest::project_slug_for_id`
- `Manifest::council_slug_for_id`
- `Manifest::merge`, `upsert_resource`, and `delete_resource` still key most
  manifest resources by `id`.
- `ManifestResource::id()` and `HasManifestId` keep ID as the generic resource
  identity.
- `Provider` imports `Uuid` only for default nil project construction.

### Harness

- `ChatRequest`, `TaskRequest`, `CronRequest`, and `HeartbeatRequest` now carry
  manifest resources as direct `Slug` fields.
- `run/chat.rs`, `run/task.rs`, `run/cron.rs`, `run/heartbeat.rs`, and
  `domain.rs` now call provider APIs with slugs. Runtime events and core SDK
  input structs may still need manifest UUIDs until the core routine/task input
  surface is converted.
- `execution_context.rs` uses slug-native project namespace helpers.
- `DomainSession` stores `agent` and `project` slugs, so refresh/rebuild does not
  re-enter `id -> slug` conversion.
- `SessionRecord` and local runtime upserts now store manifest resource refs as
  direct `agent`, `project`, and `routine` slug fields.

### Worker Boundary

- Worker command handlers receive platform UUIDs and pass them into harness
  request types.
- Cron and heartbeat handlers still resolve routine/agent IDs using
  `manifest_snapshot().*_slug_for_id`.
- Event bridging still needs UUIDs for platform response/event payloads, but
  that should be isolated from provider selection.

## Design Rules

- Do not add resource-specific slug wrapper types unless their invariants differ.
  Use the common `Slug`.
- Do not allow `Uuid` in provider/harness APIs when the value selects a manifest
  resource.
- Keep UUIDs for runtime identity: `session_id`, `turn_id`, `execution_run_id`,
  `step_run_id`, lease tokens, persisted platform event IDs, and platform
  command payloads.
- Translate platform UUIDs to slug once at the worker/platform boundary.
- Prefer typed request structs with `Slug` fields over helpers that accept
  generic strings.
- Do not keep compatibility overloads. Remove the old ID constructors and enum
  variants.

## Phase 1: Add Slug-Native Harness Request Types

Status: complete. Harness request structs now store `Slug` directly for agent,
project, routine, and heartbeat selectors. `AgentRef` was removed because it did
not add invariants beyond `Slug`.

Update `crates/harness/src/request.rs`:

- Replace `AgentRef::Id(Uuid) | AgentRef::Name(String)` with a slug-only ref:

```rust
pub struct AgentRef {
    pub slug: Slug,
}
```

or simply store `Slug` directly where there is no extra behavior.

- Change request resource fields:

```rust
ChatRequest {
    agent: Slug,
    project: Option<Slug>,
}

TaskRequest {
    project: Slug,
    routine: Option<Slug>,
    agent: Option<Slug>,
}

CronRequest {
    routine: Slug,
    project: Option<Slug>,
}

HeartbeatRequest {
    agent: Slug,
}
```

- Keep runtime UUID fields as UUIDs:
  `session_id`, `task_id`, `domain_session_id`, `execution_run_id`.

Remove:

- `impl From<Uuid> for AgentRef`
- `TaskRequest::with_routine(Uuid)`
- `HeartbeatRequest::new(agent_id: Uuid, ...)`
- `CronRequest::new(routine_id: Uuid, ...)`

## Phase 2: Make Harness Run Paths Slug-Native

Status: complete for harness/provider selection. Run paths no longer resolve
manifest UUIDs into slugs before calling provider APIs. Some reverse
`slug -> id` lookups remain only where core SDK input/event structures still
require UUID fields, such as `TaskInput`, `CronInput`, `HeartbeatInput`, and
trace/transcript response metadata.

Follow-up update: `TaskInput`, `ChatInput`, `RoutineInput`, `CronInput`,
`GateInput`, `CouncilSubtaskInput`, and `HeartbeatInput` now carry project/agent
manifest refs as `Slug` values rather than UUIDs. Remaining routine UUID fields
are event/response metadata or step/agent runtime correlation.

Update:

- `crates/harness/src/run/chat.rs`
- `crates/harness/src/run/task.rs`
- `crates/harness/src/run/cron.rs`
- `crates/harness/src/run/heartbeat.rs`
- `crates/harness/src/domain.rs`
- `crates/harness/src/execution_context.rs`

Remove all `resolve_agent_id` helpers and calls to:

- `manifest.agent_slug_for_id`
- `manifest.routine_slug_for_id`
- `manifest.project_slug_for_id`
- `manifest.council_slug_for_id`

The run path should call provider directly:

```rust
provider.agent(&request.agent).await?
provider.routine(&request.routine)?
provider.project(request.project.as_ref()?)?
```

Replace `agent_name(manifest, agent_id)` with the already resolved agent
manifest or slug. If display name is needed, add slug lookup:

```rust
provider.find_agent_manifest(&agent_slug)
```

Replace `project_slug(manifest, project_id)` with the request project slug.

## Phase 3: Slug-Native Domain Sessions

Change `DomainSession` from:

```rust
agent_id: Uuid,
project_id: Uuid,
```

to:

```rust
agent: Slug,
project: Option<Slug>,
```

Update rebuild/refresh paths:

- `DomainRegistry::rebuild_domain_session`
- `crates/harness/src/manifest.rs`
- persisted domain session restoration in `run/chat.rs`

Domain session IDs remain UUIDs.

## Phase 4: Slug-Native Session Resource Selectors

Change harness session record/upsert resource selector fields:

```rust
agent: Option<Slug>
project: Option<Slug>
routine: Option<Slug>
```

Keep these as UUIDs:

```rust
session_id
turn_id
execution_run_id
task_id
```

Update:

- `crates/harness/src/session.rs`
- `crates/harness/src/local_runtime/runtime.rs`
- `crates/harness/src/local_runtime/record_store.rs`
- `crates/harness/src/local_runtime/event_store.rs`

If platform event queries still filter by UUID, implement that filter in the
worker/platform event adapter, not in provider/harness execution selection.

## Phase 5: Worker Boundary Translation

Worker command payloads may still arrive as platform UUIDs. Convert them before
constructing harness requests.

Introduce a small boundary resolver, for example:

```rust
struct PlatformResourceResolver<'a> {
    manifest: &'a Manifest,
}

impl PlatformResourceResolver<'_> {
    fn agent(&self, id: Uuid) -> Result<Slug>;
    fn routine(&self, id: Uuid) -> Result<Slug>;
    fn project(&self, id: Uuid) -> Result<Option<Slug>>;
}
```

Use it in:

- `worker/src/handlers/chat.rs`
- `worker/src/handlers/task/mod.rs`
- `worker/src/handlers/cron.rs`
- `worker/src/handlers/heartbeat.rs`
- `worker/src/handlers/domain.rs`

The resolver is the only acceptable `id -> slug` bridge. It should live in the
worker/platform adapter layer, not in `nenjo::Manifest` or harness execution.

## Phase 6: Clean Provider/Manifest Identity Helpers

After harness no longer calls them, remove:

- `Manifest::agent_slug_for_id`
- `Manifest::routine_slug_for_id`
- `Manifest::project_slug_for_id`
- `Manifest::council_slug_for_id`

Then replace generic manifest merge identity for slug-owned resources:

- agent by derived `Slug::derive(name)`
- model by derived `Slug::derive(name)`
- routine by derived `Slug::derive(name)`
- council by derived `Slug::derive(name)`
- project by explicit `ProjectManifest.slug`

Keep ID merge/delete only for platform-owned resources that are not yet
slug-modeled, such as domains/MCP servers/context blocks if they still require
UUID identity.

Recommended implementation shape:

```rust
trait HasManifestSlug {
    fn manifest_slug(&self) -> Slug;
}

fn upsert_by_slug<T: HasManifestSlug>(items: &mut Vec<T>, incoming: T)
```

Avoid one trait that tries to cover both UUID and slug resources. Use explicit
helpers so the identity rule is obvious at each call site.

## Phase 7: Routine Events And Worker Event Bridging

`RoutineEvent::StepStarted` currently carries `agent_id: Option<Uuid>`. This
keeps worker task/cron event plumbing tied to agent UUIDs.

Change SDK routine events to carry the agent slug:

```rust
StepStarted {
    agent: Option<Slug>,
}
```

Worker response conversion can map slug back to platform UUID if the platform
event schema still requires it. That mapping belongs in `worker::event_bridge`,
not in `nenjo` routine execution.

Do the same audit for any SDK event field that carries a manifest resource UUID.

## Phase 8: Platform Client/MCP Adapter Cleanup

Platform-facing crates can still deserialize and submit UUIDs for persisted rows
and protocol commands, but SDK manifest construction should not use UUID-derived
fallback slugs for relationships once endpoints expose names/slugs. MCP server
assignments are already slug-native at the platform/dashboard/worker boundary
and use `mcp_servers.name` as the canonical ref.

Update platform APIs or response DTOs to include related slugs for:

- agent model
- routine step agent/council
- routine edge step refs
- council leader/member agents

Then remove temporary fallback conversions such as:

```rust
Slug::derive(id.to_string())
```

from:

- `crates/nenjo/src/client/types.rs`
- `crates/platform/src/client.rs`
- `crates/platform/src/local/executor.rs`
- `crates/platform/src/manifest_mcp/types.rs`

### Platform-To-Manifest Conversion Notes

Current non-MCP conversion sites that still manufacture SDK manifest slugs from
platform UUID fields:

- `crates/worker/src/bootstrap.rs` builds the initial worker manifest from the
  bootstrap response. `AgentManifest.model` now consumes the platform-provided
  `model` slug directly. MCP server assignments also use bootstrap
  `mcp_servers` slug arrays sourced from `mcp_servers.name`.
- `crates/nenjo/src/client/types.rs` converts platform API detail DTOs into SDK
  manifest types. It now consumes platform-provided agent model, council, and
  routine relationship slugs directly.
- `crates/platform/src/client.rs` converts package/platform routine and council
  details into local manifest values. It now consumes platform-provided routine
  step agent/council slugs, routine edge step slugs, and council leader/member
  slugs directly.
- `crates/platform/src/backend.rs` now accepts slug-native manifest MCP routine
  and council references and sends those slug refs to platform endpoints. It no
  longer performs routine/council/agent slug-to-id lookup for these manifest
  write paths.
- `crates/platform/src/local/executor.rs` is the local platform-compatible
  executor. Manifest MCP agent model refs, routine graph refs, and council
  member/leader writes are now slug-native.
- `crates/platform/src/manifest_mcp/types.rs` converts between MCP documents and
  SDK manifests. Agent model refs, agent domain refs, MCP server assignments,
  routine graph agent/council refs, and council leader/member refs are now
  slug-native.

The clean platform integration point is the API/DTO boundary: every manifest
resource relationship that the SDK must execute by slug should arrive from
platform/package/local sources as a slug/ref, not as a UUID that SDK code must
interpret. UUIDs can continue to identify persisted platform rows and protocol
commands, but they should not be the manifest relationship representation.

## Phase 9: Tests And Guardrails

Add tests that fail if UUID selectors creep back in:

- harness `ChatRequest`/`TaskRequest`/`CronRequest`/`HeartbeatRequest` can be
  constructed only with slugs for manifest resources
- harness run paths do not call `*_slug_for_id`
- provider/manifest no longer exposes `*_slug_for_id`
- routine events expose agent slug, not agent UUID
- prompt context remains UUID-free for manifest resources
- worker boundary resolver is the only production `id -> slug` bridge

Add a simple grep-style CI check or test fixture for banned provider/harness
symbols:

```text
agent_slug_for_id
routine_slug_for_id
project_slug_for_id
council_slug_for_id
AgentRef::Id
agent_id: Uuid
routine_id: Uuid
project_id: Uuid
```

Scope the last three bans to harness/provider SDK resource selectors, not
platform command/event payloads.

## Suggested Implementation Order

1. Change harness request types to slugs.
2. Update harness run paths to call provider with request slugs.
3. Convert domain session and session record resource selectors to slugs.
4. Add worker boundary resolver and update worker handlers to build slug-native
   harness requests.
5. Remove manifest `*_slug_for_id` helpers.
6. Convert manifest merge/upsert/delete for slug-owned resources to slug keys.
7. Change SDK routine events from agent UUID to agent slug.
8. Remove UUID-derived fallback slugs from platform/client conversions after
   platform DTOs expose real slugs.

## Verification

Run after each phase:

```text
cargo check -p nenjo --all-targets
cargo check -p nenjo-worker
cargo test -p nenjo
cargo test -p nenjo --test routines
cargo clippy -p nenjo --all-targets -- -D warnings
cargo clippy -p nenjo-worker -- -D warnings
```

## Current Implementation Status

Completed in this cleanup pass:

- Harness request types are slug-native for agent/project/routine selection.
- Harness chat/task/cron/heartbeat execution paths call provider APIs with
  slugs instead of resolving UUIDs through the provider.
- Domain session refresh/rebuild stores and restores agent/project slugs.
- Worker chat/task/cron/heartbeat/domain entry points translate platform UUID
  command fields through `PlatformResourceResolver` at the boundary.
- `nenjo::Manifest` no longer exposes `*_slug_for_id` helper methods.
- Manifest merge/upsert uses slug identity for agents, models, routines,
  projects, councils, and abilities.
- The clippy `question_mark` warning in `crates/knowledge/src/lib.rs` was
  already resolved.
- Platform bootstrap now emits agent/domain/ability MCP assignments as
  `mcp_servers` slug arrays resolved from `mcp_servers.name`; the worker
  bootstrap path consumes those directly instead of deriving slugs from MCP UUIDs.
- Worker bootstrap now consumes the platform-resolved agent `model` slug to
  build `AgentManifest.model`, avoiding UUID-derived model refs during manifest
  hydration.
- Inline platform agent update events now carry and consume `model` as well, so
  manifest.changed events do not overwrite a slug-native model ref with a
  UUID-derived fallback.
- Platform detail/event APIs, manifest MCP document types, local executor paths,
  platform backend paths, and dashboard assignment controls now use
  `mcp_servers` slug arrays for agent/domain/ability MCP assignments.
- Manifest MCP document types no longer contain `slug_to_protocol_uuid` or
  UUID-derived protocol placeholders for agent domain refs, routine graph
  agent/council refs, or council leader/member refs.
- Manifest MCP council tools now accept `leader_agent` and `agent` slugs for
  membership operations. Routine graph step inputs now accept `agent` and
  `council` slugs.
- Platform routine/council response DTOs, manifest bootstrap payloads, and
  manifest.changed inline event documents now expose relationship refs as slugs:
  `leader_agent`, member `agent`, routine step `agent`/`council`, and routine
  edge `source_step`/`target_step`.
- Platform routine/council manifest mutation request DTOs now accept slug refs
  for these relationships. The compatibility `agent_id`, `leader_agent_id`,
  `council_id`, `source_step_id`, and `target_step_id` request/manifest fields
  were removed from the SDK-facing route/MCP shapes where the values are
  manifest refs rather than persisted row IDs.
- Platform agent, routine, and council endpoints are now slug-first at the API
  route boundary. The route handlers resolve slugs through the same org-scoped
  name uniqueness boundary used by their database indexes, and only convert to
  persisted UUIDs at service/storage and event-command boundaries.
- Manifest MCP agent tools now accept `agent` slugs for get, prompt get, update,
  prompt update, and delete. The local SDK executor and platform backend resolve
  agents by slug/name instead of requiring callers to pass UUIDs.
- Manifest MCP agent create/update tools now accept `model` slugs instead of
  model UUIDs. Platform resolves the model slug to the stored model UUID at the
  API boundary.
- Route refs now parse slug input rather than deriving it. Derivation is reserved
  for creation/defaulting boundaries where a user has not explicitly supplied a
  slug.
- Context block slugs are path-aware (`path + name`) to match their folder
  uniqueness model. Platform now has a migration that drops the org-wide
  context-block name uniqueness index and adds a unique `(org_id, path, name)`
  boundary.
- Project create/update now accepts explicit parsed `slug` values, and project
  lookup/update/delete routes and Manifest MCP params use `project` slugs.
  Platform also has a migration replacing the global project slug index with an
  org-scoped `(org_id, slug)` uniqueness boundary.
- Domain lookup/update/delete/prompt/move/reset routes and Manifest MCP params
  now use path-aware `domain` slugs. Platform domain responses include `slug`,
  and platform has a migration replacing org-wide domain name uniqueness with a
  `(org_id, path, name)` boundary to match folder semantics.
- Agent-domain assignment REST routes now resolve `{agent}` and `{domain}` path
  params from slugs at the route boundary, keeping persisted UUIDs inside the
  service/storage/event plumbing.
- Workspace knowledge pack/document REST routes now resolve `{pack}` and `{doc}`
  path params from slugs. Manifest MCP knowledge writes call the slug routes
  directly, and edge writes use `target_doc` instead of `target_item_id`.
- Local session records now use direct `project`, `agent`, and `routine` slug
  fields. Harness chat/task/domain upserts and worker task/cron/agent heartbeat
  upserts populate those fields without a separate manifest-ref sidecar.
- Core SDK execution inputs now use slug refs for project/agent manifest
  selectors: task/chat/routine/cron/gate/council-subtask project refs and
  heartbeat agent refs.
- The platform e2e manifest council scenario now calls `create_council` with
  `leader_agent` and member `agent` slugs.

Still intentionally left for follow-up platform/session cleanup:

- Operational/session endpoints still have UUID-shaped routes or response fields
  where they identify runtime rows rather than manifest resources.
- Platform event payloads still carry UUID fields where they identify persisted
  runtime/platform rows or existing event API fields. The worker now writes slug
  refs directly into local session records after resolving those command IDs
  against the current manifest.
- Routine events still carry step agent UUIDs for worker response conversion.
  Moving those to slugs requires updating worker event bridge DTO mapping and
  platform response schemas that still expect agent UUIDs.
