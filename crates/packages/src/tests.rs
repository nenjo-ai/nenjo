use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

fn temp_repo(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("nenjo-packages-{name}-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_file(root: &Path, path: &str, content: &str) {
    let full_path = root.join(path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full_path, content).unwrap();
}

fn resolved_resource(
    path: &str,
    version: Option<&str>,
    depends_on: Vec<ResourceDependency>,
) -> ResolvedResource {
    ResolvedResource {
        path: path.to_string(),
        entry_path: path.replace("package.yaml", "agent.yaml"),
        hash: sha256_hex(path.as_bytes()),
        kind: PackageKind::Agent,
        descriptor: PackageDescriptor {
            schema: "nenjo.package.v1".to_string(),
            kind: PackageKind::Agent,
            slug: path.replace('/', "-"),
            name: path.to_string(),
            version: version.map(str::to_string),
            entry: "agent.yaml".to_string(),
            depends_on,
            metadata: serde_json::Value::Null,
        },
        manifest: ResourceManifest {
            schema: "nenjo.agent.v1".to_string(),
            slug: None,
            root_uri: None,
            selector: None,
            imports: BTreeMap::new(),
            manifest: serde_json::json!({
                "name": path,
            }),
        },
    }
}

#[test]
fn parses_resource_schema_version() {
    let schema = ResourceSchema::parse("nenjo.agent.v1").unwrap();
    assert_eq!(schema.kind, PackageKind::Agent);
    assert_eq!(schema.version, ManifestSchemaVersion::V1);
}

#[test]
fn parses_all_supported_resource_types() {
    let cases = [
        ("nenjo.agent.v1", PackageKind::Agent),
        ("nenjo.ability.v1", PackageKind::Ability),
        ("nenjo.domain.v1", PackageKind::Domain),
        ("nenjo.context_block.v1", PackageKind::ContextBlock),
        ("nenjo.knowledge.v1", PackageKind::Knowledge),
        ("nenjo.knowledge_ref.v1", PackageKind::Knowledge),
        ("nenjo.skill.v1", PackageKind::Skill),
        ("nenjo.plugin.v1", PackageKind::Plugin),
        ("nenjo.mcp_server.v1", PackageKind::McpServer),
        ("nenjo.routine.v1", PackageKind::Routine),
    ];

    for (schema, expected) in cases {
        assert_eq!(PackageKind::parse_schema(schema).unwrap(), expected);
        assert_eq!(
            ResourceSchema::parse(schema).unwrap().version.as_str(),
            "v1"
        );
    }
}

#[test]
fn parses_and_serializes_package_adapters() {
    let cases = [
        ("nenjo_packages", PackageAdapter::NenjoPackages),
        ("claude_marketplace", PackageAdapter::ClaudeMarketplace),
        ("codex_plugin", PackageAdapter::CodexPlugin),
    ];

    for (adapter_name, expected) in cases {
        let parsed: PackageAdapter = adapter_name.parse().unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(parsed.as_str(), adapter_name);
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::Value::String(adapter_name.to_string())
        );
    }
}

#[test]
fn rejects_unknown_package_adapter() {
    let err = PackageAdapter::parse("unknown").unwrap_err().to_string();
    assert!(err.contains("unsupported package adapter"));
}

#[test]
fn rejects_unversioned_resource_schema() {
    let err = ResourceSchema::parse("agent").unwrap_err().to_string();
    assert!(err.contains("must start with 'nenjo.'"));
}

#[test]
fn rejects_unknown_resource_schema_version() {
    let err = ResourceSchema::parse("nenjo.agent.v2")
        .unwrap_err()
        .to_string();
    assert!(err.contains("unsupported version"));
}

#[test]
fn validates_package_catalog_schema() {
    let catalog: PackageCatalog = parse_json_or_yaml_as(
        r#"
schema: nenjo.packages.v1
packages:
- type: agent
  slug: nenji
  path: nenjo/agents/nenji/package.yaml
"#,
    )
    .unwrap();
    assert_eq!(catalog.schema_version().unwrap(), ManifestSchemaVersion::V1);
    catalog.validate().unwrap();
}

