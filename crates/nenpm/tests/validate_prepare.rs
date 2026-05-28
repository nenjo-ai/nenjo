mod support;

use std::fs;

use nenjo_nenpm::{PrepareOptions, ValidateOptions, prepare, validate};

use support::{copy_dir, fixture, temp_workspace, write_file, write_minimal_registry};

#[test]
fn validate_accepts_registry_and_prepare_writes_compiled_metadata() {
    let workspace = temp_workspace("validate-registry");
    copy_dir(&fixture("local-workspace"), &workspace);
    let packages = workspace.join("packages");

    let report = validate(ValidateOptions::new(&packages)).unwrap();

    assert_eq!(report.registry_path, "packages.yaml");
    assert!(report.packages.contains_key("agent"));
    assert!(report.packages.contains_key("core"));

    let output = workspace.join("compiled.json");
    let prepared = prepare(PrepareOptions::new(&packages).output(&output)).unwrap();

    assert_eq!(prepared.output_path, output);
    assert!(prepared.output_path.exists());
    assert_eq!(prepared.compiled.schema, "nenjo.prepared_registry.v1");
    assert!(prepared.compiled.packages.iter().any(|package| {
        package.name == "agent"
            && package
                .modules
                .iter()
                .any(|module| module.prompt_package_selectors == vec!["pkg.acme.core".to_string()])
    }));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_rejects_manifest_imports() {
    let workspace = temp_workspace("validate-manifest-imports");
    write_minimal_registry(
        &workspace,
        r#"
schema: nenjo.agent.v1
manifest:
  name: broken
  imports:
    context:
      - ./context.yml
"#,
    );

    let err = format!(
        "{:?}",
        validate(ValidateOptions::new(&workspace)).unwrap_err()
    );

    assert!(
        err.contains("resource manifest body must not contain imports"),
        "{err}"
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_rejects_package_refs_in_module_imports() {
    let workspace = temp_workspace("validate-package-imports");
    write_minimal_registry(
        &workspace,
        r#"
schema: nenjo.agent.v1
imports:
  context:
    - "@acme/core/methodology"
manifest:
  name: broken
"#,
    );

    let err = format!(
        "{:?}",
        validate(ValidateOptions::new(&workspace)).unwrap_err()
    );

    assert!(err.contains("references a package"), "{err}");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_rejects_undeclared_pkg_selector() {
    let workspace = temp_workspace("validate-undeclared-selector");
    write_minimal_registry(
        &workspace,
        r#"
schema: nenjo.agent.v1
manifest:
  name: broken
  prompt_config:
    system_prompt: |
      {{ pkg.acme.core.methodology }}
"#,
    );

    let err = format!(
        "{:?}",
        validate(ValidateOptions::new(&workspace)).unwrap_err()
    );

    assert!(err.contains("is not the current package or a package dependency"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_rejects_context_selector_without_import() {
    let workspace = temp_workspace("validate-context-selector");
    write_minimal_registry(
        &workspace,
        r#"
schema: nenjo.context_block.v1
manifest:
  name: broken
  template: |
    {{ context.methodology }}
"#,
    );

    let err = format!(
        "{:?}",
        validate(ValidateOptions::new(&workspace)).unwrap_err()
    );

    assert!(err.contains("references context.methodology"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_does_not_treat_package_context_selector_as_local_context() {
    let workspace = temp_workspace("validate-package-context-selector");
    write_file(
        &workspace,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "agent": packages/agent/nenjo.package.yaml
  "context": packages/context/nenjo.package.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/context/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "context"
version: "0.1.0"
modules:
  - knowledge.yml
"#,
    );
    write_file(
        &workspace,
        "packages/context/knowledge.yml",
        r#"schema: nenjo.context_block.v1
manifest:
  name: knowledge_routing
  template: "Use knowledge carefully."
"#,
    );
    write_file(
        &workspace,
        "packages/agent/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "agent"
version: "0.1.0"
dependencies:
  context: "^0.1.0"
modules:
  - agent.yml
"#,
    );
    write_file(
        &workspace,
        "packages/agent/agent.yml",
        r#"schema: nenjo.agent.v1
manifest:
  name: agent
  prompt_config:
    developer_prompt: |
      {{ pkg.nenjo.context.knowledge.knowledge_routing }}
"#,
    );

    validate(ValidateOptions::new(&workspace)).unwrap();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_rejects_context_import_cycles() {
    let workspace = temp_workspace("validate-context-cycle");
    write_file(
        &workspace,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "agent": packages/agent/nenjo.package.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/agent/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "agent"
version: "0.1.0"
modules:
  - a.yml
  - b.yml
"#,
    );
    write_file(
        &workspace,
        "packages/agent/a.yml",
        r#"schema: nenjo.context_block.v1
imports:
  context:
    - ./b.yml
manifest:
  name: a
  template: "{{ context.b }}"
"#,
    );
    write_file(
        &workspace,
        "packages/agent/b.yml",
        r#"schema: nenjo.context_block.v1
imports:
  context:
    - ./a.yml
manifest:
  name: b
  template: "{{ context.a }}"
"#,
    );

    let err = format!(
        "{:?}",
        validate(ValidateOptions::new(&workspace)).unwrap_err()
    );

    assert!(err.contains("context import cycle"));
    fs::remove_dir_all(workspace).unwrap();
}
