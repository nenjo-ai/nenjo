# Nenpm Package And Module Plan

Nenjo packages should follow the same broad shape as npm and Cargo packages:
a package is the versioned dependency and distribution unit, while modules are
manifest files inside that package. Runtime behavior is inferred from each
module manifest schema.

## Model

```text
Repository
  contains many packages

Package
  versioned dependency unit

Module
  manifest file included by a package

Resource/source behavior
  inferred from each module's schema

Imports
  local runtime composition between modules
```

Packages are generic in v1. There are no separate package types for resources,
knowledge, sources, or future module families. The resolver loads modules and
dispatches by manifest schema.

## Repository Manifest

```yaml
schema: nenjo.registry.v1
packages:
  "@nenjo-ai/core": packages/core/nenjo.package.yaml
  "@nenjo-ai/nenji": packages/nenji/nenjo.package.yaml
```

## Package Manifest

```yaml
schema: nenjo.package.v1
name: "@nenjo-ai/nenji"
version: "0.1.0"
description: Nenjo platform guide agent.

dependencies:
  "@nenjo-ai/core": "^0.1.0"

modules:
  - agent.yaml
```

Rules:

- `dependencies` are package-level only.
- `modules` are package-relative root entrypoints. The resolver follows local
  wrapper-level imports transitively from those roots.
- Directory references must contain `index.yml` or `index.yaml`; the resolver
  expands that explicit index and never imports a directory by implicit file
  listing.
- Runtime kind is inferred from module manifest `schema`.
- Runtime name is inferred from `manifest.name`.

Module files may contain a single resource manifest or a `nenjo.modules.v1`
bundle:

```yaml
schema: nenjo.module_index.v1
modules:
  - design_agent.yaml
  - diagnose_failure.yaml
```

Index entries are relative to the index file directory and can point at nested
directories that also contain an index. Index cycles are rejected.

```yaml
schema: nenjo.modules.v1
resources:
  - schema: nenjo.ability.v1
    manifest:
      name: design_agent
  - schema: nenjo.ability.v1
    manifest:
      name: diagnose_failure
```

Resources can declare structured runtime imports:

```yaml
schema: nenjo.agent.v1
imports:
  abilities:
    - ./capabilities/design/
  context:
    - ./shared/context/methodology.yml
manifest:
  name: nenji
```

Imports are local module refs only. Package dependencies are declared once at
the package level in `dependencies`.

## Prompt Selectors

`pkg.*` is only a prompt/template selector namespace. It is not the general
package module import syntax.

In v1, package prompt selectors are used for:

- package-installed context blocks, for example
  `{{ pkg.nenjo.core.methodology }}`
- package-installed knowledge pack indexes and document metadata, for example
  `{{ pkg.nenjo.core.knowledge }}` and
  `{{ pkg.nenjo.core.knowledge.guide.agents }}`

Agents, abilities, domains, routines, MCP servers, and future runtime resources
are resolved through package dependencies, module paths, and explicit wrapper
imports. They do not become prompt variables just because they were installed
from a package.

## Runtime Dispatch

```text
nenjo.agent.v1         -> install agent resource
nenjo.ability.v1       -> install ability resource
nenjo.domain.v1        -> install domain resource
nenjo.context_block.v1 -> register prompt context block
nenjo.knowledge.v1     -> register knowledge/source surface
nenjo.skill.v1         -> install/export skill
nenjo.plugin.v1        -> install plugin
nenjo.mcp_server.v1    -> install MCP server
nenjo.routine.v1       -> install routine
future nenjo.source.v1 -> dynamic/RAG source provider
```

Package resolution should not need changes when a new module schema appears.
Only the importer/registrar layer needs to learn how to handle the new schema.

## Nenpm Responsibilities

`nenjo-packages` owns pure format and resolution primitives:

- parse package and registry manifests
- validate module paths and imports
- infer module schema, kind, and runtime name
- build dependency-first package/module graphs
- expose lockfile records
- provide a local filesystem resolver for tests and local package development

