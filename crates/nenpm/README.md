# nenjo-nenpm

`nenjo-nenpm` is the package-manager layer for Nenjo packages.

The crate currently turns resolved package graphs from `nenjo-packages` into
install plans. It intentionally does not import resources into platform storage
yet; that boundary belongs to the future installer/importer layer.

## Local Install Planning

```rust
use nenjo_nenpm::InstallPlan;

# fn example() -> anyhow::Result<()> {
let plan = InstallPlan::from_local_repository("../packages", "@nenjo-ai/nenji")?;

for package in plan.packages() {
    println!("{}@{}", package.name, package.version);
    for module in package.modules {
        println!("{} {} {}", module.kind.as_str(), module.resource, module.path);
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
  "@nenjo-ai/nenji": "^0.1.0"
  "@nenjo-ai/coding": "^0.1.0"
  "@acme/test-agent": "^0.3.0"

registries:
  - https://registry.nenjo.ai/index.yaml

overrides:
  "@nenjo-ai/core": file:../packages#nenjo/core.package.yaml
  "@acme/test-agent":
    kind: local
    root: ../test-packages
    manifest_path: packages/test-agent/nenjo.package.yaml
```

Override values can use structured `PackageSource` objects or the `file:`
shorthand:

```yaml
overrides:
  "@nenjo-ai/core": file:../packages
  "@nenjo-ai/nenji": file:../packages#nenjo/nenji.package.yaml
```

Without a `#manifest_path`, `file:` defaults to `packages.yaml`, which treats
the path as a local package registry root.

Registries are an ordered list. The first registry containing a requested
package wins. GitHub-backed repository registries expose unscoped
`packages.yaml` entries under the package scope derived from the GitHub org.
For example, `https://github.com/nenjo-ai/packages.git` exposes `nenji` as
`@nenjo-ai/nenji`. Repository manifests must not author scoped package names.
Local registry sources may declare a `scope` because they do not have a remote
owner to derive from:

```yaml
registries:
  - kind: local
    scope: "@acme"
    root: ../packages
    manifest_path: packages.yaml
```

Add a GitHub-backed package registry:

```bash
nenpm add @nenjo-ai --root .
```

Add and install one package, registering the registry when needed:

```bash
nenpm add @nenjo-ai/nenji --root .
```

Add and install every package exposed by a registry:

```bash
nenpm add @nenjo-ai/* --root .
```

List packages available from configured registries:

```bash
nenpm list --root .
```

Pass a registry scope to list only one configured registry:

```bash
nenpm list @nenjo-ai --root .
```

The CLI installs the manifest by resolving package sources, writing
`nenpm.lock.yml`, and materializing the resolved package sources under
`.nenjo/packages/<scope>/<name>@<version>`:

```bash
cargo run --bin nenpm -- install --root .
```

Use `--packages-dir` to place the installed package tree and
`.nenpm-index.json` somewhere other than `<root>/.nenjo/packages`:

```bash
cargo run --bin nenpm -- install --root . --packages-dir ~/.nenjo/packages
```

`install` preserves package versions already pinned in `nenpm.lock.yml` when
they still satisfy `nenpm.yml`. Use `upgrade` to intentionally re-resolve from
the registry and rewrite those pins. By default, `upgrade` keeps already locked
packages within their current major version and only applies compatible minor
and patch upgrades:

```bash
cargo run --bin nenpm -- upgrade --root .
```

Use `--major` only when you explicitly want locked packages to move to a new
major version allowed by `nenpm.yml`:

```bash
cargo run --bin nenpm -- upgrade --root . --major
```

`nenpm update` updates the installed Nenjo command-line tools through the
bundled `nenjoup` updater.

The lockfile records package source metadata, requested dependency ranges,
exact resolved dependency versions, package manifest hashes, and module hashes.
`install` also writes `.nenjo/packages/.nenpm-index.json` so the worker can map a
locked `name@version` to its local package root without fetching at runtime.
Normal `install` verifies non-local locked packages whose pinned versions are
reused; if registry/artifact/git/remote package contents drift without a
version update, install fails. Local sources remain mutable development inputs
and are not integrity-enforced across installs.

Use `clean` to remove derived package install artifacts without touching
`nenpm.yml` or `nenpm.lock.yml`:

```bash
cargo run --bin nenpm -- clean --root .
cargo run --bin nenpm -- clean --root . --packages-dir ~/.nenjo/packages
```

