# Marketplace Installs Plan

## Goal

Nenjo should support installing knowledge packs, skills, and compatible agent
packages from remote marketplaces without storing package contents in the
platform database or object store.

The platform owns catalog metadata, org install intent, policy, and UI state.
Workers own package hydration, local validation, indexing, and runtime
registration.

This keeps the backend as a facilitator instead of a content host.
Install policy is backend-owned. Repos describe packages; they do not author
fields like `read_only` or `is_system`.

## Package Identity

Use collision-proof canonical selectors that include the repo owner:

```text
git://<owner>/<repo>/<package>
git://nenjo-ai/packages/nenjo/platform
repo://nenjo-ai/nenjo-skills/rust-performance
```

Prompt variables should use normalized identifier segments:

```text
{{ git.nenjo_ai.packages.nenjo.platform }}
{{ skill.nenjo_ai.nenjo_skills.rust_performance }}
```

Org-local aliases may be added later for ergonomics, but the canonical selector
must include the owner. Aliases are convenience names, not durable identity.

## Marketplace Catalog

Nenjo should define a native marketplace catalog while supporting package
layouts used by Codex skills/plugins and Claude plugin marketplaces.

Recommended catalog path:

```text
packages.yaml
```

The marketplace manifest is authored source metadata. It should not contain
install-time or release-generated values such as resolved commit SHAs, archive
checksums, installed paths, or hydration timestamps.

Use three metadata layers:

```text
marketplace manifest  authored catalog: what exists, where it lives, default refs
package manifest      package-owned runtime contents, such as SKILL.md or manifest.yaml
install lock metadata platform/worker generated: resolved commit, checksum, installed version
```

Archive checksums belong in generated release manifests or install lock
metadata, not in the authored catalog.

Example:

```json
{
  "schema_version": 1,
  "id": "repo://nenjo-ai/packages",
  "name": "Nenjo Official Packages",
  "description": "Official Nenjo knowledge packs and skills.",
  "publisher": {
    "name": "Nenjo",
    "url": "https://github.com/nenjo-ai"
  },
  "packages": [
    {
      "id": "git://nenjo-ai/packages/nenjo/platform",
      "kind": "knowledge_pack",
      "name": "platform",
      "display_name": "Nenjo Platform",
      "description": "Nenjo platform documentation.",
      "source": {
        "provider": "github",
        "owner": "nenjo-ai",
        "repo": "packages",
        "path": "packs/platform"
      },
      "version_policy": {
        "default_ref": "v0.12.0",
        "allowed_refs": ["tags", "sha"],
        "mutable_refs": false
      },
      "distribution": {
        "type": "github_directory",
        "path": "packs/platform",
        "manifest_path": "manifest.yaml"
      }
    },
    {
      "id": "repo://nenjo-ai/nenjo-skills/rust-performance",
      "kind": "skill",
      "name": "rust-performance",
      "display_name": "Rust Performance",
      "description": "Review and optimize Rust performance.",
      "source": {
        "provider": "github",
        "owner": "nenjo-ai",
        "repo": "nenjo-skills",
        "path": "skills/rust-performance"
      },
      "version_policy": {
        "default_ref": "v0.4.0",
        "allowed_refs": ["tags", "sha"],
        "mutable_refs": false
      },
      "distribution": {
        "type": "github_directory",
        "path": "skills/rust-performance",
        "entrypoint": "SKILL.md",
        "compatibility": ["nenjo_skill", "codex_skill"]
      }
    }
  ]
}
```

Distribution types should be supported in this order:

```text
github_archive    preferred; small, versioned, checksumable release asset
github_directory  direct download of one directory through provider APIs
git_sparse        fallback for private repos or provider API gaps
codex_skill       directory containing SKILL.md
codex_plugin      directory containing .codex-plugin/plugin.json
claude_plugin     directory containing .claude-plugin/plugin.json
```

Workers should avoid full repo clones by default.

### Generated Release Manifests

If a marketplace publishes archive artifacts, CI should generate release
metadata after the archive exists and its checksum can be computed.

