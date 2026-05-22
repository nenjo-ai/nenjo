# Claude Marketplace And Skill Adapter Plan

## Purpose

Nenpm should be able to install Claude plugin marketplaces and normal Agent
Skills out of the box, while preserving Nenjo's stronger package-manager
semantics.

Target user experience:

```text
nenpm add @anthropic/marketplace
nenpm add @vercel/skills
```

The command should add the appropriate registry source, resolve packages,
materialize them into the normal package cache, write lockfile metadata, and
make adapted resources available to the worker/runtime.

The adapter should treat Claude and Agent Skills as source formats. Nenjo stays
responsible for resolution, lockfiles, materialization, indexing, and runtime
resource loading.

```text
Claude marketplace/plugin = source/adaptor format
Agent Skills / SKILL.md   = source/adaptor format
nenpm                     = resolver, lockfile, materializer, runtime indexer
Nenjo SDK/worker          = native runtime
```

## Source Formats

### Agent Skills / Vercel-Style Skills

Agent Skills are directories with an entrypoint `SKILL.md`.

Typical layout:

```text
skill-name/
  SKILL.md
  references/
  scripts/
  assets/
```

`SKILL.md` has YAML frontmatter:

```yaml
---
name: react-best-practices
description: Use when writing or reviewing React/Next.js code for performance.
license: Apache-2.0
compatibility: Requires Node.js 22+
allowed-tools: Bash(npm:*) Read
metadata:
  author: example-org
---
```

Core behavior is progressive disclosure:

```text
1. expose compact name + description in the skill catalog
2. activate skill when relevant
3. load SKILL.md body into context
4. read references/scripts/assets only when needed
```

### Claude Plugin Marketplaces

Claude marketplace repositories include marketplace metadata and one or more
plugins.

Typical layout:

```text
.claude-plugin/marketplace.json
plugins/
  some-plugin/
    .claude-plugin/plugin.json
    skills/
    commands/
    agents/
    hooks/
    monitors/
    bin/
    .mcp.json
    .lsp.json
    settings.json
```

Claude plugins can include dependency metadata and versions, but Nenjo should
not inherit Claude's weaker installer semantics. Nenpm should translate useful
metadata into the normal Nenjo dependency graph and lockfile.

## Registry Configuration

Nenpm registries should support an adapter field:

```yaml
registries:
  - source:
      kind: github
      repo: anthropic/marketplace
      ref: main
    adapter: claude

  - source:
      kind: github
      repo: vercel/skills
      ref: main
    adapter: agent_skill
```

Convenience commands may infer the adapter for known sources:

```text
nenpm add @anthropic/marketplace
nenpm add @vercel/skills
```

Registry scope rules remain Nenjo rules:

```text
- GitHub-backed registries derive scope from the GitHub org/owner
- packages are installed under that scope
- repository package metadata must not declare arbitrary scopes
- local registries may allow explicit local-only scope for testing
```

## Package Manager Semantics

Nenpm should fill the gaps in Claude's plugin installer:

```text
- strong lockfile with resolved refs, SHAs, versions, checksums
- deterministic installs across workers
- transitive dependency resolution
- reference-counted prune/remove behavior
- cache reuse
- multi-version coexistence via @scope/name@version
- unified install paths for native Nenjo packages and adapted external content
- runtime indexes for worker/dashboard consumption
```

Claude plugin dependency metadata should be treated as input to the Nenjo
resolver, not copied as-is into runtime behavior.

## Native Resource Mapping

The adapter should produce native Nenjo resources where the mapping is clear.

```text
Agent Skills / SKILL.md       -> nenjo.skill.v1
Claude plugin skills/*        -> nenjo.skill.v1
Claude plugin agents/*.md     -> nenjo.agent.v1
Claude plugin commands/*.md   -> nenjo.domain.v1
Claude plugin MCP config      -> nenjo.mcp_server.v1
Claude bin/hooks/monitors     -> preserved on disk, inactive in v1
Claude LSP/themes/settings    -> preserved as unsupported metadata in v1
```

Important decision:

```text
Claude agents are normal Nenjo agents, not abilities.
```

Reasoning:

```text
Agent manifest = program definition
Sub-agent = runtime child task/thread using an agent manifest
```

Claude "subagents" are still agent definitions. In Nenjo they can be:

```text
- chatted with directly
- spawned as sub-agents
- used in future councils/routines
```

See `docs/sub-agent-runtime-spec.md` for the native sub-agent runtime model.

## Native Skill Primitive