#[test]
fn rejects_wrong_package_file_schema_kind() {
    let err = PackageFileSchema::parse_descriptor("nenjo.packages.v1")
        .unwrap_err()
        .to_string();
    assert!(err.contains("expected schema 'nenjo.package.*'"));
}

#[test]
fn validates_package_descriptor_schema() {
    let descriptor: PackageDescriptor = parse_json_or_yaml_as(
        r#"
schema: nenjo.package.v1
type: ability
slug: build-agent
name: Build Agent
entry: ability.yaml
"#,
    )
    .unwrap();
    assert_eq!(
        descriptor.schema_version().unwrap(),
        ManifestSchemaVersion::V1
    );
    descriptor
        .validate("nenjo/abilities/build_agent/package.yaml")
        .unwrap();
}

#[test]
fn validates_repository_manifest_schema() {
    let registry: PackageRegistryManifest = parse_json_or_yaml_as(
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core/nenjo.package.yaml
  "nenji": packages/nenji/nenjo.package.yaml
"#,
    )
    .unwrap();
    assert_eq!(
        registry.schema_version().unwrap(),
        ManifestSchemaVersion::V1
    );
    registry.validate().unwrap();
}

#[test]
fn parses_module_package_manifest_with_string_modules() {
    let package: ModulePackageManifest = parse_json_or_yaml_as(
        r#"
schema: nenjo.package.v1
name: "nenji"
version: "0.1.0"
dependencies:
  "core": "^0.1.0"
modules:
  - agents/nenji.yaml
  - path: abilities/design_agent.yaml
metadata:
  optional: false
"#,
    )
    .unwrap();
    package
        .validate("packages/nenji/nenjo.package.yaml")
        .unwrap();
    assert_eq!(package.modules[0].path, "agents/nenji.yaml");
    assert_eq!(package.modules[1].path, "abilities/design_agent.yaml");
}

#[test]
fn reads_resource_manifest_body_name() {
    let manifest: ResourceManifest = parse_json_or_yaml_as(
        r#"
schema: nenjo.agent.v1
manifest:
  name: system
  display_name: Nenji
"#,
    )
    .unwrap();
    assert_eq!(manifest.name().unwrap(), "system");
}

#[test]
fn reads_resource_manifest_body_version() {
    let manifest: ResourceManifest = parse_json_or_yaml_as(
        r#"
schema: nenjo.skill.v1
manifest:
  name: rust-review
  version: 1.2.3
"#,
    )
    .unwrap();
    assert_eq!(manifest.version(), Some("1.2.3"));
}

#[test]
fn rejects_non_object_resource_manifest_body() {
    let manifest: ResourceManifest = parse_json_or_yaml_as(
        r#"
schema: nenjo.agent.v1
manifest: []
"#,
    )
    .unwrap();
    let err = manifest.manifest_object().unwrap_err().to_string();
    assert!(err.contains("must be an object"));
}

#[test]
fn rejects_resource_manifest_without_name() {
    let manifest: ResourceManifest = parse_json_or_yaml_as(
        r#"
schema: nenjo.agent.v1
manifest:
  display_name: Nenji
"#,
    )
    .unwrap();
    let err = manifest.name().unwrap_err().to_string();
    assert!(err.contains("missing name"));
}

#[test]
fn rejects_authored_path_for_package_resource_manifests() {
    for schema in [
        "nenjo.ability.v1",
        "nenjo.domain.v1",
        "nenjo.context_block.v1",
    ] {
        let content = format!(
            r#"
schema: {schema}
manifest:
  name: design_agent
  path: authored/path
"#
        );

        let err = parse_module_file(&content, "abilities/design_agent.yaml").unwrap_err();
        let err = format!("{err:?}");
        assert!(err.contains("must not define manifest.path"), "{err}");
    }
}

#[test]
fn rejects_wrapper_slug_for_package_resource_manifests() {
    let err = parse_module_file(
        r#"
schema: nenjo.agent.v1
slug: authored-slug
manifest:
  name: Nenji
"#,
        "agents/nenji.yaml",
    )
    .unwrap_err();
    let err = format!("{err:?}");
    assert!(err.contains("must not define wrapper slug"), "{err}");
}

#[test]
fn allows_authored_path_for_non_path_derived_resource_manifests() {
    let content = r#"
schema: nenjo.agent.v1
manifest:
  name: design_agent
  path: authored/path
"#;

    parse_module_file(content, "agents/design_agent.yaml").unwrap();
}