Recommended path:

```text
.nenjo/releases/<ref>.json
```

Example:

```json
{
  "schema_version": 1,
  "ref": "v0.12.0",
  "commit": "abc123",
  "artifacts": [
    {
      "package_id": "git://nenjo-ai/packages/nenjo/platform",
      "type": "github_archive",
      "url": "https://github.com/nenjo-ai/packages/releases/download/v0.12.0/platform.tar.gz",
      "sha256": "..."
    }
  ]
}
```

This avoids the checksum chicken-and-egg problem. The authored marketplace
manifest points to source/package locations and default refs. Generated release
metadata points to immutable artifacts and checksums.

## Platform Ownership

The platform stores install metadata only. It must not upload, proxy, or store
remote package contents.

### Knowledge Packs

Extend `knowledge_packs` with source/install fields:

```sql
ALTER TABLE knowledge_packs
  ADD COLUMN source_type TEXT NOT NULL DEFAULT 'uploaded',
  ADD COLUMN read_only BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN is_system BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN metadata JSONB NOT NULL DEFAULT '{}'::jsonb;
```

Existing columns continue to own display and lifecycle state:

```text
id
org_id
slug
name
description
status
created_by
created_at
updated_at
deleted_at
```

For repo-backed packs, `metadata` only needs the install reference and
distribution information required to hydrate the package:

```json
{
  "install": {
    "selector": "git://nenjo-ai/packages/nenjo/platform",
    "kind": "knowledge_pack"
  },
  "source": {
    "provider": "github",
    "owner": "nenjo-ai",
    "repo": "packages",
    "package": "platform"
  },
  "version": {
    "ref": "v0.12.0",
    "resolved_commit_sha": "abc123"
  },
  "distribution": {
    "type": "github_archive",
    "url": "https://github.com/nenjo-ai/packages/releases/download/v0.12.0/platform.tar.gz",
    "sha256": "..."
  }
}
```

Do not create `knowledge_items` rows for repo-backed packs. Those rows are for
backend-owned uploaded content.

### Skills As Abilities

Use `abilities` as the platform primitive for installed skills, but add only
minimal source/install fields:

```sql
ALTER TABLE abilities
  ADD COLUMN source_type TEXT NOT NULL DEFAULT 'native',
  ADD COLUMN read_only BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN metadata JSONB NOT NULL DEFAULT '{}'::jsonb;
```

`is_system` already exists and should continue to mean platform-seeded ability.

Map a skill-backed ability into existing columns:

```text
name                  normalized skill name
tool_name             unique callable ability tool name
path                  grouping path, e.g. repo/nenjo_ai/nenjo_skills
display_name          copied from catalog/SKILL.md for UI
description           copied from catalog/SKILL.md for UI
activation_condition  copied from SKILL.md description/use-when text
prompt_config         empty placeholder or fallback wrapper
platform_scopes       explicit platform scopes, default {}
mcp_server_ids        explicit MCP dependencies, default {}
tool_filter           optional tool policy
is_system             true for Nenjo-seeded skills
read_only             true for marketplace-installed skills unless forked
source_type           native | skill
metadata              install reference and distribution pointer
```

Minimal skill metadata:

```json
{
  "install": {
    "selector": "repo://nenjo-ai/nenjo-skills/rust-performance",
    "kind": "skill"
  },
  "source": {
    "provider": "github",
    "owner": "nenjo-ai",
    "repo": "nenjo-skills",
    "path": "skills/rust-performance"
  },
  "version": {
    "ref": "v0.4.0",
    "resolved_commit_sha": "def456"
  },
  "distribution": {
    "type": "github_archive",
    "url": "https://github.com/nenjo-ai/nenjo-skills/releases/download/v0.4.0/rust-performance.tar.gz",
    "sha256": "..."
  }
}
```

Do not duplicate all skill metadata into the DB. The sourced package remains
the source of truth for `SKILL.md`, references, scripts, assets, and compatibility
metadata.

### Plugin MCP Servers

Claude plugins can also produce Nenjo MCP server rows. MCP servers need the same
minimal marketplace ownership fields as abilities:

```sql
ALTER TABLE mcp_servers
  ADD COLUMN source_type TEXT NOT NULL DEFAULT 'native',
  ADD COLUMN read_only BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN metadata JSONB NOT NULL DEFAULT '{}'::jsonb;
```

Map Claude plugin MCP config into existing MCP columns:

```text
name          normalized plugin/server name
display_name  plugin and server display name
description   copied from marketplace/plugin metadata
transport     stdio | http
command       Claude mcpServers.<name>.command for stdio
args          Claude mcpServers.<name>.args for stdio
url           Claude mcpServers.<name>.url for http
env_schema    keys from Claude mcpServers.<name>.env
source_type   native | plugin
read_only     true for marketplace-installed plugin MCPs
metadata      install reference, source, version, original Claude MCP config
```

Abilities created from plugin `skills/*/SKILL.md` should attach plugin MCPs by
writing the existing `abilities.mcp_server_ids` relationship. This keeps the
runtime chain native to Nenjo:

```text
agent -> assigned ability -> ability.mcp_server_ids -> MCP tools/resources
```

## Skill Runtime Mapping

Every installed skill produces one runtime ability.

Worker mapping:

```text
ability row with source_type = skill
  -> hydrate package
  -> read SKILL.md
  -> parse frontmatter/body
  -> synthesize runtime AbilityManifest
  -> expose assigned ability tool
```

Use existing ability fields for assignment, activation, scopes, and UI. The
runtime prompt body should come from the hydrated `SKILL.md`, not from backend
content storage.

References in a skill should initially be exposed as read-only, skill-scoped
files rather than forced into knowledge packs:

```text
list_skill_files(skill)
search_skill_files(skill, query)
read_skill_file(skill, path)
```

These tools must be sandboxed to the installed skill directory and should only
be available to the active skill execution context. A future package may declare
a companion knowledge pack, but ordinary skill references should remain simple
local support files.

## Worker Filesystem Layout

All marketplace-managed packages should live outside the user workspace.

Recommended worker root:

```text
$NENJO_HOME/
  marketplace/
    catalogs/
    downloads/
  library/
    repos/
      github/
        nenjo-ai/
          packages/
            platform/
              v0.12.0/
                manifest.json
                docs/
  skills/
    repos/
      github/
        nenjo-ai/
          nenjo-skills/
            rust-performance/
              v0.4.0/
                SKILL.md
                references/
                scripts/
                assets/
  plugins/
    repos/
      github/
        owner/
          repo/
            plugin/
              version/
```

`marketplace/downloads` is an optional archive cache. Workers may discard
archives after extraction if disk simplicity is preferred.

The workspace remains for user/project work. Marketplace packages are managed
runtime dependencies and should not appear in normal project file search unless
accessed through explicit knowledge or skill-scoped tools.

## Worker Hydration

For each installed package:

```text
1. Read install metadata from bootstrap/API.
2. Resolve distribution.
3. Download archive or directory without full clone when possible.
4. Verify sha256 when present.
5. Extract into a temporary directory.
6. Validate package shape.
7. Normalize into the runtime layout.
8. Atomically move into the final versioned directory.
9. Write a local install marker.
10. Register package with the runtime before provider assembly.
```

Validation rules:

```text
knowledge_pack  requires manifest.yaml/json
skill           requires SKILL.md
codex_plugin    requires .codex-plugin/plugin.json
claude_plugin   requires .claude-plugin/plugin.json
```

All package paths referenced by manifests must remain inside the extracted
package root.

For Claude plugin MCPs, the worker hydrates the plugin directory under
`$NENJO_HOME/plugins/repos/...` before connecting the MCP server. It substitutes
`${CLAUDE_PLUGIN_ROOT}` and `${CLAUDE_PLUGIN_DATA}` in command, args, URL, and
stored metadata, then starts the MCP through Nenjo's normal external MCP pool.

## Platform API

Add marketplace and install endpoints around metadata:

