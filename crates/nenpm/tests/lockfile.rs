mod support;

use std::fs;

use nenjo_nenpm::{InstallOptions, install, package_install_path};

use support::{copy_dir, fixture, temp_workspace, write_file};

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

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.lockfile.packages.len(), 2);
    assert_eq!(report.lockfile.packages[0].version, "0.1.0");
    assert_eq!(report.lockfile.packages[1].version, "0.1.0");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn locked_install_rejects_missing_lockfile() {
    let workspace = temp_workspace("locked-missing-lockfile");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");

    let err = install(InstallOptions::new(&project).locked(true))
        .expect_err("locked install should require a lockfile")
        .to_string();

    assert!(err.contains("locked install requires"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn locked_install_rejects_out_of_date_lockfile() {
    let workspace = temp_workspace("locked-out-of-date-lockfile");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies:
  "core": "^0.2.0"

registries:
  - ../registry/registry.yaml
"#,
    );

    let err = install(InstallOptions::new(&project).locked(true))
        .expect_err("locked install should reject dependency drift")
        .to_string();

    assert!(err.contains("nenpm.lock.yml is out of date"));
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn locked_install_accepts_matching_lockfile() {
    let workspace = temp_workspace("locked-matching-lockfile");
    copy_dir(&fixture("registry-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();

    let report = install(InstallOptions::new(&project).locked(true)).unwrap();

    assert_eq!(report.lockfile.packages.len(), 2);
    assert!(package_install_path(&project, "agent", "0.2.0").exists());
    fs::remove_dir_all(workspace).unwrap();
}
