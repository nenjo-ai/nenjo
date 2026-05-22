#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use nenjo_packages::sha256_hex;

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("install")
        .join(name)
}

pub fn temp_workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("nenpm-install-{name}-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

pub fn copy_dir(from: &Path, to: &Path) {
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

pub fn write_file(root: &Path, path: &str, content: &str) {
    let full_path = root.join(path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full_path, content).unwrap();
}

pub fn write_minimal_registry(root: &Path, module: &str) {
    write_file(
        root,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "agent": packages/agent/nenjo.package.yaml
"#,
    );
    write_file(
        root,
        "packages/agent/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "agent"
version: "0.1.0"
modules:
  - agent.yaml
"#,
    );
    write_file(root, "packages/agent/agent.yaml", module);
}

pub fn write_artifact(source: &Path, artifact: &Path) -> String {
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
