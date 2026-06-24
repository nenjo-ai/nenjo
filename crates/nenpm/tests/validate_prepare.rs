mod support;

use std::fs;

use nenjo_nenpm::{PrepareOptions, ValidateOptions, prepare, validate, validate_with_progress};

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
fn validate_reports_runtime_validation_stages() {
    let workspace = temp_workspace("validate-progress-stages");
    copy_dir(&fixture("local-workspace"), &workspace);
    let packages = workspace.join("packages");
    let mut stages = Vec::new();

    validate_with_progress(ValidateOptions::new(&packages), |stage| {
        stages.push(stage.label().to_string());
    })
    .unwrap();

    assert_eq!(
        stages,
        vec![
            "discovering registry manifest",
            "loading registry manifest",
            "resolving package graph",
            "building render fixture",
            "validating module imports",
            "validating prompt selectors",
            "validating knowledge selectors",
            "validating assignments",
            "validating routine graphs",
            "strict-rendering prompts",
            "validating context graph",
        ]
    );
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
      {{ pkg.nenjo_ai.packages.context.knowledge.knowledge_routing }}
"#,
    );

    validate(ValidateOptions::new(&workspace)).unwrap();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_accepts_official_package_knowledge_selector_dependency() {
    let workspace = temp_workspace("validate-package-knowledge-selector");
    write_file(
        &workspace,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "agent": packages/agent/nenjo.package.yaml
  "knowledge": packages/knowledge/nenjo.package.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/knowledge/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "knowledge"
version: "0.1.0"
modules:
  - core/manifest.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/knowledge/core/manifest.yaml",
        r#"schema: nenjo.knowledge.v1
manifest:
  name: Core
  docs:
    - selector: orientation.nenjo
      source_path: docs/orientation.md
      title: Nenjo
      summary: Core orientation.
"#,
    );
    write_file(
        &workspace,
        "packages/knowledge/core/docs/orientation.md",
        "# Nenjo\n",
    );
    write_file(
        &workspace,
        "packages/agent/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "agent"
version: "0.1.0"
dependencies:
  knowledge: "^0.1.0"
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
      {{ pkg.nenjo_ai.packages.knowledge.core.orientation.nenjo }}
"#,
    );

    validate(ValidateOptions::new(&workspace)).unwrap();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_accepts_valid_routine_manifest_graph() {
    let workspace = temp_workspace("validate-routine-graph");
    write_file(
        &workspace,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "routines": packages/routines/nenjo.package.yaml
  "agents": packages/agents/nenjo.package.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/routines/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "routines"
version: "0.1.0"
modules:
  - review.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/agents/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "agents"
version: "0.1.0"
modules:
  - coder.yaml
  - reviewer.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/agents/coder.yaml",
        r#"schema: nenjo.agent.v1
manifest:
  name: coder
  prompt_config: {}
"#,
    );
    write_file(
        &workspace,
        "packages/agents/reviewer.yaml",
        r#"schema: nenjo.agent.v1
manifest:
  name: reviewer
  prompt_config: {}
"#,
    );
    write_file(
        &workspace,
        "packages/routines/review.yaml",
        r#"schema: nenjo.routine.v1
manifest:
  name: review_flow
  trigger: task
  entry_steps:
    - implement
  steps:
    - ref: implement
      name: Implement
      type: agent
      agent: packages/agents/coder.yaml
    - ref: review
      name: Review
      type: gate
      agent: packages/agents/reviewer.yaml
    - ref: done
      name: Done
      type: terminal
  edges:
    - from: implement
      to: review
      condition: on_pass
    - from: review
      to: done
      condition: on_pass
    - from: review
      to: implement
      condition: on_fail
      max_attempts: 2
"#,
    );

    validate(ValidateOptions::new(&workspace).registry("packages.yaml")).unwrap();
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn validate_rejects_invalid_routine_manifest_graph_with_context() {
    let workspace = temp_workspace("validate-invalid-routine-graph");
    write_file(
        &workspace,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "routines": packages/routines/nenjo.package.yaml
  "agents": packages/agents/nenjo.package.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/routines/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "routines"
version: "0.1.0"
modules:
  - review.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/agents/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "agents"
version: "0.1.0"
modules:
  - coder.yaml
  - reviewer.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/agents/coder.yaml",
        r#"schema: nenjo.agent.v1
manifest:
  name: coder
  prompt_config: {}
"#,
    );
    write_file(
        &workspace,
        "packages/agents/reviewer.yaml",
        r#"schema: nenjo.agent.v1
manifest:
  name: reviewer
  prompt_config: {}
"#,
    );
    write_file(
        &workspace,
        "packages/routines/review.yaml",
        r#"schema: nenjo.routine.v1
manifest:
  name: review_flow
  trigger: task
  entry_steps:
    - implement
  steps:
    - ref: implement
      name: Implement
      type: agent
      agent: packages/agents/coder.yaml
    - ref: review
      name: Review
      type: gate
      agent: packages/agents/reviewer.yaml
    - ref: done
      name: Done
      type: terminal
  edges:
    - from: implement
      to: review
      condition: on_pass
    - from: review
      to: done
      condition: always
"#,
    );

    let err = format!(
        "{:?}",
        validate(ValidateOptions::new(&workspace).registry("packages.yaml")).unwrap_err()
    );

    assert!(err.contains("routine graph validation failed"));
    assert!(err.contains("Gate step 'Review' must use on_pass/on_fail edges"));
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
