# nenjo-packages

`nenjo-packages` defines the package catalog, descriptor, resource manifest,
dependency graph, GitHub fetch, and lockfile primitives used by Nenjo package
install flows.

The crate is intentionally format-focused. It validates package repository
files and resolves dependency metadata, but it does not write resources to the
Nenjo platform or worker manifest store. Callers decide how a resolved resource
is imported.

## Package Repository Shape

A native Nenjo package repository has three layers:

- A catalog file with schema `nenjo.packages.v1`.
- One package descriptor per installable resource with schema `nenjo.package.v1`.
- One resource manifest per descriptor with schema `nenjo.<resource>.v1`.

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
schema: nenjo.packages.v1
name: Nenjo examples
description: Example packages for Nenjo workers.
packages:
  - type: agent
    slug: reviewer
    name: Reviewer
    path: packages/reviewer/package.yaml
```

## Package Descriptor Example

```yaml
schema: nenjo.package.v1
type: agent
slug: reviewer
name: Reviewer
version: 1.0.0
entry: agent.yaml
depends_on:
  - path: packages/review-ability/package.yaml
    version: ^1.0.0
```

`entry` must be a filename next to the descriptor. Dependency paths are
repository-relative descriptor paths.

## Resource Manifest Example

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

## Fetching And Resolving

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
