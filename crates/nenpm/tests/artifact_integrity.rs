mod support;

use std::fs;

use nenjo_nenpm::{InstallOptions, install};

use support::{copy_dir, fixture, temp_workspace, write_artifact, write_file};

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
  "core":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/core-v020/nenjo.package.yaml
  "agent":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "core": "^0.2.0"
"#
        ),
    );

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.lockfile.packages[0].name, "core");
    assert_eq!(report.lockfile.packages[1].name, "agent");
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
  "core":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/core-v020/nenjo.package.yaml
  "agent":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{checksum}"
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "core": "^0.2.0"
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
  prompt_config:
    system_prompt: |
      {{ pkg.acme.core.methodology }}
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
  "core":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{changed_checksum}"
        manifest_path: packages/core-v020/nenjo.package.yaml
  "agent":
    - version: "0.2.0"
      source:
        kind: artifact
        url: packages.tar.gz
        checksum: "{changed_checksum}"
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "core": "^0.2.0"
"#
        ),
    );

    let err = format!("{:?}", install(InstallOptions::new(&project)).unwrap_err());

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
  "agent":
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
  "agent":
    - version: "0.2.0"
      checksum: "sha256:not-the-package-manifest-hash"
      source:
        kind: local
        root: ../packages
        manifest_path: packages/agent-v020/nenjo.package.yaml
      dependencies:
        "core": "^0.2.0"
  "core":
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
