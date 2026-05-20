use std::fs;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use nenjo_nenpm::{
    AddOptions, DependencyManifest, InfoOptions, InstallOptions, ListOptions, NenpmLock,
    PackageSpec, RemoveOptions, add, info, install, list, remove, update,
};
use nenjo_packages::sha256_hex;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("install")
        .join(name)
}

fn temp_workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("nenpm-install-{name}-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target);
        } else {
            fs::copy(&source, &target).unwrap();
        }
    }
}

fn write_file(root: &Path, path: &str, content: &str) {
    let full_path = root.join(path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full_path, content).unwrap();
}

fn write_artifact(source: &Path, artifact: &Path) -> String {
    if let Some(parent) = artifact.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let file = fs::File::create(artifact).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.append_dir_all(".", source).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    sha256_hex(&fs::read(artifact).unwrap())
}

#[test]
fn install_resolves_local_repository_override_and_writes_lockfile() {
    let workspace = temp_workspace("local-workspace");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");

    let report = install(InstallOptions::new(&project)).unwrap();

    assert!(report.wrote_lockfile);
    assert_eq!(report.lockfile_path, project.join("nenpm.lock.yml"));
    assert!(report.lockfile_path.exists());
    assert_eq!(
        report
            .plan
            .packages()
            .map(|package| package.name.to_string())
            .collect::<Vec<_>>(),
        vec!["@acme/core".to_string(), "@acme/agent".to_string()]
    );

    let lock: NenpmLock =
        serde_yaml::from_str(&fs::read_to_string(&report.lockfile_path).unwrap()).unwrap();
    assert_eq!(lock.schema, "nenjo.lock.v1");
    assert_eq!(lock.packages.len(), 2);
    assert_eq!(lock.packages[0].name, "@acme/core");
    assert_eq!(lock.packages[1].name, "@acme/agent");
    assert_eq!(lock.packages[0].modules.len(), 2);
    assert_eq!(lock.packages[1].modules.len(), 3);
    assert!(
        lock.packages[1]
            .modules
            .iter()
            .any(|module| module.path == "agents/support.yaml"
                && module.resource.as_deref() == Some("support_agent")
                && module.imports.len() == 2)
    );

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_dry_run_does_not_write_lockfile() {
    let workspace = temp_workspace("dry-run");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");

    let report = install(InstallOptions::new(&project).dry_run(true)).unwrap();

    assert!(!report.wrote_lockfile);
    assert!(!project.join("nenpm.lock.yml").exists());
    assert_eq!(report.lockfile.packages.len(), 2);

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_resolves_specific_package_manifest_overrides() {
    let workspace = temp_workspace("specific-package");
    copy_dir(&fixture("specific-package"), &workspace);
    let project = workspace.join("project");

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.lockfile.packages.len(), 2);
    assert_eq!(report.lockfile.packages[0].name, "@acme/core");
    assert_eq!(report.lockfile.packages[1].name, "@acme/agent");
    assert_eq!(
        report.lockfile.packages[1].dependencies["@acme/core"],
        "^0.1.0"
    );

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_resolves_registry_dependency_and_writes_lockfile() {
    let workspace = temp_workspace("registry-workspace");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");

    let report = install(InstallOptions::new(&project)).unwrap();

    assert!(report.wrote_lockfile);
    assert_eq!(
        report
            .lockfile
            .packages
            .iter()
            .map(|package| format!("{}@{}", package.name, package.version))
            .collect::<Vec<_>>(),
        vec!["@acme/core@0.2.0", "@acme/agent@0.2.0"]
    );
    assert_eq!(
        report.lockfile.packages[1].dependencies["@acme/core"],
        "^0.2.0"
    );
    assert!(
        report.lockfile.packages[1]
            .modules
            .iter()
            .any(|module| module.name == "support_agent" && module.imports.len() == 1)
    );

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_preserves_existing_lockfile_version_pin() {
    let workspace = temp_workspace("preserve-lock");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies:
  "@acme/agent": "^0.1.0"

registries:
  default: ../registry/registry.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.lock.yml",
        r#"
schema: nenjo.lock.v1
packages:
  - name: "@acme/core"
    version: "0.1.0"
    manifest_path: packages/core-v010/nenjo.package.yaml
    hash: old
    dependencies: {}
    modules: []
  - name: "@acme/agent"
    version: "0.1.0"
    manifest_path: packages/agent-v010/nenjo.package.yaml
    hash: old
    dependencies:
      "@acme/core": "^0.1.0"
    modules: []
"#,
    );

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.lockfile.packages.len(), 2);
    assert_eq!(report.lockfile.packages[0].version, "0.1.0");
    assert_eq!(report.lockfile.packages[1].version, "0.1.0");
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
  "@acme/agent": "^0.1.0"

registries:
  default: ../registry/registry.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.lock.yml",
        r#"
schema: nenjo.lock.v1
packages:
  - name: "@acme/core"
    version: "0.1.0"
    manifest_path: packages/core-v010/nenjo.package.yaml
    hash: old
    dependencies: {}
    modules: []
  - name: "@acme/agent"
    version: "0.1.0"
    manifest_path: packages/agent-v010/nenjo.package.yaml
    hash: old
    dependencies:
      "@acme/core": "^0.1.0"
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
        vec!["@acme/core@0.2.0", "@acme/agent@0.2.0"]
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
  "@acme/core": "^0.1.0"

registries:
  default: ../registry/registry.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.lock.yml",
        r#"
schema: nenjo.lock.v1
packages:
  - name: "@acme/core"
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
        .find(|package| package.name == "@acme/core")
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
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies: {}

registries:
  default: ../registry/registry.yaml
"#,
    );

    let report = add(AddOptions::new(
        &project,
        PackageSpec::parse("@acme/agent@^0.2.0").unwrap(),
    ))
    .unwrap();

    let manifest = DependencyManifest::load_from_dir(&project)
        .unwrap()
        .manifest;
    assert_eq!(manifest.dependencies["@acme/agent"], "^0.2.0");
    assert_eq!(report.install.lockfile.packages[1].name, "@acme/agent");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn add_dry_run_resolves_without_persisting_manifest_change() {
    let workspace = temp_workspace("add-dry-run");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies: {}

registries:
  default: ../registry/registry.yaml
"#,
    );
    let original = fs::read_to_string(project.join("nenpm.yml")).unwrap();

    let report = add(
        AddOptions::new(&project, PackageSpec::parse("@acme/agent@^0.2.0").unwrap()).dry_run(true),
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(project.join("nenpm.yml")).unwrap(),
        original
    );
    assert!(!project.join("nenpm.lock.yml").exists());
    assert_eq!(report.install.lockfile.packages[1].name, "@acme/agent");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn remove_updates_dependency_manifest_and_prunes_lockfile() {
    let workspace = temp_workspace("remove-command");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();

    let report = remove(RemoveOptions::new(&project, "@acme/agent")).unwrap();

    let manifest = fs::read_to_string(project.join("nenpm.yml")).unwrap();
    assert!(!manifest.contains("@acme/agent"));
    assert!(report.install.lockfile.packages.is_empty());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn remove_dry_run_resolves_without_persisting_manifest_change() {
    let workspace = temp_workspace("remove-dry-run");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();
    let original = fs::read_to_string(project.join("nenpm.yml")).unwrap();

    let report = remove(RemoveOptions::new(&project, "@acme/agent").dry_run(true)).unwrap();

    assert_eq!(
        fs::read_to_string(project.join("nenpm.yml")).unwrap(),
        original
    );
    assert!(report.install.lockfile.packages.is_empty());
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn list_reads_current_lockfile() {
    let workspace = temp_workspace("list-command");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();

    let lockfile = list(ListOptions::new(&project)).unwrap();

    assert_eq!(lockfile.packages.len(), 2);
    assert_eq!(lockfile.packages[1].name, "@acme/agent");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn info_reads_package_versions_from_default_registry() {
    let workspace = temp_workspace("info-command");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");

    let package_info = info(InfoOptions::new(&project, "@acme/agent")).unwrap();

    assert_eq!(package_info.versions.len(), 2);
    assert_eq!(package_info.versions[0].version, "0.1.0");
    assert_eq!(package_info.versions[1].version, "0.2.0");
    assert_eq!(
        package_info.versions[1].dependencies["@acme/core"],
        "^0.2.0"
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_uses_override_before_registry() {
    let workspace = temp_workspace("override-before-registry");
    copy_dir(&fixture("registry-workspace"), &workspace);
    copy_dir(
        &fixture("specific-package").join("packages"),
        &workspace.join("override-packages"),
    );
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies:
  "@acme/agent": "^0.1.0"

registries:
  default: ../registry/registry.yaml

overrides:
  "@acme/agent": file:../override-packages#packages/agent/nenjo.package.yaml
  "@acme/core": file:../override-packages#packages/core/nenjo.package.yaml
"#,
    );

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(
        report
            .lockfile
            .packages
            .iter()
            .map(|package| format!("{}@{}", package.name, package.version))
            .collect::<Vec<_>>(),
        vec!["@acme/core@0.1.0", "@acme/agent@0.1.0"]
    );

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_fetches_registry_artifact_sources() {
    let workspace = temp_workspace("registry-artifact");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let packages = workspace.join("packages");
    let artifact = workspace.join("registry").join("packages.tar.gz");
    let checksum = write_artifact(&packages, &artifact);
    let project = workspace.join("project");
    write_file(
        &workspace.join("registry"),
        "registry.yaml",
        &format!(
            r#"
schema: nenjo.registry.v1
packages:
  "@acme/core":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/core-v020/nenjo.package.yaml
  "@acme/agent":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "@acme/core": "^0.2.0"
"#
        ),
    );

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.lockfile.packages[0].name, "@acme/core");
    assert_eq!(report.lockfile.packages[1].name, "@acme/agent");
    assert_eq!(report.lockfile.packages[1].modules[0].name, "support_agent");
    assert!(report.lockfile.packages[1].source.is_some());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_rejects_changed_locked_artifact_contents() {
    let workspace = temp_workspace("locked-artifact-integrity");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let packages = workspace.join("packages");
    let artifact = workspace.join("registry").join("packages.tar.gz");
    let checksum = write_artifact(&packages, &artifact);
    write_file(
        &workspace.join("registry"),
        "registry.yaml",
        &format!(
            r#"
schema: nenjo.registry.v1
packages:
  "@acme/core":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/core-v020/nenjo.package.yaml
  "@acme/agent":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "@acme/core": "^0.2.0"
"#
        ),
    );
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();

    write_file(
        &packages,
        "packages/agent-v020/agents/support.yaml",
        r#"
schema: nenjo.agent.v1
manifest:
  name: support_agent
  imports:
    context:
      - "@acme/core/methodology"
  prompt_config:
    system_prompt: |
      {{ @acme/core/methodology }}
      changed
"#,
    );
    let changed_checksum = write_artifact(&packages, &artifact);
    write_file(
        &workspace.join("registry"),
        "registry.yaml",
        &format!(
            r#"
schema: nenjo.registry.v1
packages:
  "@acme/core":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{changed_checksum}"
        manifest_path: packages/core-v020/nenjo.package.yaml
  "@acme/agent":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{changed_checksum}"
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "@acme/core": "^0.2.0"
"#
        ),
    );

    let err = install(InstallOptions::new(&project))
        .unwrap_err()
        .to_string();

    assert!(err.contains("hash changed"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_rejects_registry_metadata_that_differs_from_package_manifest() {
    let workspace = temp_workspace("registry-metadata-mismatch");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &workspace.join("registry"),
        "registry.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "@acme/agent":
    - version: "0.2.0"
      source:
        kind: local
        root: ../packages
        manifest_path: packages/agent-v020/nenjo.package.yaml
"#,
    );

    let err = install(InstallOptions::new(&project))
        .unwrap_err()
        .to_string();

    assert!(err.contains("does not match source manifest dependencies"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_rejects_registry_package_checksum_mismatch() {
    let workspace = temp_workspace("registry-checksum-mismatch");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &workspace.join("registry"),
        "registry.yaml",
        r#"
schema: nenjo.registry.v1
packages:
  "@acme/agent":
    - version: "0.2.0"
      checksum: "sha256:not-the-package-manifest-hash"
      source:
        kind: local
        root: ../packages
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "@acme/core": "^0.2.0"
  "@acme/core":
    - version: "0.2.0"
      source:
        kind: local
        root: ../packages
        manifest_path: packages/core-v020/nenjo.package.yaml
"#,
    );

    let err = install(InstallOptions::new(&project))
        .unwrap_err()
        .to_string();

    assert!(err.contains("registry checksum"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_rejects_dependency_without_override_or_registry() {
    let workspace = temp_workspace("missing-override");
    copy_dir(&fixture("missing-override"), &workspace);
    let project = workspace.join("project");

    let err = install(InstallOptions::new(&project))
        .unwrap_err()
        .to_string();

    assert!(err.contains("requires registry resolution"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_rejects_missing_registry_package() {
    let workspace = temp_workspace("missing-registry-package");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies:
  "@acme/missing": "^0.1.0"

registries:
  default: ../registry/registry.yaml
"#,
    );

    let err = install(InstallOptions::new(&project))
        .unwrap_err()
        .to_string();

    assert!(err.contains("failed to resolve @acme/missing from registry"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_rejects_unsatisfied_root_requirement() {
    let workspace = temp_workspace("unsatisfied");
    copy_dir(&fixture("unsatisfied"), &workspace);
    let project = workspace.join("project");

    let err = install(InstallOptions::new(&project))
        .unwrap_err()
        .to_string();

    assert!(err.contains("@acme/agent resolved to 0.1.0"));
    assert!(err.contains("does not satisfy ^2.0.0"));
    fs::remove_dir_all(workspace).unwrap();
}
