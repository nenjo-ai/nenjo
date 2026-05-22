# nenpm Package Cleanup Execution Plan

## Goal

Make the package manager and package schema code canonical for the new package/module model without preserving legacy compatibility. The implementation should make package invariants explicit, keep install/runtime logic modular, and add tests for the edge cases most likely to break platform, worker, and local package installs.

## Canonical Rules

- Package dependencies are declared only at the package/dependency manifest level.
- Module imports are local module/resource imports only.
- Resource manifests stay pure; package-derived fields such as resource `path` are not authored.
- Directory imports resolve through an explicit `index.yml` or `index.yaml`.
- Installed packages are materialized under `packages/<scope>/<name>/<version>` and indexed by lockfile metadata.
- Worker package resources are read-only runtime resources derived from installed package lock/index state.

## Implementation Plan

1. Split package crate domains.
   - Move schema/version/kind/adapter parsing into a schema module.
   - Move package and registry manifest structs into package/registry modules.
   - Move module wrapper, bundle, index, imports, and resource manifest parsing into a module module.
   - Move resolved graph types into a resolved module.
   - Move source path, package name, resource key, and version helper logic into identity/path modules.
   - Keep public exports stable from `nenjo-packages` through `pub use`.

2. Split nenpm install internals.
   - Keep `install()` as the orchestrator.
   - Move package directory materialization and pruning into an install materialization module.
   - Move lockfile integrity comparison into an install integrity module.
   - Move configured registry selection into a dedicated install registry config module.
   - Keep CLI and public API behavior canonical: `install --root .` default, lockfile pinning, checksums, and package materialization.

3. Strengthen validation tests.
   - Reject authored `path` for ability/domain/context_block resources.
   - Reject `manifest.imports` inside module bundles as well as single-resource module files.
   - Reject directory imports without an index file.
   - Detect local module import cycles involving directory indexes.
   - Verify package root escape attempts in imports fail.

4. Strengthen install tests.
   - Verify `.git` directories are never copied into installed packages.
   - Verify locked installs work with repository-style registries.
   - Verify updated lockfiles materialize changed package versions and prune unreferenced versions.

5. Strengthen worker package runtime tests.
   - Verify package-derived paths for ability, domain, and context_block resources.
   - Verify authored paths are overwritten defensively.
   - Verify package instance keys preserve coexisting package versions.

6. Performance cleanup.
   - Keep rayon-based parallelism for package materialization.
   - Avoid copying `.git`.
   - Isolate materialization so future source-cache/file-set optimization can be added without touching resolver logic.

## Verification

- `cargo test -p nenjo-packages`
- `cargo test -p nenjo-nenpm`
- `cargo test -p nenjo-worker package_manifests`
- `cargo check -p nenjo-packages -p nenjo-nenpm -p nenpm-cli -p nenjo-worker`
- `cargo clippy -p nenjo-packages -p nenjo-nenpm -p nenpm-cli -p nenjo-worker`
