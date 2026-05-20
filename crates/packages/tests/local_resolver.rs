use std::path::PathBuf;

use nenjo_packages::{LocalPackageResolver, PackageKind, validate_source_path};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("local_resolver")
        .join(name)
}

#[test]
fn resolves_realistic_local_package_graph_dependency_first() {
    let root = fixture("realistic");
    let graph = LocalPackageResolver::new(&root)
        .resolve_package_graph("@acme/agent")
        .unwrap();

    assert_eq!(
        graph.topo_order().unwrap(),
        vec!["@acme/core".to_string(), "@acme/agent".to_string()]
    );

    let core = &graph.packages["@acme/core"];
    assert_eq!(core.version, "0.1.0");
    assert_eq!(core.modules.len(), 6);
    assert_eq!(
        core.modules["knowledge/core.yaml"].source_path,
        "packages/core/knowledge/core.yaml"
    );
    assert_eq!(
        core.modules["knowledge/core.yaml"].kind,
        PackageKind::Knowledge
    );
    assert_eq!(core.modules["knowledge/core.yaml"].name(), "core_knowledge");
    assert_eq!(
        core.modules["context_blocks/methodology.yaml"].kind,
        PackageKind::ContextBlock
    );
    assert_eq!(
        core.manifest.exports["./methodology"].path,
        "context_blocks/methodology.yaml"
    );

    let agent = &graph.packages["@acme/agent"];
    assert_eq!(agent.modules.len(), 6);
    assert_eq!(
        agent.modules["agents/support.yaml"].kind,
        PackageKind::Agent
    );
    assert_eq!(agent.modules["agents/support.yaml"].name(), "support_agent");
    assert_eq!(
        agent.modules["abilities/triage.yaml"].kind,
        PackageKind::Ability
    );
    assert_eq!(
        agent.modules["domains/support.yaml"].kind,
        PackageKind::Domain
    );
    assert_eq!(agent.manifest.exports["."].path, "agents/support.yaml");
}

#[test]
fn resolves_independent_root_with_shared_dependency() {
    let graph = LocalPackageResolver::new(fixture("realistic"))
        .resolve_package_graph("@acme/coding")
        .unwrap();

    assert_eq!(
        graph.topo_order().unwrap(),
        vec!["@acme/core".to_string(), "@acme/coding".to_string()]
    );
    assert_eq!(
        graph.packages["@acme/coding"].modules["context_blocks/git.yaml"].name(),
        "git_discipline"
    );
}

#[test]
fn resolves_bundled_module_resources_and_imports() {
    let graph = LocalPackageResolver::new(fixture("bundled-modules"))
        .resolve_package_graph("@acme/agent")
        .unwrap();

    assert_eq!(
        graph.topo_order().unwrap(),
        vec!["@acme/core".to_string(), "@acme/agent".to_string()]
    );

    let core = &graph.packages["@acme/core"];
    assert!(core.modules.contains_key("context/core.yaml#methodology"));
    assert!(core.modules.contains_key("context/core.yaml#tool_usage"));
    assert!(!core.modules.contains_key("context/core.yaml"));
    assert_eq!(
        core.manifest.exports["./methodology"].path,
        "context/core.yaml#methodology"
    );
    let tool_usage = &core.modules["context/core.yaml#tool_usage"];
    assert_eq!(tool_usage.kind, PackageKind::ContextBlock);
    assert_eq!(tool_usage.imports.len(), 1);
    assert_eq!(tool_usage.imports[0].surface, "context");
    assert_eq!(tool_usage.imports[0].reference, "#methodology");

    let agent = &graph.packages["@acme/agent"];
    assert!(agent.modules.contains_key("agents/support.yaml"));
    assert!(
        agent
            .modules
            .contains_key("agents/support.yaml#support_agent")
    );
    assert!(
        agent
            .modules
            .contains_key("abilities/design.yaml#design_agent")
    );
    assert!(
        agent
            .modules
            .contains_key("abilities/design.yaml#diagnose_failure")
    );
    assert!(!agent.modules.contains_key("abilities/design.yaml"));

    let support = &agent.modules["agents/support.yaml"];
    assert_eq!(support.key(), "agents/support.yaml#support_agent");
    let refs: Vec<_> = support
        .imports
        .iter()
        .map(|import| (import.surface.as_str(), import.reference.as_str()))
        .collect();
    assert!(refs.contains(&("abilities", "./abilities/design.yaml#design_agent")));
    assert!(refs.contains(&("domains", "./domains/support.yaml#support")));
    assert!(refs.contains(&("context", "@acme/core/methodology")));
    assert!(refs.contains(&("context", "@acme/core/tool-usage")));

    let design_agent = &agent.modules["abilities/design.yaml#design_agent"];
    assert_eq!(design_agent.kind, PackageKind::Ability);
    assert_eq!(design_agent.name(), "design_agent");
    assert_eq!(design_agent.imports[0].reference, "@acme/core/methodology");

    let support_domain = &agent.modules["domains/support.yaml"];
    assert_eq!(support_domain.kind, PackageKind::Domain);
    assert_eq!(support_domain.imports.len(), 2);
}

