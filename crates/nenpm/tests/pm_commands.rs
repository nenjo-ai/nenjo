mod support;

use std::fs;

use nenjo_nenpm::{
    AddOptions, CleanOptions, DependencyManifest, InfoOptions, InitOptions, InstallOptions,
    ListOptions, PackageSource, PackageSpec, RemoveOptions, add, clean, info, init, install, list,
    package_install_path, remove, update,
};

use support::{copy_dir, fixture, temp_workspace, write_file};

#[test]
fn init_creates_starter_dependency_manifest() {
    let workspace = temp_workspace("init-command");

    let report = init(InitOptions::new(&workspace)).unwrap();

    assert_eq!(report.manifest_path, workspace.join("nenpm.yml"));
    let manifest = DependencyManifest::load_from_dir(&workspace)
        .unwrap()
        .manifest;
    assert_eq!(manifest.schema, "nenjo.dependencies.v1");
    assert!(manifest.dependencies.is_empty());
    assert!(manifest.registries.is_empty());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn init_rejects_existing_dependency_manifest() {
    let workspace = temp_workspace("init-existing");
    write_file(
        &workspace,
        "nenpm.yaml",
        r#"schema: nenjo.dependencies.v1
"#,
    );

    let err = init(InitOptions::new(&workspace)).unwrap_err().to_string();

    assert!(err.contains("dependency manifest already exists"));
    assert!(!workspace.join("nenpm.yml").exists());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn update_ignores_existing_lockfile_version_pin() {
    let workspace = temp_workspace("update-lock");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies:
  "agent": "^0.1.0"

registries:
  - ../registry/registry.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.lock.yml",
        r#"
schema: nenjo.lock.v1
packages:
  - name: "core"
    version: "0.1.0"
    manifest_path: packages/core-v010/nenjo.package.yaml
    hash: old
    dependencies: {}
    modules: []
  - name: "agent"
    version: "0.1.0"
    manifest_path: packages/agent-v010/nenjo.package.yaml
    hash: old
    dependencies:
      "core": "^0.1.0"
    modules: []
"#,
    );

    let report = update(InstallOptions::new(&project)).unwrap();

    assert_eq!(
        report
            .lockfile
            .packages
            .iter()
            .map(|package| format!("{}@{}", package.name, package.version))
            .collect::<Vec<_>>(),
        vec!["core@0.2.0", "agent@0.2.0"]
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn update_replaces_same_module_path_with_new_version_content() {
    let workspace = temp_workspace("update-same-module-path");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies:
  "core": "^0.1.0"

registries:
  - ../registry/registry.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.lock.yml",
        r#"
schema: nenjo.lock.v1
packages:
  - name: "core"
    version: "0.1.0"
    manifest_path: packages/core-v010/nenjo.package.yaml
    hash: old-package-hash
    dependencies: {}
    modules:
      - path: context/core.yaml
        source_path: packages/core-v010/context/core.yaml
        schema: nenjo.context_block.v1
        kind: context_block
        name: old_methodology
        hash: old-module-hash
"#,
    );

    let report = update(InstallOptions::new(&project)).unwrap();
    let package = report
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "core")
        .expect("core package is locked");
    let module = package
        .modules
        .iter()
        .find(|module| module.path == "context/core.yaml")
        .expect("core module is locked");

    assert_eq!(package.version, "0.2.0");
    assert_eq!(
        package.manifest_path,
        "packages/core-v020/nenjo.package.yaml"
    );
    assert_eq!(module.source_path, "packages/core-v020/context/core.yaml");
    assert_eq!(module.name, "methodology");
    assert_ne!(module.hash, "old-module-hash");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn add_updates_dependency_manifest_and_lockfile() {
    let workspace = temp_workspace("add-command");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &workspace,
        "packages/packages.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core-v020/nenjo.package.yaml
  "agent": packages/agent-v020/nenjo.package.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies: {}

registries:
  - kind: local
    scope: "@local"
    root: ../packages
    manifest_path: packages.yaml
"#,
    );

    let report = add(AddOptions::new(
        &project,
        PackageSpec::parse("@local/agent@^0.2.0").unwrap(),
    ))
    .unwrap();

    let manifest = DependencyManifest::load_from_dir(&project)
        .unwrap()
        .manifest;
    assert_eq!(manifest.dependencies["@local/agent"], "^0.2.0");
    assert_eq!(
        report.install.as_ref().unwrap().lockfile.packages[1].name,
        "@local/agent"
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn add_uses_scoped_repository_registry() {
    let workspace = temp_workspace("add-scoped-repository-registry");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &workspace,
        "packages/packages.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core-v020/nenjo.package.yaml
  "agent": packages/agent-v020/nenjo.package.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies: {}

registries:
  - kind: local
    scope: "@acme"
    root: ../packages
    manifest_path: packages.yaml
"#,
    );

    let report = add(AddOptions::new(
        &project,
        PackageSpec::parse("@acme/agent").unwrap(),
    ))
    .unwrap();

    let manifest = DependencyManifest::load_from_dir(&project)
        .unwrap()
        .manifest;
    assert_eq!(manifest.dependencies["@acme/agent"], "^0.2.0");
    assert_eq!(
        report.install.as_ref().unwrap().lockfile.packages[1].name,
        "@acme/agent"
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn add_wildcard_adds_all_registry_packages() {
    let workspace = temp_workspace("add-wildcard");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &workspace,
        "packages/packages.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core-v020/nenjo.package.yaml
  "agent": packages/agent-v020/nenjo.package.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies: {}

registries:
  - kind: local
    scope: "@acme"
    root: ../packages
    manifest_path: packages.yaml
"#,
    );

    let report = add(AddOptions::new(
        &project,
        PackageSpec::parse("@acme/*").unwrap(),
    ))
    .unwrap();

    let manifest = DependencyManifest::load_from_dir(&project)
        .unwrap()
        .manifest;
    assert_eq!(manifest.dependencies["@acme/agent"], "^0.2.0");
    assert_eq!(manifest.dependencies["@acme/core"], "^0.2.0");
    assert_eq!(report.dependencies_added.len(), 2);
    assert_eq!(
        report
            .install
            .as_ref()
            .unwrap()
            .lockfile
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>(),
        vec!["@acme/core", "@acme/agent"]
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn add_dry_run_resolves_without_persisting_manifest_change() {
    let workspace = temp_workspace("add-dry-run");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &workspace,
        "packages/packages.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "core": packages/core-v020/nenjo.package.yaml
  "agent": packages/agent-v020/nenjo.package.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies: {}

registries:
  - kind: local
    scope: "@local"
    root: ../packages
    manifest_path: packages.yaml
"#,
    );
    let original = fs::read_to_string(project.join("nenpm.yml")).unwrap();

    let report = add(
        AddOptions::new(&project, PackageSpec::parse("@local/agent@^0.2.0").unwrap()).dry_run(true),
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(project.join("nenpm.yml")).unwrap(),
        original
    );
    assert!(!project.join("nenpm.lock.yml").exists());
    assert_eq!(
        report.install.as_ref().unwrap().lockfile.packages[1].name,
        "@local/agent"
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn remove_updates_dependency_manifest_and_prunes_lockfile() {
    let workspace = temp_workspace("remove-command");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();
    assert!(package_install_path(&project, "agent", "0.2.0").exists());

    let report = remove(RemoveOptions::new(&project, "agent")).unwrap();

    let manifest = fs::read_to_string(project.join("nenpm.yml")).unwrap();
    assert!(!manifest.contains("agent"));
    assert!(report.install.lockfile.packages.is_empty());
    assert_eq!(report.install.materialization.pruned, 2);
    assert!(!package_install_path(&project, "agent", "0.2.0").exists());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn remove_dry_run_resolves_without_persisting_manifest_change() {
    let workspace = temp_workspace("remove-dry-run");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();
    let original = fs::read_to_string(project.join("nenpm.yml")).unwrap();
    let package_root = package_install_path(&project, "agent", "0.2.0");

    let report = remove(RemoveOptions::new(&project, "agent").dry_run(true)).unwrap();

    assert_eq!(
        fs::read_to_string(project.join("nenpm.yml")).unwrap(),
        original
    );
    assert!(report.install.lockfile.packages.is_empty());
    assert!(package_root.exists());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn clean_removes_default_package_install_artifacts_only() {
    let workspace = temp_workspace("clean-default");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();

    let report = clean(CleanOptions::new(&project)).unwrap();

    assert_eq!(report.package_count, 2);
    assert!(report.removed);
    assert!(!project.join(".nenjo/packages").exists());
    assert!(project.join("nenpm.yml").exists());
    assert!(project.join("nenpm.lock.yml").exists());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn clean_dry_run_preserves_package_install_artifacts() {
    let workspace = temp_workspace("clean-dry-run");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();

    let report = clean(CleanOptions::new(&project).dry_run(true)).unwrap();

    assert_eq!(report.package_count, 2);
    assert!(!report.removed);
    assert!(project.join(".nenjo/packages").exists());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn clean_respects_custom_packages_dir() {
    let workspace = temp_workspace("clean-custom-dir");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    let packages_dir = workspace.join("custom-packages");
    install(InstallOptions::new(&project).packages_dir(&packages_dir)).unwrap();

    let report = clean(CleanOptions::new(&project).packages_dir(&packages_dir)).unwrap();

    assert_eq!(report.package_count, 2);
    assert!(report.removed);
    assert!(!packages_dir.exists());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn list_reads_configured_registries() {
    let workspace = temp_workspace("list-command");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");

    let packages = list(ListOptions::new(&project)).unwrap();

    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0].name, "agent");
    assert_eq!(packages[0].versions, vec!["0.1.0", "0.2.0"]);
    assert_eq!(packages[1].name, "core");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn list_can_filter_by_registry_scope() {
    let workspace = temp_workspace("list-registry-scope");
    write_file(
        &workspace,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1
registries:
  - kind: local
    scope: "@acme"
    root: packages
    manifest_path: packages.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  agent: packages/agent/nenjo.package.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/packages/agent/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: agent
version: "0.1.0"
"#,
    );

    let packages = list(ListOptions::new(&workspace).registry("@acme")).unwrap();

    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].name, "@acme/agent");

    let err = list(ListOptions::new(&workspace).registry("@missing"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no configured registry matches @missing"));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn add_org_adds_github_registry_to_dependency_manifest() {
    let workspace = temp_workspace("add-registry-command");

    let report = add(
        AddOptions::new(&workspace, PackageSpec::parse("@nenjo-ai").unwrap()).reference("feat/v2"),
    )
    .unwrap();

    assert!(report.registry_added);
    assert!(report.install.is_none());
    let manifest = DependencyManifest::load_from_dir(&workspace)
        .unwrap()
        .manifest;
    assert_eq!(manifest.registries.len(), 1);
    assert_eq!(
        manifest.registries[0],
        nenjo_nenpm::RegistryReference::Source(PackageSource::Git {
            url: "https://github.com/nenjo-ai/packages.git".to_string(),
            reference: "feat/v2".to_string(),
            manifest_path: "packages.yaml".to_string(),
        })
    );

    let second = add(
        AddOptions::new(&workspace, PackageSpec::parse("@nenjo-ai").unwrap()).reference("feat/v2"),
    )
    .unwrap();
    assert!(!second.registry_added);

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn info_reads_package_versions_from_default_registry() {
    let workspace = temp_workspace("info-command");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");

    let package_info = info(InfoOptions::new(&project, "agent")).unwrap();

    assert_eq!(package_info.versions.len(), 2);
    assert_eq!(package_info.versions[0].version, "0.1.0");
    assert_eq!(package_info.versions[1].version, "0.2.0");
    assert_eq!(
        package_info.versions[1].description.as_deref(),
        Some("Support agent package.")
    );
    assert_eq!(package_info.versions[1].dependencies["core"], "^0.2.0");
    assert_eq!(package_info.versions[1].modules.len(), 1);
    assert_eq!(package_info.versions[1].modules[0].name, "support_agent");
    assert_eq!(
        package_info.versions[1].modules[0].description.as_deref(),
        Some("Handles support requests.")
    );
    fs::remove_dir_all(workspace).unwrap();
}