`nenpm` should own package-manager behavior:

- registry client
- source fetching
- install planning
- cache layout
- lockfile IO
- invoking platform/worker importers
- publish/search/info/add/update/remove commands

The runtime should consume installed, locked resources. It should not resolve
semver while an agent is executing.

## Dependency Manifest

Projects declare install roots in `nenpm.yml` or `nenpm.yaml`:

```yaml
schema: nenjo.dependencies.v1

dependencies:
  "@nenjo-ai/nenji": "^0.1.0"

registries:
  - https://registry.nenjo.ai/index.yaml

overrides:
  "@nenjo-ai/core": file:../packages#nenjo/core.package.yaml
```

`nenpm.yml` is preferred, but both extensions are supported. If both files exist
in the same directory, the loader fails with a clear ambiguity error.

Registries are ordered. The first registry containing a requested package wins.
GitHub-backed repository registries expose unscoped `packages.yaml` entries
under the package scope derived from the GitHub org.

Overrides support structured package sources and `file:` shorthand. The
shorthand form is:

```text
file:<root>#<manifest_path>
```

When `#<manifest_path>` is omitted, it defaults to `packages.yaml`, treating the
root as a local registry.

`nenpm install` resolves the dependency manifest, fetches package sources,
builds the package/module graph, writes `nenpm.lock.yml`, and materializes the
resolved package sources under `.nenjo/packages/<scope>/<name>@<version>` by
default. It also writes `.nenjo/packages/.nenpm-index.json`, which maps each
locked `name@version` to its installed package root. `--packages-dir` overrides
the final package install directory when callers need global or platform-managed
package trees. `--dry-run` performs the same resolution without writing the lockfile or
package tree. Overrides take precedence for local development. Dependencies
without overrides resolve from the ordered `registries` list; the first registry
containing the requested package wins.
`--locked` requires `nenpm.lock.yml` to exist and match the resolved graph; it is
the mode used by worker bootstrap when the platform supplies both dependency and
lock files.

Registry packages use registry metadata to compute the full dependency graph
before download. Selected registry sources are fetched concurrently with a
bounded concurrency limit, and downloaded package manifests must verify the
registry package name, version, and dependency metadata.

`install` preserves versions pinned in `nenpm.lock.yml` when they still satisfy
the dependency manifest. `update` intentionally ignores those pins and
re-resolves from the registry. `add` and `remove` edit the dependency manifest
and then install; `list` reads the lockfile; `info` reads package metadata from
the configured default registry.

The lockfile is also an integrity input. It records source metadata, requested
dependency ranges, exact resolved dependency versions, package manifest hashes,
and module hashes. Normal `install` verifies reused non-local pins; local
sources remain mutable development inputs. Artifact source checksums verify
archive bytes, remote source checksums verify manifest bytes, and the
version-level registry checksum verifies the package manifest hash.

## Registry Sources

The registry should be the discovery and version authority. It can point each
resolved package version at different source kinds:

```yaml
schema: nenjo.registry.v1
packages:
  "@nenjo-ai/nenji":
    - version: "0.1.0"
      source:
        kind: git
        url: https://github.com/nenjo-ai/packages.git
        reference: v0.1.0
        manifest_path: nenjo/nenji.package.yaml
      dependencies:
        "@nenjo-ai/core": "^0.1.0"
```

```text
git       remote git repo/ref/path
artifact  immutable registry-cached tarball or zip
remote    direct HTTPS manifest source
local     local checkout for tests and package development
```

The default source fetcher supports all four source kinds. Tests exercise local,
git, artifact, and direct remote manifest resolution without external network
access. File-backed registry indexes can use relative `local.root`,
`artifact.url`, and `remote.url` values; they resolve relative to the registry
index file. The next package-manager layer should add cache layout, registry
HTTP publishing APIs, and platform import execution.