#[test]
fn rejects_export_to_missing_bundled_resource_selector() {
    let err = LocalPackageResolver::new(fixture("bad-bundle-export"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err()
        .to_string();

    assert!(err.contains("export ./missing points at 'abilities/design.yaml#missing'"));
}

#[test]
fn rejects_duplicate_bundled_resource_names() {
    let err = LocalPackageResolver::new(fixture("duplicate-bundled-resource"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err()
        .to_string();

    assert!(err.contains("declares duplicate bundled resource 'design_agent'"));
}

#[test]
fn exact_version_dependency_accepts_leading_v_on_actual_version() {
    LocalPackageResolver::new(fixture("exact-v-prefix"))
        .resolve_package_graph("@acme/agent")
        .unwrap();
}

#[test]
fn rejects_unsatisfied_dependency_when_version_changes() {
    let err = LocalPackageResolver::new(fixture("unsatisfied-version"))
        .resolve_package_graph("@acme/agent")
        .unwrap_err()
        .to_string();

    assert!(err.contains("@acme/agent requires @acme/core version ^2.0.0, got 1.0.0"));
}

#[test]
fn caret_requirement_matches_same_major_version() {
    LocalPackageResolver::new(fixture("caret-major"))
        .resolve_package_graph("@acme/agent")
        .unwrap();
}

#[test]
fn rejects_missing_dependency_package_in_repository_manifest() {
    let err = LocalPackageResolver::new(fixture("missing-dependency"))
        .resolve_package_graph("@acme/agent")
        .unwrap_err()
        .to_string();

    assert!(err.contains("package @acme/missing is not listed in repository"));
}

#[test]
fn rejects_dependency_cycles_between_packages() {
    let graph = LocalPackageResolver::new(fixture("dependency-cycle"))
        .resolve_package_graph("@acme/a")
        .unwrap();
    let err = graph.topo_order().unwrap_err().to_string();

    assert!(err.contains("dependency cycle includes @acme/a"));
}

#[test]
fn rejects_repository_mapping_to_mismatched_package_name() {
    let err = LocalPackageResolver::new(fixture("mismatched-package-name"))
        .resolve_package_graph("@acme/expected")
        .unwrap_err()
        .to_string();

    assert!(err.contains("repository maps @acme/expected"));
    assert!(err.contains("declares @acme/actual"));
}

#[test]
fn rejects_missing_module_file() {
    let err = LocalPackageResolver::new(fixture("missing-module"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err();
    let err = format!("{err:?}");

    assert!(err.contains("failed to read local package file packages/broken/missing.yaml"));
}

#[test]
fn rejects_module_manifest_without_name() {
    let err = LocalPackageResolver::new(fixture("module-without-name"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("failed to validate module manifest packages/broken/agent.yaml"),
        "{err}"
    );
}

#[test]
fn rejects_unknown_module_schema() {
    let err = LocalPackageResolver::new(fixture("unknown-schema"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err()
        .to_string();

    assert!(err.contains("unsupported package resource schema 'source'"));
}

#[test]
fn rejects_export_to_module_not_listed_in_modules() {
    let err = LocalPackageResolver::new(fixture("bad-export"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err()
        .to_string();

    assert!(err.contains("export ./other points at 'other.yaml' which is not listed in modules"));
}

#[test]
fn rejects_duplicate_module_paths_in_package_manifest() {
    let err = LocalPackageResolver::new(fixture("duplicate-module"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err()
        .to_string();

    assert!(err.contains("declares duplicate module path 'agent.yaml'"));
}

#[test]
fn rejects_module_paths_that_escape_package_directory() {
    let err = LocalPackageResolver::new(fixture("path-escape"))
        .resolve_package_graph("@acme/broken")
        .unwrap_err()
        .to_string();

    assert!(err.contains("invalid module path '../shared.yaml'"));
}

#[test]
fn rejects_absolute_repository_package_paths() {
    let err = LocalPackageResolver::new(fixture("absolute-repo-path"))
        .load_repository()
        .unwrap_err();
    let err = format!("{err:?}");

    assert!(err.contains("repository package '@acme/broken' has invalid path"));
}

#[test]
fn custom_repository_manifest_path_is_supported() {
    let graph = LocalPackageResolver::with_repository_path(
        fixture("custom-repository-path"),
        "nenjo.repository.yaml",
    )
    .resolve_package_graph("@acme/core")
    .unwrap();

    assert_eq!(graph.packages["@acme/core"].modules.len(), 6);
}

#[test]
fn validates_source_path_rejects_nested_parent_segments() {
    assert!(validate_source_path("packages/core/manifest.yaml").is_ok());
    assert!(validate_source_path("packages/core/../manifest.yaml").is_err());
    assert!(validate_source_path("/packages/core/manifest.yaml").is_err());
}
