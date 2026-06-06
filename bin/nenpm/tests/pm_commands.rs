use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn nenpm() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nenpm"))
}

fn temp_workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("nenpm-cli-{name}-{}", std::process::id()));
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

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/nenpm/tests/fixtures")
        .join(path)
}

fn copy_dir(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

fn write_local_registry(root: &Path, scope: &str) {
    write_file(
        root,
        "nenpm.yml",
        &format!(
            r#"schema: nenjo.dependencies.v1
registries:
  - kind: local
    scope: "{scope}"
    root: packages
    manifest_path: packages.yaml
"#
        ),
    );
    write_file(
        root,
        "packages/packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  agent: packages/agent/nenjo.package.yaml
"#,
    );
    write_file(
        root,
        "packages/packages/agent/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: agent
version: "0.1.0"
description: Local test agent.
modules:
  - agent.yaml
"#,
    );
    write_file(
        root,
        "packages/packages/agent/agent.yaml",
        r#"schema: nenjo.agent.v1
manifest:
  name: agent
  description: Local test agent entrypoint.
"#,
    );
}

#[test]
fn init_creates_nenpm_yml() {
    let workspace = temp_workspace("init");

    let output = nenpm()
        .args(["init", "--root"])
        .arg(&workspace)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let manifest = fs::read_to_string(workspace.join("nenpm.yml")).unwrap();
    assert!(manifest.contains("schema: nenjo.dependencies.v1"));

    let second = nenpm()
        .args(["init", "--root"])
        .arg(&workspace)
        .output()
        .unwrap();
    assert!(!second.status.success());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn upgrade_reresolves_package_versions() {
    let workspace = temp_workspace("upgrade");
    copy_dir(&fixture("install/registry-workspace"), &workspace);
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

    let output = nenpm()
        .args(["upgrade", "--root"])
        .arg(&project)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let lockfile = fs::read_to_string(project.join("nenpm.lock.yml")).unwrap();
    assert!(lockfile.contains("0.2.0"));
    assert!(lockfile.contains("agent"));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn upgrade_help_documents_major_flag() {
    let output = nenpm().args(["upgrade", "--help"]).output().unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--major"));
    assert!(stdout.contains("new major version"));
}

#[test]
fn add_org_writes_github_registry_to_manifest() {
    let workspace = temp_workspace("add-registry");

    let output = nenpm()
        .args(["add", "@nenjo-ai", "--ref", "feat/v2", "--root"])
        .arg(&workspace)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let manifest = fs::read_to_string(workspace.join("nenpm.yml")).unwrap();
    assert!(manifest.contains("kind: git"));
    assert!(manifest.contains("https://github.com/nenjo-ai/packages.git"));
    assert!(manifest.contains("reference: feat/v2"));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn list_prints_configured_registry_packages() {
    let workspace = temp_workspace("list");
    write_local_registry(&workspace, "@acme");

    let output = nenpm()
        .args(["list", "--root"])
        .arg(&workspace)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("@acme/agent"));
    assert!(stdout.contains("0.1.0"));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn list_filters_by_registry_scope() {
    let workspace = temp_workspace("list-scope");
    write_local_registry(&workspace, "@acme");

    let output = nenpm()
        .args(["list", "@acme", "--root"])
        .arg(&workspace)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("@acme/agent"));

    let missing = nenpm()
        .args(["list", "@missing", "--root"])
        .arg(&workspace)
        .output()
        .unwrap();

    assert!(!missing.status.success());
    let stderr = String::from_utf8(missing.stderr).unwrap();
    assert!(stderr.contains("no configured registry matches @missing"));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn info_prints_package_modules() {
    let workspace = temp_workspace("info");
    write_local_registry(&workspace, "@acme");

    let output = nenpm()
        .args(["info", "@acme/agent", "--root"])
        .arg(&workspace)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("@acme/agent"));
    assert!(stdout.contains("Local test agent."));
    assert!(stdout.contains("source local"));
    assert!(stdout.contains("modules"));
    assert!(stdout.contains("agent agent agent.yaml"));
    assert!(stdout.contains("Local test agent entrypoint."));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_accepts_custom_packages_dir() {
    let workspace = temp_workspace("install-packages-dir");
    write_local_registry(&workspace, "@acme");
    let manifest = fs::read_to_string(workspace.join("nenpm.yml")).unwrap();
    write_file(
        &workspace,
        "nenpm.yml",
        &(manifest
            + r#"
dependencies:
  "@acme/agent": "^0.1.0"
"#),
    );
    let packages_dir = workspace.join("custom-packages");

    let output = nenpm()
        .args(["install", "--root"])
        .arg(&workspace)
        .args(["--packages-dir"])
        .arg(&packages_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(packages_dir.join("@acme/agent@0.1.0").exists());
    assert!(packages_dir.join(".nenpm-index.json").exists());
    assert!(!workspace.join(".nenjo").exists());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn clean_removes_custom_packages_dir() {
    let workspace = temp_workspace("clean-packages-dir");
    write_local_registry(&workspace, "@acme");
    let manifest = fs::read_to_string(workspace.join("nenpm.yml")).unwrap();
    write_file(
        &workspace,
        "nenpm.yml",
        &(manifest
            + r#"
dependencies:
  "@acme/agent": "^0.1.0"
"#),
    );
    let packages_dir = workspace.join("custom-packages");
    let install = nenpm()
        .args(["install", "--root"])
        .arg(&workspace)
        .args(["--packages-dir"])
        .arg(&packages_dir)
        .output()
        .unwrap();
    assert!(install.status.success(), "{install:?}");

    let output = nenpm()
        .args(["clean", "--root"])
        .arg(&workspace)
        .args(["--packages-dir"])
        .arg(&packages_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(!packages_dir.exists());
    assert!(workspace.join("nenpm.yml").exists());
    assert!(workspace.join("nenpm.lock.yml").exists());

    fs::remove_dir_all(workspace).unwrap();
}
