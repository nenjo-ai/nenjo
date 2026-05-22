use std::path::PathBuf;

use nenjo_nenpm::{
    DependencyManifest, DependencyOverride, NenpmError, PackageSource, RegistryReference,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("dependency_manifest")
        .join(name)
}

#[test]
fn loads_preferred_yml_dependency_manifest() {
    let loaded = DependencyManifest::load_from_dir(fixture("yml")).unwrap();

    assert_eq!(loaded.path.file_name().unwrap(), "nenpm.yml");
    assert_eq!(loaded.manifest.schema, "nenjo.dependencies.v1");
    assert_eq!(loaded.manifest.dependencies["@nenjo/nenji"], "^0.1.0");
    assert_eq!(loaded.manifest.dependencies["@acme/test-agent"], "^0.3.0");
    assert_eq!(
        loaded.manifest.registries[0],
        RegistryReference::Index("https://registry.nenjo.ai".to_string())
    );

    let source = loaded.manifest.overrides["@nenjo/core"]
        .to_package_source()
        .unwrap();
    assert_eq!(
        source,
        PackageSource::Local {
            root: PathBuf::from("../packages"),
            manifest_path: "nenjo/core.package.yaml".to_string(),
            scope: None,
        }
    );

    let source = loaded.manifest.overrides["@acme/test-agent"]
        .to_package_source()
        .unwrap();
    assert_eq!(
        source,
        PackageSource::Local {
            root: PathBuf::from("../test-packages"),
            manifest_path: "packages/test-agent/nenjo.package.yaml".to_string(),
            scope: None,
        }
    );
}

#[test]
fn rejects_dev_dependencies() {
    let err = DependencyManifest::parse_yaml(
        r#"
schema: nenjo.dependencies.v1
dependencies: {}
dev_dependencies:
  "@acme/test-agent": "^0.3.0"
"#,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("failed to parse nenpm dependency manifest"));
}

#[test]
fn parses_scoped_repository_registry_source() {
    let manifest = DependencyManifest::parse_yaml(
        r#"
schema: nenjo.dependencies.v1
registries:
  - kind: git
    url: https://github.com/nenjo-ai/packages.git
    reference: main
    manifest_path: packages.yaml
"#,
    )
    .unwrap();

    assert_eq!(
        manifest.registries[0],
        RegistryReference::Source(PackageSource::Git {
            url: "https://github.com/nenjo-ai/packages.git".to_string(),
            reference: "main".to_string(),
            manifest_path: "packages.yaml".to_string(),
        })
    );
}

#[test]
fn rejects_named_registry_map() {
    let err = DependencyManifest::parse_yaml(
        r#"
schema: nenjo.dependencies.v1
registries:
  "@nenjo":
    kind: git
    url: https://github.com/nenjo-ai/packages.git
    reference: main
    manifest_path: packages.yaml
"#,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("failed to parse nenpm dependency manifest"));
}

#[test]
fn loads_yaml_dependency_manifest() {
    let loaded = DependencyManifest::load_from_dir(fixture("yaml")).unwrap();

    assert_eq!(loaded.path.file_name().unwrap(), "nenpm.yaml");
    assert_eq!(loaded.manifest.dependencies["@nenjo/nenji"], "^0.1.0");
}

#[test]
fn rejects_directory_with_both_yml_and_yaml() {
    let err = DependencyManifest::load_from_dir(fixture("both")).unwrap_err();

    assert!(matches!(err, NenpmError::DependencyManifest { .. }));
    assert!(
        err.to_string()
            .contains("found both nenpm.yml and nenpm.yaml")
    );
}

#[test]
fn rejects_missing_dependency_manifest() {
    let err = DependencyManifest::load_from_dir(fixture("missing"))
        .unwrap_err()
        .to_string();

    assert!(err.contains("missing nenpm.yml or nenpm.yaml"));
}

#[test]
fn rejects_invalid_schema() {
    let err = DependencyManifest::load_from_dir(fixture("invalid-schema"))
        .unwrap_err()
        .to_string();

    assert!(err.contains("failed to load"));
}

#[test]
fn rejects_invalid_file_shorthand() {
    let err = DependencyManifest::load_from_dir(fixture("bad-file-shorthand"))
        .unwrap_err()
        .to_string();

    assert!(err.contains("failed to load"));
}

#[test]
fn parses_file_shorthand_without_manifest_path() {
    let manifest = DependencyManifest::parse_yaml(
        r#"
schema: nenjo.dependencies.v1
dependencies:
  "@nenjo/core": "^0.1.0"
overrides:
  "@nenjo/core": file:../packages
"#,
    )
    .unwrap();

    let DependencyOverride::Shorthand(raw) = &manifest.overrides["@nenjo/core"] else {
        panic!("expected shorthand override");
    };
    assert_eq!(raw, "file:../packages");
    assert_eq!(
        manifest.overrides["@nenjo/core"]
            .to_package_source()
            .unwrap(),
        PackageSource::Local {
            root: PathBuf::from("../packages"),
            manifest_path: "packages.yaml".to_string(),
            scope: None,
        }
    );
}
