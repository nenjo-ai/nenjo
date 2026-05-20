# nenjo-packages

`nenjo-packages` defines the repository manifest, package manifest, module
manifest, dependency graph, GitHub fetch, local resolver, and lockfile
primitives used by Nenjo package install flows.

The crate is intentionally format-focused. It validates package repository
files and resolves dependency metadata, but it does not write resources to the
Nenjo platform or worker manifest store. Callers decide how a resolved module is
imported.

## Package Repository Shape

The current package model has three layers:

- A repository file with schema `nenjo.repository.v1`.
- One package manifest per versioned dependency unit with schema
  `nenjo.package.v1`.
- One or more module manifests per package with schema
  `nenjo.<resource>.v1`.

Packages are the versioned dependency and distribution unit. Modules are
package-relative manifest files. Runtime behavior is inferred from each module's
manifest schema and manifest body.

Supported resource schemas are:

- `nenjo.agent.v1`
- `nenjo.ability.v1`
- `nenjo.domain.v1`
- `nenjo.context_block.v1`
- `nenjo.knowledge.v1`
- `nenjo.knowledge_ref.v1`
- `nenjo.skill.v1`
- `nenjo.plugin.v1`
- `nenjo.mcp_server.v1`
- `nenjo.routine.v1`

## Catalog Example

```yaml
schema: nenjo.repository.v1
packages:
  "@nenjo/core": packages/core/nenjo.package.yaml
  "@nenjo/nenji": packages/nenji/nenjo.package.yaml
```

The legacy `nenjo.packages.v1` catalog shape is still parsed for compatibility
while descriptor-per-resource package files are migrated.

## Package Manifest Example

```yaml
schema: nenjo.package.v1
name: "@nenjo/nenji"
version: 1.0.0
dependencies:
  "@nenjo/core": ^1.0.0
modules:
  - agents/nenji.yaml
  - abilities/design_agent.yaml
exports:
  ".": agents/nenji.yaml
  "./design-agent": abilities/design_agent.yaml
```

Dependencies are package-level. `modules` are package-relative manifest paths.
`exports` are optional public aliases and are not required for modules to be
installed.

## Module Manifest Example

```yaml
schema: nenjo.agent.v1
selector: git://nenjo-ai/packages/packages/reviewer
root_uri: git://nenjo-ai/packages/packages/reviewer/
manifest:
  name: reviewer
  display_name: Reviewer
  instructions: Review the provided work and return actionable findings.
```

The `manifest` body is resource-specific JSON/YAML. This crate requires it to
be an object with a non-empty `name`. `selector` and `root_uri` are optional
source-management identifiers that downstream importers can use when replacing
previously installed resources.

## Multi-Resource Module Files

A module path can point to one resource manifest or to a bundle envelope with
schema `nenjo.modules.v1`:

```yaml
schema: nenjo.modules.v1
resources:
  - schema: nenjo.ability.v1
    manifest:
      name: design_agent
      tool_name: design_agent

  - schema: nenjo.ability.v1
    manifest:
      name: diagnose_failure
      tool_name: diagnose_failure
```

Bundled resources are addressed with `#resource_name` selectors:

```yaml
exports:
  "./design-agent": abilities/design.yaml#design_agent
```

Single-resource module files are addressable by both their file path and their
canonical `path#name` key. Multi-resource bundle files require the
`path#resource_name` form.

## Resource Imports

Resource manifests can declare structured runtime imports inside their
`manifest` body:

```yaml
schema: nenjo.agent.v1
manifest:
  name: support_agent
  imports:
    abilities:
      - ./abilities/design.yaml#design_agent
    domains:
      - ./domains/support.yaml#support
    context:
      - "@nenjo/core/methodology"
```

The package resolver records these imports on resolved modules. Package
resolution still happens at the package level; imports describe runtime
composition between resolved resources.

## Local Resolution

`LocalPackageResolver` resolves `nenjo.repository.v1` package graphs from a
local filesystem checkout. It is intended for tests and local package authoring.

```rust
use nenjo_packages::LocalPackageResolver;

# fn example() -> anyhow::Result<()> {
let graph = LocalPackageResolver::new("../packages")
    .resolve_package_graph("@nenjo/nenji")?;

for package in graph.topo_order()? {
    let package = &graph.packages[&package];
    for module in package.modules.values() {
        println!("install {} {}", module.kind.as_str(), module.name());
    }
}
# Ok(())
# }
```

## Fetching And Resolving

`GitHubFetcher::resolve_resource_graph` currently supports the legacy
descriptor-per-resource model. The new package/module model is available through
the shared manifest types and local resolver while the registry-backed resolver
is introduced.

```rust
use nenjo_packages::{GitHubFetcher, GitHubSource};

# async fn example() -> anyhow::Result<()> {
let fetcher = GitHubFetcher::new(GitHubSource {
    owner: "nenjo-ai".to_string(),
    repo: "packages".to_string(),
    reference: "main".to_string(),
    manifest_path: "packages.yaml".to_string(),
});

let catalog = fetcher.fetch_catalog().await?;
let graph = fetcher
    .resolve_resource_graph("packages/reviewer/package.yaml")
    .await?;

for path in graph.topo_order()? {
    let resource = &graph.resources[&path];
    println!("install {} {}", resource.kind.as_str(), resource.name());
}
# Ok(())
# }
```

`resolve_resource_graph` fetches the root descriptor, follows `depends_on`,
validates each descriptor and resource manifest, checks kind consistency between
descriptor and resource schema, hashes descriptor and entry content, and checks
dependency versions.

## Adapters

`PackageAdapter` names the external format that produced a package:

- `nenjo_packages` for this crate's native catalog and descriptor format.
- `claude_marketplace` for Claude marketplace imports.
- `codex_plugin` for Codex plugin imports.

Adapters are stable serialized identifiers. Use `PackageAdapter::parse` or
`str::parse::<PackageAdapter>()` to validate user or database values.

## Validation Helpers

The crate also exposes small helpers used by importers:

- `validate_source_path` normalizes safe repository-relative paths.
- `package_entry_path` resolves a descriptor-relative entry filename.
- `version_satisfies` checks exact and caret-major version requirements.
- `parse_json_or_yaml` and `parse_json_or_yaml_as` support either JSON or YAML.
- `sha256_hex` returns `sha256:<hex>` content hashes for lockfiles.

## Lockfiles

`PackageLock` and `PackageLockResource` are plain serializable records for
capturing what was installed from a Git source: requested ref, resolved commit,
resource paths, resource IDs, hashes, versions, and optional selectors.