Nenjo should make skills native rather than forcing marketplace skills into
abilities.

Conceptual split:

```text
Ability
= assigned to an agent/domain
= exposed as individual tool calls
= structured execution surface
= can add scopes, MCP assignments, policies, prompt overlays
= curated and intentional

Skill
= installed package prompt asset
= available through a catalog
= activated by native skill mechanism
= loaded progressively
= preserves SKILL.md/reference/script layout
= no platform scopes by default
```

Canonical skill manifest:

```yaml
schema: nenjo.skill.v1
manifest:
  name: react_best_practices
  display_name: React Best Practices
  description: Use when writing, reviewing, or refactoring React/Next.js code for performance.
  entrypoint: SKILL.md
  metadata:
    adapter:
      kind: agent_skill
      source_path: skills/react-best-practices/SKILL.md
```

The runtime should expose a native skill activation surface, similar to Claude's
single `Skill` tool and Codex-style skill activation.

Possible future model-facing surface:

```text
activate_skill
```

or:

```text
use_skill
```

The skill catalog should expose compact metadata only:

```text
name
description
package
entrypoint
```

Full `SKILL.md` content should be loaded only when the skill is activated.

## Claude Agent Mapping

Claude plugin `agents/*.md` should map to `nenjo.agent.v1`.

Claude agent frontmatter may include:

```yaml
---
name: security-reviewer
description: Use for focused security review.
model: sonnet
effort: medium
maxTurns: 20
disallowedTools:
  - Write
  - Edit
---
```

Nenjo package-authored output:

```yaml
schema: nenjo.agent.v1
manifest:
  name: security_reviewer
  display_name: Security Reviewer
  description: Use for focused security review.
  prompt_config:
    system_prompt: |-
      <Claude agent markdown body>
    developer_prompt: ""
  metadata:
    adapter:
      kind: claude
      component: agent
      source_path: agents/security-reviewer.md
      model_hint: sonnet
      effort_hint: medium
      max_turns: 20
      tool_policy:
        disallowed:
          - Write
          - Edit
```

Model hints are advisory. Imported Claude agents should inherit the configured
Nenjo/session model by default. The adapter should not create a hard Anthropic
model dependency.

## Claude Command Mapping

Claude plugin `commands/*.md` should map to `nenjo.domain.v1`.

Reasoning:

```text
Claude command = explicit user-invoked surface
Nenjo domain   = explicit user-activated mode/command surface
```

Adapter output:

```yaml
schema: nenjo.domain.v1
manifest:
  name: deploy_review
  display_name: Deploy Review
  description: Review deployment readiness.
  command: deploy_review
  prompt_config:
    developer_prompt_addon: |-
      <Claude command markdown body>
  metadata:
    adapter:
      kind: claude
      component: command
      source_path: commands/deploy-review.md
```

## MCP Mapping

Nenjo already has native MCP manifests, including env schema requirements.

Claude plugin MCP config should map to `nenjo.mcp_server.v1` where possible.

```text
Claude .mcp.json / plugin mcpServers
-> nenjo.mcp_server.v1
```

Preserve source metadata:

```yaml
metadata:
  adapter:
    kind: claude
    component: mcp
    source_path: .mcp.json
```

## Unsupported Claude Components In V1

Do not enable these as active runtime behavior in v1:

```text
bin/
hooks/
monitors/
.lsp.json
themes/
settings.json
```

They should be preserved on disk and recorded in adapter metadata as unsupported
or inactive.

Reasoning:

```text
- bin changes PATH behavior
- hooks can execute commands or trigger runtime side effects
- monitors/LSP/themes/settings are Claude-specific runtime integrations
- enabling them silently would bypass Nenjo's permission model
```

Future support can add typed native resources for these behaviors.

## Runtime File Projection

Packages live outside the workspace:

```text
~/.nenjo/packages/@scope/name@version
.nenjo/packages/@scope/name@version
```

Skills expect relative file layouts:

```text
SKILL.md
references/
scripts/
assets/
```

The runtime should preserve native pathing. Do not rewrite prompts and do not
inject custom path instructions just to make references work.

Preferred v1 approach:

```text
copy the needed skill/plugin root into a temporary runtime scope
set the active skill/plugin root as the working/resource root
clean up with Drop
also clean stale runtime scopes on startup
```

Example source:

```text
~/.nenjo/packages/@vercel/skills@sha/react/
  SKILL.md
  references/
  scripts/
```

Runtime projection:

```text
workspace/.nenjo/runtime/<run_id>/skills/react/
  SKILL.md
  references/
  scripts/
```

