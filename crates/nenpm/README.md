# nenjo-nenpm

`nenjo-nenpm` is the package-manager layer for Nenjo packages.

The crate currently turns resolved package graphs from `nenjo-packages` into
install plans. It intentionally does not import resources into platform storage
yet; that boundary belongs to the future installer/importer layer.

## Local Install Planning

```rust
use nenjo_nenpm::InstallPlan;

# fn example() -> anyhow::Result<()> {
let plan = InstallPlan::from_local_repository("../packages", "@nenjo/nenji")?;

for package in plan.packages() {
    println!("{}@{}", package.name, package.version);
    for module in package.modules {
        println!("{} {} {}", module.kind.as_str(), module.name, module.path);
    }
}
# Ok(())
# }
```

## Dependency Manifest

Projects can declare package dependencies in `nenpm.yml` or `nenpm.yaml`.
`nenpm.yml` is the preferred name, but both are supported. If both exist in the
same directory, loading fails so resolution is never ambiguous.

```yaml
schema: nenjo.dependencies.v1

dependencies:
  "@nenjo/nenji": "^0.1.0"
  "@nenjo/coding": "^0.1.0"

dev_dependencies:
  "@acme/test-agent": "^0.3.0"

registries:
  default: https://registry.nenjo.ai/index.yaml

overrides:
  "@nenjo/core": file:../packages#nenjo/core.package.yaml
  "@acme/test-agent":
    kind: local
    root: ../test-packages
    manifest_path: packages/test-agent/nenjo.package.yaml
```

Override values can use structured `PackageSource` objects or the `file:`
shorthand:

```yaml
overrides:
  "@nenjo/core": file:../packages
  "@nenjo/nenji": file:../packages#nenjo/nenji.package.yaml
```

Without a `#manifest_path`, `file:` defaults to `packages.yaml`, which treats
the path as a local package repository root.

The CLI installs the manifest by resolving package sources and writing
`nenpm.lock.yml`:

```bash
cargo run --bin nenpm -- install --root .
```

`install` preserves package versions already pinned in `nenpm.lock.yml` when
they still satisfy `nenpm.yml`. Use `update` to intentionally re-resolve from
the registry and rewrite those pins:

```bash
cargo run --bin nenpm -- update --root .
```

The lockfile records package source metadata, package manifest hashes, and
module hashes. Normal `install` verifies non-local locked packages whose pinned
versions are reused; if registry/artifact/git/remote package contents drift
without a version update, install fails. Local sources remain mutable
development inputs and are not integrity-enforced across installs.

Use `--dry-run` to resolve and print the install plan without writing the
lockfile:

```bash
cargo run --bin nenpm -- install --root . --dry-run
```

Registry installs resolve the full version graph from registry metadata before
fetching sources. Selected registry sources are fetched concurrently, with a
default limit of eight concurrent source fetches:

```bash
cargo run --bin nenpm -- install --root . --max-concurrency 16
```

Common dependency commands:

```bash
cargo run --bin nenpm -- add @nenjo/nenji@^0.1.0 --root .
cargo run --bin nenpm -- remove @nenjo/nenji --root .
cargo run --bin nenpm -- list --root .
cargo run --bin nenpm -- info @nenjo/nenji --root .
```

The main `nenjo` CLI exposes the same package manager flow under `pm`:

```bash
cargo run --bin nenjo -- pm install --root . --dry-run
```

Install resolution uses overrides first. Dependencies without overrides are
resolved from `registries.default`. For registry packages, dependency metadata
from the registry is used to compute the full package graph before download;
downloaded package manifests must match the registry name, version, and
dependency metadata. Platform/worker resource import execution is still a
future layer.

## Registry Resolution Contract

The registry is the discovery and version authority. `registries.default` can
point at an HTTP(S) YAML index, `file:/path/to/index.yaml`, or a relative local
path. A registry record resolves a package name and version requirement to one
concrete source:

```yaml
schema: nenjo.registry.v1
packages:
  "@nenjo/core":
    - version: "0.1.0"
      source:
        kind: artifact
        url: artifacts/nenjo-core-0.1.0.tar.gz
        checksum: "<sha256>"
        manifest_path: nenjo/core.package.yaml
  "@nenjo/nenji":
    - version: "0.1.0"
      source:
        kind: git
        url: https://github.com/nenjo-ai/packages.git
        reference: v0.1.0
        manifest_path: nenjo/nenji.package.yaml
      dependencies:
        "@nenjo/core": "^0.1.0"
```

For file-backed registry indexes, relative `local.root`, `artifact.url`, and
`remote.url` values are resolved relative to the registry index file.

`artifact.source.checksum` verifies the downloaded archive bytes before
extraction. `remote.source.checksum` verifies the downloaded manifest bytes.
The optional version-level `checksum` verifies the resolved package manifest
hash.

Supported source kinds in the data model:

- `git`: remote git repo, ref, and package manifest path.
- `artifact`: immutable registry-cached tarball or zip plus checksum.
- `remote`: direct HTTPS manifest source, mainly for future escape hatches.
- `local`: local repository checkout for tests and development.

Platform package sources use the same shape under `metadata.source` for Nenjo
packages:

```json
{
  "source_type": "registry",
  "adapter": "nenjo_packages",
  "metadata": {
    "source": {
      "kind": "git",
      "url": "https://github.com/nenjo-ai/packages.git",
      "reference": "v0.1.0",
      "manifest_path": "packages.yaml"
    }
  }
}
```

The platform treats GitHub-hosted `git` sources as a raw-file optimization, not
as a GitHub-shaped package schema. Claude marketplace and Codex plugin adapters
remain GitHub-oriented because those upstream formats are repository-specific.

The default source fetcher supports all four source kinds:

- local sources are read directly from a checkout.
- git sources are cloned and checked out at the requested branch, tag, or
  commit.
- artifact sources are downloaded or read from disk as `.tar.gz` archives and
  verified by SHA-256 checksum.
- remote sources are downloaded or read from disk as direct package manifests.

Registry tests use local git repositories and file-backed artifacts so the
resolver can be exercised without external network access.

Runtime should consume installed, locked resources. It should not resolve
registry versions while an agent is executing.
