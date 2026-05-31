# Platform Package Sync Plan

## Goal

Dashboard-installed Nenjo packages should be plug-and-play in the platform while
the worker materializes package contents through `nenpm`.

The platform owns desired package state and dashboard-visible handles. The
worker owns package download, verification, and runtime loading.

## Package Roots

```text
<project>/nenpm.yml                 # project-local packages, highest precedence
~/.nenjo/platform_pkgs/nenpm.yml     # platform/dashboard-managed packages
~/.nenjo/packages/nenpm.yml          # user global packages
~/.nenjo/manifests/                  # normal platform-authored resources
```

Runtime package precedence:

```text
local > platform > global
```

## Identity

Use two identities.

Logical key, used for platform-visible resource IDs:

```text
pkg:<logical-package-name>:<resource-kind>:<module-path>#<resource-name>
```

Resolved instance key, used for provenance and lock/cache/debugging:

```text
pkg:<real-package-name>@<version>:<resource-kind>:<module-path>#<resource-name>
```

Logical IDs intentionally do not include version, org id, source id, or install
id. Package upgrades should keep dashboard/routine/chat references stable.
Version belongs in metadata.

Side-by-side versions require explicit aliases in `nenpm.yml`; aliases become
the logical package name for those installed instances.

## Platform Responsibilities

- Keep normal package install tables for desired state, audit, and dashboard UX.
- Create deterministic read-only DB handles in normal resource tables so package
  agents/abilities/domains/routines are selectable and referenceable.
- Mark package handles `source_type = package` and `read_only = true`.
- Store the resolved package dependency contribution in the package install lock
  when a package is installed: package requirements plus the registry source
  needed to resolve them.
- Generate one inline `nenpm.yml` per platform-managed package root by merging
  current install locks into a single `registries` list and `dependencies` map,
  then run the normal `nenpm` resolver to produce `nenpm.lock.yml`.
- Do not make copied DB manifest content the runtime source of truth.

## Worker Responsibilities

- Save bootstrap inline `nenpm.yml` to `~/.nenjo/platform_pkgs/nenpm.yml` and
  `nenpm.lock.yml` to `~/.nenjo/platform_pkgs/nenpm.lock.yml`.
- Run `nenpm install --locked` in-process for `~/.nenjo/platform_pkgs` so all
  workers use the platform-selected dependency graph.
- Load package resources from package roots through a shared package manifest
  loader.
- Merge package manifests with normal platform manifests using package
  precedence.

## Dashboard Responsibilities

- Treat `source_type = package` as managed/read-only.
- Continue using DB handles for chat, routine, assignment, and picker UX.
- Display package name, version, and provenance from metadata.

## Implementation Order

1. Add shared logical/instance key and deterministic ID helpers.
2. Update platform package install rows to use stable IDs and `source_type = package`.
3. Save lock-backed platform package dependency manifests.
4. Add worker bootstrap write/install to `~/.nenjo/platform_pkgs`.
5. Add package manifest loader.
6. Change manifest merge to upsert by ID.
7. Wire global/platform/local package loaders into worker runtime.
8. Add alias support to `nenpm`.
9. Adjust dashboard filters/edit guards for `source_type = package`.
10. Add incremental package sync events.