```text
GET  /api/v1/marketplaces
POST /api/v1/marketplaces
POST /api/v1/marketplaces/{id}/install/claude-plugin

POST /api/v1/knowledge/install
POST /api/v1/abilities/install
PATCH /api/v1/skills/{id}
DELETE /api/v1/skills/{id}
```

The backend stores marketplace sources and installed resource references only.
The dashboard fetches public marketplace manifests and plugin details directly
from GitHub for browsing/preview. Install endpoints validate the frontend
provided package summary and normalized plugin skill/MCP declarations, enforce
org uniqueness, and write install records. They should not upload package files
or persist full marketplace/plugin manifests.

After install, update, or uninstall, platform should emit the same worker sync
signal used for manifest/bootstrap changes.

## Dashboard UX

The dashboard should load marketplace sources from the platform. During
onboarding, the official `nenjo-ai/packages` source should be added by calling
the same marketplace-source API used for user-imported sources, then selected
packages should be installed through the normal install APIs. The dashboard
fetches public marketplace package data directly from GitHub, caches it
locally/SWR, and writes only install records through the platform API.

Knowledge pack card:

```text
Nenjo Platform
Source: GitHub · nenjo-ai/packages
Version: v0.12.0
Ref: git://nenjo-ai/packages/nenjo/platform
System · Read-only
```

Skill card:

```text
Rust Performance
Source: GitHub · nenjo-ai/nenjo-skills
Version: v0.4.0
Ability: rust_performance
System/User installed · Enabled/Disabled
```

System packages should be visible but not destructively editable. User-installed
packages may support uninstall, enable/disable, alias changes, and version
updates.

## Onboarding Nenjo

Add the official `nenjo-ai/packages` marketplace source through the platform API
and install official packages through the same endpoints used by normal
marketplace installs. Do not upload package files or create uploaded document
rows, and do not insert this source or its installs directly through DB seed
migrations.

Official marketplace source API payload:

```text
marketplace_sources.name = Nenjo Official Packages
marketplace_sources.source_type = github
marketplace_sources.status = active
metadata.provider = github
metadata.owner = nenjo-ai
metadata.repo = packages
metadata.ref = v0.12.0
metadata.manifest_path = packages.yaml
```

Platform knowledge install API payload:

```text
knowledge_packs.slug = platform
knowledge_packs.name = Nenjo Platform
knowledge_packs.source_type = github
metadata.install.selector = git://nenjo-ai/packages/nenjo/platform
metadata.install.marketplace_source_id = <source-id>
metadata.version.ref = v0.12.0
```

Official skill install API payload:

```text
abilities.name = rust_performance
abilities.tool_name = rust_performance
abilities.path = repo/nenjo_ai/nenjo_skills
abilities.source_type = skill
metadata.install.selector = repo://nenjo-ai/nenjo-skills/rust-performance
metadata.version.ref = v0.4.0
```

The backend can still apply install policy such as read-only handling for
official packages, but the flow remains API-driven. Do not keep a legacy
built-in compatibility alias. New prompts, UI, and installed records should
use the canonical `git://nenjo-ai/packages/nenjo/platform` selector.

## Rollout

1. Add marketplace source/package metadata APIs.
2. Add `source_type`, `read_only`, `is_system`, and `metadata` to
   `knowledge_packs`.
3. Add `source_type`, `read_only`, and `metadata` to `abilities`.
4. Add onboarding calls that create the official `nenjo-ai/packages`
   marketplace source and install official packages through the public APIs.
5. Update dashboard library/skills UI to show source, version, selector, system,
   and read-only state.
6. Add worker downloader/extractor/validator.
7. Add repo-backed knowledge-pack hydration.
8. Add skill-backed ability hydration from `SKILL.md`.
9. Add Claude marketplace sync and plugin install adapter.
10. Add plugin MCP hydration and ability attachment.
11. Add skill-scoped read/search file tools.
12. Add Codex skill/plugin compatibility adapters.
13. Move embedded Nenjo docs into the official `nenjo-ai/packages` platform package.
14. Remove embedded built-in docs and builtin prompt variables from runtime registration.
