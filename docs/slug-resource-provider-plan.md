# Slug Resource Provider Plan

## Goal

Agents should reference agent, project, council, model, and routine manifest resources by slug only. UUIDs remain platform persistence and runtime correlation details, but they must not appear in agent prompt context, SDK manifest references, or provider lookup APIs.

The provider should expose one lookup function per manifest resource:

```rust
provider.agent("reviewer")
provider.routine("triage")
provider.project("platform")
provider.model("gpt_4_1")
provider.council("review_board")
```

Remove public ID lookup APIs such as `agent_by_id`, `agent_by_name`, `routine_by_id`, `build_agent_by_id`, and `project_by_slug`.

## Design Rules

- Use the common `Slug` type for resource handles.
- Do not create per-resource slug types unless a real invariant differs by resource kind.
- `nenjo` manifest and runtime types are slug-native.
- Platform UUIDs terminate at platform/client/worker translation boundaries.
- Model-visible context renders slugs, names, and descriptions only.
- Package/local manifests preserve authored slug references instead of resolving them to UUIDs.

## Phase 1: Manifest Slugs

Use `ProjectManifest.slug` as the explicit project handle. For agent, model,
routine, and council manifests, treat `name` as the authored resource handle and
normalize it through the common `Slug` type when indexing or resolving. Do not
add parallel `slug` fields for those resources unless the SDK later needs a
separate display label.

Convert resource references to slugs:

```rust
AgentManifest {
    model: Option<Slug>,
    domains: Vec<Slug>,
    mcp_servers: Vec<Slug>,
    abilities: Vec<Slug>,
}

CouncilManifest {
    leader_agent: Slug,
    members: Vec<CouncilMemberManifest>,
}

CouncilMemberManifest {
    agent: Slug,
    priority: i32,
}

RoutineStepManifest {
    slug: Slug,
    routine: Slug,
    agent: Option<Slug>,
    council: Option<Slug>,
}

RoutineEdgeManifest {
    source_step: Slug,
    target_step: Slug,
}
```

Routine step slugs are graph node handles scoped to the routine. They are not platform resource slugs.

## Phase 2: Manifest Index

Replace ID/name indexes with slug indexes:

```rust
agents: HashMap<Slug, usize>
models: HashMap<Slug, usize>
routines: HashMap<Slug, usize>
projects: HashMap<Slug, usize>
councils: HashMap<Slug, usize>
```

Provider lookup indexes key these resource types by slug. Manifest persistence
can still carry UUIDs for platform merge/upsert/delete until the store protocol
is moved fully to slug keys.

Decide duplicate behavior explicitly:

- provider index construction is last-write-wins by slug today, matching current
  manifest precedence behavior
- within a single loaded manifest, duplicate slugs should eventually produce a
  validation error

## Phase 3: Provider API

Expose only slug lookup methods:

```rust
pub async fn agent(&self, slug: impl AsRef<str>) -> Result<AgentBuilder<Self>, ProviderError>;
pub fn routine(&self, slug: impl AsRef<str>) -> Result<RoutineRunner<Self>, ProviderError>;
pub fn project(&self, slug: impl AsRef<str>) -> Result<&ProjectManifest, ProviderError>;
pub fn model(&self, slug: impl AsRef<str>) -> Result<&ModelManifest, ProviderError>;
pub fn council(&self, slug: impl AsRef<str>) -> Result<&CouncilManifest, ProviderError>;
```

Provider internals should use slug helpers:

```rust
find_agent(slug)
find_project(slug)
find_model(slug)
find_council(slug)
build_agent(slug)
```

Remove these from public and provider-runtime surfaces:

- `agent_by_id`
- `agent_by_name`
- `routine_by_id`
- `project_by_slug`
- `find_project(id)`
- `build_agent_by_id`

Model resolution becomes:

```rust
let model_slug = agent.model.as_ref().ok_or(...)?;
manifest.model(model_slug)
```

## Phase 4: Routine And Council Execution

Update routine execution to resolve by slug:

- agent step: `step.agent` -> `provider.agent(slug)`
- gate step: `step.agent` -> `provider.agent(slug)`
- cron step: config `agent` or step `agent` -> `provider.agent(slug)`
- council step: `step.council` -> `provider.council(slug)`

Update council execution:

- `leader_agent` is a slug
- member list is `Vec<Slug>`
- cycle/delegation tracking should use agent slug, not UUID

Internal runtime events may continue carrying UUIDs if required by platform persistence, but SDK routine/council logic should not require UUID references.

## Phase 5: Prompt Context

Remove UUID prompt vars:

- `agent.id`
- `project.id`
- `routine.id`

Add slug prompt vars:

- `agent.slug`
- `project.slug`
- `routine.slug`
- `model.slug` if useful

Rendered XML for `self`, `project`, and `routine` should not include UUIDs. UUIDs can remain in logs, storage records, and platform events outside model-visible context.

## Phase 6: Platform Translation

Platform/client response structs may still deserialize UUIDs, but conversion into SDK manifests must resolve related slugs first:

- `AgentDetailResponse.model_id` -> `AgentManifest.model`
- `CouncilDetailResponse.leader_agent_id` -> `CouncilManifest.leader_agent`
- council member `agent_id` -> member `agent`
- routine step `agent_id/council_id` -> `agent/council`
- routine ID -> routine slug

If platform detail endpoints do not include related slugs, boundary conversion
derives a deterministic slug from the platform UUID as a temporary transport
fallback. Do not expose those UUID-derived handles to prompt context or provider
public APIs.

## Phase 7: Package And Local Manifests

Package routine manifests already author references as strings:

```yaml
steps:
  - ref: review
    type: agent
    agent: reviewer
```

Preserve these as slugs through package resolution. Remove package conversion code that resolves `agent`, `council`, or routine graph refs into UUIDs.

Local SDK builders should also accept slugs directly.

## Phase 8: Worker Boundary

Worker commands, schedulers, sessions, encryption, and persistence can still carry UUIDs where platform protocol requires them.

Before entering `nenjo::Provider`:

1. Resolve incoming platform UUIDs to slugs using the loaded manifest or platform metadata.
2. Call provider APIs by slug.
3. Emit model-facing context and tool-visible values by slug.

Logs may include both UUID and slug during the transition, but the provider runtime should not depend on UUIDs to find manifest resources.

## Phase 9: Tests

Add focused tests:

- provider exposes `agent`, `routine`, `project`, `model`, and `council` by slug
- removed ID APIs are no longer used in `nenjo` routine/council execution
- agent resolves model by slug
- routine step resolves agent/council by slug
- council leader/member agents resolve by slug
- package routine graph preserves authored slug refs
- prompt vars do not include `agent.id`, `project.id`, or `routine.id`
- rendered prompt XML contains no UUID-looking resource identifiers

Run at minimum:

```text
cargo test -p nenjo provider
cargo test -p nenjo routines
cargo test -p nenjo-worker assembly
cargo test -p nenjo-worker marketplace
cargo clippy -p nenjo --all-targets -- -D warnings
```

Also run platform API tests after endpoint translation changes.