If `SKILL.md` says:

```text
Read references/foo.md
Run scripts/check.sh
```

those paths should work relative to the projected skill root without rewriting.

Copy `SKILL.md` even when its body is already loaded into the skill manifest.
This keeps the source layout faithful and avoids special cases.

Prompt rule:

```text
- load SKILL.md body once when the skill activates
- do not inject SKILL.md twice
- copy SKILL.md to runtime projection for path/layout compatibility
```

Cleanup:

```rust
pub struct RuntimeSkillProjection {
    root: PathBuf,
}

impl Drop for RuntimeSkillProjection {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
```

Drop cleanup is best-effort. Add startup/periodic cleanup for old
`.nenjo/runtime/*` directories.

## Adapter Metadata

Use namespaced metadata to avoid collisions with existing runtime metadata.

For skills:

```yaml
metadata:
  adapter:
    kind: claude
    component: skill
    source_path: skills/react/SKILL.md
    skill_root: skills/react
```

For agents:

```yaml
metadata:
  adapter:
    kind: claude
    component: agent
    source_path: agents/security-reviewer.md
    model_hint: sonnet
    effort_hint: medium
    max_turns: 20
    tool_policy:
      disallowed:
        - Write
        - Edit
```

For unsupported components:

```yaml
metadata:
  adapter:
    kind: claude
    unsupported:
      - component: hooks
        path: hooks/hooks.json
        reason: Nenjo hook runtime is not implemented in v1.
      - component: bin
        path: bin/
        reason: Nenjo PATH projection for package executables is not implemented in v1.
```

Avoid using `metadata.runtime` for adapter fields. Existing code already reads
some ability metadata under `runtime/env_names`.

## Installed Layout

Native and adapted packages should use the same package materialization layout:

```text
<packages_dir>/.nenpm-index.json
<packages_dir>/@scope/name@version/
```

For adapted packages, preserve raw source content and generated Nenjo index
metadata.

Example:

```text
.nenjo/packages/
  @anthropic/marketplace@<sha>/
    raw/
      .claude-plugin/marketplace.json
      plugins/security/
        .claude-plugin/plugin.json
        agents/security-reviewer.md
        skills/...
        commands/...
        .mcp.json
    adapted/
      nenjo.package.yaml
      skills/
      agents/
      domains/
      mcp/
```

The exact internal raw/adapted layout can evolve, but the runtime index should
always point to concrete package-relative source paths.

## Worker And Platform Behavior

Worker/local behavior:

```text
- nenpm installs packages into package dirs
- worker package loader indexes native/adapted resources
- skills are available through native skill catalog
- imported Claude agents are normal package agents
- imported Claude domains are normal package domains
- imported Claude MCP servers are normal package MCP servers
```

Platform behavior:

```text
- platform should send nenpm.yml/lock data to worker for platform-managed package installs
- worker performs install/materialization
- package resources remain read-only
- dashboard can display imported resources from package loaders
```

Do not require the backend to git clone Claude marketplaces directly. Fetching
belongs in nenpm.

## Open Design Items

Native skill runtime:

```text
- exact tool name: activate_skill vs use_skill
- whether all agents get skill activation by default
- how the skill catalog is compacted/retrieved for large marketplaces
```

Tool policy:

```text
- Claude allowedTools/disallowedTools can be preserved in metadata now
- later promote to native tool policy when Nenjo has that schema/runtime support
```

PATH/bin support:

```text
- not active in v1
- future support could add scoped PATH projection for active skill/plugin roots
```

Hooks:

```text
- not active in v1
- future support should be a typed nenjo.hook.v1 resource with explicit permission model
```

## Test Plan

Add tests for:

```text
Agent Skill directory adapts to nenjo.skill.v1
SKILL.md frontmatter name/description parsing
skill root preserves references/scripts/assets paths
Claude marketplace registry is detected
Claude plugin metadata is parsed
Claude skills adapt to nenjo.skill.v1
Claude agents adapt to nenjo.agent.v1
Claude commands adapt to nenjo.domain.v1
Claude MCP config adapts to nenjo.mcp_server.v1
bin/hooks/monitors are preserved but inactive
adapter metadata is namespaced under metadata.adapter
model hints are advisory and do not create required model refs
adapted package installs into normal packages_dir layout
lockfile records source ref/SHA/checksum for adapted packages
runtime projection copies SKILL.md plus references/scripts without prompt rewriting
projection Drop cleanup removes runtime copy
startup cleanup removes stale runtime projections
```