Use `--dry-run` to resolve and print the install plan without writing the
lockfile or package tree:

```bash
cargo run --bin nenpm -- install --root . --dry-run
```

Use `--locked` when a caller supplies `nenpm.lock.yml` and installation must
fail if the dependency file and lockfile no longer describe the same graph:

```bash
cargo run --bin nenpm -- install --root . --locked
```

Registry installs resolve the full version graph from registry metadata before
fetching sources. Selected registry sources are fetched concurrently using the
host CPU count.

Set `NENPM_FETCH_MODE` to control how git-backed sources are fetched:

- `git` uses the existing `git clone` path and is the default.
- `provider` uses GitHub/GitLab HTTP APIs and fails for unsupported hosts.
- `raw` is accepted as an alias for `provider`.
- `auto` uses provider APIs for supported hosts and falls back to `git clone`.

Provider mode still materializes fetched files into a bounded temporary
directory so the existing package resolver and Claude plugin adapter can use the
same filesystem path. Set `GITHUB_TOKEN` or `GH_TOKEN` for GitHub provider
requests, and `GITLAB_TOKEN` or `GL_TOKEN` for GitLab provider requests.

Common dependency commands:

```bash
cargo run --bin nenpm -- init --root .
cargo run --bin nenpm -- add @nenjo-ai/nenji@^0.1.0 --root .
cargo run --bin nenpm -- remove @nenjo-ai/nenji --root .
cargo run --bin nenpm -- upgrade --root .
cargo run --bin nenpm -- clean --root .
cargo run --bin nenpm -- list --root .
cargo run --bin nenpm -- info @nenjo-ai/nenji --root .
```

Install resolution uses overrides first. Dependencies without overrides are
resolved from the ordered `registries` list. For registry packages, dependency
metadata from the registry is used to compute the full package graph before
download; downloaded package manifests must match the projected registry name,
version, and dependency metadata. Platform/worker resource import execution is
still a future layer.

## Publisher Validation

Publisher-side validation starts from a `nenjo.registry.v1` file, usually
`packages.yaml`:

```bash
cargo run --bin nenpm -- validate --root .
cargo run --bin nenpm -- prepare --root .
```

`validate` checks registry, package, module, wrapper import, and prompt
selector rules. Module imports are wrapper-level local refs only:

```yaml
schema: nenjo.context_block.v1
imports:
  context:
    - ./methodology.yml
manifest:
  name: tool_usage
  template: |
    {{ context.methodology }}
```

Package dependencies are declared only in `nenjo.package.v1`. Context prompt
selectors are derived from package/module paths. Knowledge prompt selectors
include source scope, source repo, package name, and knowledge pack name, such
as `{{ pkg.nenjo_ai.packages.knowledge.core }}`.

`prepare` runs the same validation and writes `.nenpm/registry-compiled.json`
with package versions, modules, wrapper imports, context selectors, and
package selector usages for publisher/runtime tooling.

## Registry Resolution Contract

The registry is the discovery and version authority. `registries` is an ordered
list of registry sources; the first source containing a requested package wins.
Repository-backed registries keep package keys unscoped and derive the package
scope from the registry source. A registry record resolves a package name and
version requirement to one concrete source:

```yaml
schema: nenjo.registry.v1
packages:
  core:
    - version: "0.1.0"
      source:
        kind: artifact
        url: artifacts/nenjo-core-0.1.0.tar.gz
        checksum: "<sha256>"
        manifest_path: nenjo/core.package.yaml
  nenji:
    - version: "0.1.0"
      source:
        kind: git
        url: https://github.com/nenjo-ai/packages.git
        reference: v0.1.0
        manifest_path: nenjo/nenji.package.yaml
      dependencies:
        core: "^0.1.0"
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
- `local`: local registry checkout for tests and development.

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
- git sources use `git clone` by default, or provider HTTP APIs when
  `NENPM_FETCH_MODE=provider` or `NENPM_FETCH_MODE=auto`.
- artifact sources are downloaded or read from disk as `.tar.gz` archives and
  verified by SHA-256 checksum.
- remote sources are downloaded or read from disk as direct package manifests.

Registry tests use local git repositories and file-backed artifacts so the
resolver can be exercised without external network access.

Runtime should consume installed, locked resources. It should not resolve
registry versions while an agent is executing.