#[test]
fn rejects_manifest_body_imports_inside_module_bundles() {
    let content = r#"
schema: nenjo.modules.v1
resources:
  - schema: nenjo.context_block.v1
    manifest:
      name: methodology
      imports:
        context:
          - ./other.yml
      template: Use the method.
"#;

    let err = parse_module_file(content, "context/index.yml").unwrap_err();
    let err = format!("{err:?}");
    assert!(err.contains("must not contain imports"), "{err}");
}

#[test]
fn local_module_imports_must_not_escape_package_root() {
    let root = temp_repo("import-escape");
    write_file(
        &root,
        "packages/core/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "core"
version: "0.1.0"
modules:
  - abilities/design_agent.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/abilities/design_agent.yaml",
        r#"
schema: nenjo.ability.v1
imports:
  context:
    - ../../outside.yml
manifest:
  name: design_agent
"#,
    );

    let err = LocalPackageResolver::new(&root)
        .resolve_package_manifest("packages/core/nenjo.package.yaml")
        .unwrap_err();
    let err = format!("{err:?}");
    assert!(err.contains("escapes the package root"), "{err}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn allows_selector_metadata_outside_manifest_body() {
    let manifest: ResourceManifest = parse_json_or_yaml_as(
        r#"
schema: nenjo.agent.v1
selector: pkg:nenjo.agent
root_uri: pkg://nenjo.agent/
manifest:
  name: system
  display_name: Nenji
"#,
    )
    .unwrap();
    assert_eq!(manifest.selector(), Some("pkg:nenjo.agent"));
    assert_eq!(manifest.root_uri(), Some("pkg://nenjo.agent/"));
    assert_eq!(manifest.name().unwrap(), "system");
}

#[test]
fn caret_version_matches_major() {
    assert!(version_satisfies("0.1.2", "^0.1.0"));
    assert!(version_satisfies("v1.2.3", "^1.0.0"));
    assert!(!version_satisfies("2.0.0", "^1.0.0"));
}

#[test]
fn exact_version_ignores_leading_v_prefix() {
    assert!(version_satisfies("v1.2.3", "1.2.3"));
    assert!(version_satisfies("1.2.3", "v1.2.3"));
    assert!(!version_satisfies("1.2.4", "1.2.3"));
}

#[test]
fn source_path_rejects_escape() {
    assert!(validate_source_path("nenjo/agents/nenji.yaml").is_ok());
    let err = validate_source_path("../nenjo/agents/nenji.yaml").unwrap_err();
    assert!(matches!(err, PackageError::InvalidPath { .. }));
    assert!(validate_source_path("/nenjo/agents/nenji.yaml").is_err());
}

#[test]
fn package_name_validation_returns_structured_error() {
    let err = validate_package_name("@broken").unwrap_err();
    assert!(matches!(err, PackageError::InvalidPackageName { .. }));
}

#[test]
fn source_path_normalizes_relative_prefix_and_trailing_slash() {
    assert_eq!(
        validate_source_path("./nenjo/agents/nenji/").unwrap(),
        "nenjo/agents/nenji"
    );
}

#[test]
fn package_entry_path_must_stay_in_descriptor_directory() {
    assert_eq!(
        package_entry_path("nenjo/agents/nenji/package.yaml", "agent.yaml").unwrap(),
        "nenjo/agents/nenji/agent.yaml"
    );
    let err = package_entry_path("nenjo/agents/nenji/package.yaml", "nested/agent.yaml")
        .unwrap_err()
        .to_string();
    assert!(err.contains("relative to the package directory"));
}

#[test]
fn package_module_source_path_resolves_package_relative_paths() {
    assert_eq!(
        package_module_source_path("packages/nenji/nenjo.package.yaml", "agents/nenji.yaml")
            .unwrap(),
        "packages/nenji/agents/nenji.yaml"
    );
    assert!(
        package_module_source_path("packages/nenji/nenjo.package.yaml", "../agent.yaml").is_err()
    );
}

#[test]
fn graph_topo_order_places_dependencies_before_root() {
    let root_path = "packages/root/package.yaml".to_string();
    let dependency_path = "packages/dependency/package.yaml".to_string();
    let mut resources = BTreeMap::new();
    resources.insert(
        root_path.clone(),
        resolved_resource(
            &root_path,
            Some("1.0.0"),
            vec![ResourceDependency {
                path: dependency_path.clone(),
                version: Some("^2.0.0".to_string()),
            }],
        ),
    );
    resources.insert(
        dependency_path.clone(),
        resolved_resource(&dependency_path, Some("2.1.0"), Vec::new()),
    );

    let graph = ResolvedResourceGraph {
        root_path: root_path.clone(),
        resources,
    };
    assert_eq!(
        graph.topo_order().unwrap(),
        vec![dependency_path, root_path]
    );
    graph.validate_versions().unwrap();
}

#[test]
fn graph_rejects_dependency_cycle() {
    let first_path = "packages/first/package.yaml".to_string();
    let second_path = "packages/second/package.yaml".to_string();
    let mut resources = BTreeMap::new();
    resources.insert(
        first_path.clone(),
        resolved_resource(
            &first_path,
            None,
            vec![ResourceDependency {
                path: second_path.clone(),
                version: None,
            }],
        ),
    );
    resources.insert(
        second_path,
        resolved_resource(
            "packages/second/package.yaml",
            None,
            vec![ResourceDependency {
                path: first_path.clone(),
                version: None,
            }],
        ),
    );

    let graph = ResolvedResourceGraph {
        root_path: first_path,
        resources,
    };
    let err = graph.topo_order().unwrap_err().to_string();
    assert!(err.contains("dependency cycle"));
}

#[test]
fn graph_rejects_unsatisfied_dependency_version() {
    let root_path = "packages/root/package.yaml".to_string();
    let dependency_path = "packages/dependency/package.yaml".to_string();
    let mut resources = BTreeMap::new();
    resources.insert(
        root_path.clone(),
        resolved_resource(
            &root_path,
            Some("1.0.0"),
            vec![ResourceDependency {
                path: dependency_path.clone(),
                version: Some("^2.0.0".to_string()),
            }],
        ),
    );
    resources.insert(
        dependency_path,
        resolved_resource(
            "packages/dependency/package.yaml",
            Some("1.9.0"),
            Vec::new(),
        ),
    );

    let graph = ResolvedResourceGraph {
        root_path,
        resources,
    };
    let err = graph.validate_versions().unwrap_err().to_string();
    assert!(err.contains("requires packages/dependency/package.yaml version ^2.0.0"));
}

#[test]
fn local_resolver_resolves_package_modules_and_dependencies() {
    let root = temp_repo("local-resolver");
    write_file(
        &root,
        "packages.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core/nenjo.package.yaml
  "nenji": packages/nenji/nenjo.package.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "core"
version: "0.1.0"
modules:
  - context_blocks/methodology.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/methodology.yaml",
        r#"
schema: nenjo.context_block.v1
manifest:
  name: methodology
  template: think clearly
"#,
    );
    write_file(
        &root,
        "packages/nenji/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "nenji"
version: "0.1.0"
dependencies:
  "core": "^0.1.0"
modules:
  - agents/nenji.yaml
  - abilities/design_agent.yaml
"#,
    );
    write_file(
        &root,
        "packages/nenji/agents/nenji.yaml",
        r#"
schema: nenjo.agent.v1
manifest:
  name: nenji
"#,
    );
    write_file(
        &root,
        "packages/nenji/abilities/design_agent.yaml",
        r#"
schema: nenjo.ability.v1
manifest:
  name: design_agent
"#,
    );

    let graph = LocalPackageResolver::new(&root)
        .resolve_package_graph("nenji")
        .unwrap();
    assert_eq!(
        graph.topo_order().unwrap(),
        vec!["core".to_string(), "nenji".to_string()]
    );
    let nenji = &graph.packages["nenji"];
    assert_eq!(nenji.modules.len(), 4);
    assert_eq!(
        nenji.modules["agents/nenji.yaml"].source_path,
        "packages/nenji/agents/nenji.yaml"
    );
    assert_eq!(nenji.modules["agents/nenji.yaml"].kind, PackageKind::Agent);
    assert_eq!(nenji.modules["agents/nenji.yaml"].name(), "nenji");
    assert_eq!(
        graph.packages["core"].modules["context_blocks/methodology.yaml"].kind,
        PackageKind::ContextBlock
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_resolver_expands_directory_module_indexes() {
    let root = temp_repo("module-index");
    write_file(
        &root,
        "packages.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core/nenjo.package.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "core"
version: "0.1.0"
modules:
  - context_blocks/
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/index.yml",
        r#"
schema: nenjo.module_index.v1
modules:
  - core/
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/core/index.yml",
        r#"
schema: nenjo.module_index.v1
modules:
  - methodology.yaml
  - tool_usage.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/core/methodology.yaml",
        r#"
schema: nenjo.context_block.v1
manifest:
  name: methodology
  template: Think clearly.
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/core/tool_usage.yaml",
        r#"
schema: nenjo.context_block.v1
manifest:
  name: tool_usage
  template: Use tools carefully.
"#,
    );

    let package = LocalPackageResolver::new(&root)
        .resolve_package_manifest("packages/core/nenjo.package.yaml")
        .unwrap();

    assert_eq!(package.modules.len(), 4);
    assert_eq!(
        package.modules["context_blocks/core/methodology.yaml"].source_path,
        "packages/core/context_blocks/core/methodology.yaml"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_resolver_rejects_directory_module_without_index() {
    let root = temp_repo("missing-module-index");
    write_file(
        &root,
        "packages/core/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "core"
version: "0.1.0"
modules:
  - context_blocks/
"#,
    );

    let err = LocalPackageResolver::new(&root)
        .resolve_package_manifest("packages/core/nenjo.package.yaml")
        .unwrap_err();
    let err = format!("{err:?}");

    assert!(err.contains("requires index.yml or index.yaml"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_resolver_rejects_module_index_cycles() {
    let root = temp_repo("module-index-cycle");
    write_file(
        &root,
        "packages/core/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "core"
version: "0.1.0"
modules:
  - context_blocks/
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/index.yml",
        r#"
schema: nenjo.module_index.v1
modules:
  - index.yml
"#,
    );

    let err = LocalPackageResolver::new(&root)
        .resolve_package_manifest("packages/core/nenjo.package.yaml")
        .unwrap_err();
    let err = format!("{err:?}");

    assert!(err.contains("module index cycle"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_resolver_rejects_unsatisfied_package_dependency() {
    let root = temp_repo("bad-version");
    write_file(
        &root,
        "packages.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core/nenjo.package.yaml
  "nenji": packages/nenji/nenjo.package.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "core"
version: "1.0.0"
modules:
  - context_blocks/core.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/core.yaml",
        r#"
schema: nenjo.context_block.v1
manifest:
  name: core
"#,
    );
    write_file(
        &root,
        "packages/nenji/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "nenji"
version: "0.1.0"
dependencies:
  "core": "^2.0.0"
modules:
  - agents/nenji.yaml
"#,
    );
    write_file(
        &root,
        "packages/nenji/agents/nenji.yaml",
        r#"
schema: nenjo.agent.v1
manifest:
  name: nenji
"#,
    );

    let err = LocalPackageResolver::new(&root)
        .resolve_package_graph("nenji")
        .unwrap_err()
        .to_string();
    assert!(err.contains("requires core version ^2.0.0"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_resource_logical_id_ignores_version_and_scope() {
    let key =
        PackageResourceLogicalKey::new("nenji", PackageKind::Agent, "agents/nenji.yaml", "nenji")
            .unwrap();
    let same_key =
        PackageResourceLogicalKey::new("nenji", PackageKind::Agent, "agents/nenji.yaml", "nenji")
            .unwrap();
    let instance_v1 = PackageResourceInstanceKey::new(
        "nenji",
        "0.1.0",
        PackageKind::Agent,
        "agents/nenji.yaml",
        "nenji",
    )
    .unwrap();
    let instance_v2 = PackageResourceInstanceKey::new(
        "nenji",
        "0.2.0",
        PackageKind::Agent,
        "agents/nenji.yaml",
        "nenji",
    )
    .unwrap();

    assert_eq!(key.as_str(), "pkg:nenji:agent:agents/nenji.yaml#nenji");
    assert_eq!(key.resource_id(), same_key.resource_id());
    assert_ne!(instance_v1.as_str(), instance_v2.as_str());
}
