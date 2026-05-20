use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::write::GzEncoder;
use nenjo_nenpm::{
    InMemoryRegistry, InstallPlan, PackageSource, RegistryPackageResolver, RegistryPackageVersion,
};
use nenjo_packages::{PackageKind, sha256_hex};

fn temp_repo(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("nenpm-{name}-{}", std::process::id()));
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

fn create_fixture_package_repo(name: &str) -> PathBuf {
    let root = temp_repo(name);
    write_file(
        &root,
        "packages.yaml",
        r#"
schema: nenjo.repository.v1
packages:
  "@nenjo/core": packages/core/nenjo.package.yaml
  "@nenjo/nenji": packages/nenji/nenjo.package.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "@nenjo/core"
version: "0.1.0"
modules:
  - context_blocks/methodology.yaml
"#,
    );
    write_file(
        &root,
        "packages/core/context_blocks/methodology.yaml",
        r#"
schema: nenjo.context_block.v1
manifest:
  name: methodology
"#,
    );
    write_file(
        &root,
        "packages/nenji/nenjo.package.yaml",
        r#"
schema: nenjo.package.v1
name: "@nenjo/nenji"
version: "0.1.0"
dependencies:
  "@nenjo/core": "^0.1.0"
modules:
  - agents/nenji.yaml
"#,
    );
    write_file(
        &root,
        "packages/nenji/agents/nenji.yaml",
        r#"
schema: nenjo.agent.v1
manifest:
  name: nenji
"#,
    );
    root
}

fn registry_for_fixture(source: impl Fn(&str) -> PackageSource) -> InMemoryRegistry {
    InMemoryRegistry::new()
        .with_version(RegistryPackageVersion {
            name: "@nenjo/core".to_string(),
            version: "0.1.0".to_string(),
            source: source("packages/core/nenjo.package.yaml"),
            dependencies: BTreeMap::new(),
            checksum: None,
        })
        .with_version(RegistryPackageVersion {
            name: "@nenjo/nenji".to_string(),
            version: "0.1.0".to_string(),
            source: source("packages/nenji/nenjo.package.yaml"),
            dependencies: BTreeMap::from([("@nenjo/core".to_string(), "^0.1.0".to_string())]),
            checksum: None,
        })
}

#[test]
fn local_install_plan_orders_packages_and_modules() {
    let root = create_fixture_package_repo("install-plan");

    let plan = InstallPlan::from_local_repository(&root, "@nenjo/nenji").unwrap();
    let packages: Vec<_> = plan.packages().collect();
    assert_eq!(packages[0].name, "@nenjo/core");
    assert_eq!(packages[1].name, "@nenjo/nenji");
    assert_eq!(packages[0].modules[0].kind, PackageKind::ContextBlock);
    assert_eq!(packages[1].modules[0].kind, PackageKind::Agent);
    assert_eq!(packages[1].modules[0].name, "nenji");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_local_source_plan_uses_registry_version_contract() {
    let root = create_fixture_package_repo("registry-local");
    let registry = registry_for_fixture(|manifest_path| PackageSource::Local {
        root: root.clone(),
        manifest_path: manifest_path.to_string(),
    });

    let resolver = RegistryPackageResolver::new(registry);
    let plan =
        InstallPlan::from_registry_local_sources(&resolver, "@nenjo/nenji", "^0.1.0").unwrap();
    let packages: Vec<_> = plan.packages().collect();
    assert_eq!(packages[0].name, "@nenjo/core");
    assert_eq!(packages[1].name, "@nenjo/nenji");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_records_git_sources_for_fetchers() {
    let source = PackageSource::Git {
        url: "https://github.com/nenjo-ai/packages.git".to_string(),
        reference: "v0.1.0".to_string(),
        manifest_path: "nenjo/nenji.package.yaml".to_string(),
    };
    assert_eq!(source.manifest_path(), Some("nenjo/nenji.package.yaml"));
}

#[test]
fn registry_resolves_from_git_source() {
    let source_repo = create_fixture_package_repo("git-source");
    assert!(
        Command::new("git")
            .arg("init")
            .arg(&source_repo)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source_repo)
            .arg("config")
            .arg("commit.gpgsign")
            .arg("false")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source_repo)
            .arg("config")
            .arg("tag.gpgSign")
            .arg("false")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source_repo)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source_repo)
            .arg("config")
            .arg("user.name")
            .arg("Nenpm Test")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source_repo)
            .arg("add")
            .arg(".")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source_repo)
            .arg("commit")
            .arg("-m")
            .arg("fixture")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source_repo)
            .arg("tag")
            .arg("v0.1.0")
            .status()
            .unwrap()
            .success()
    );

    let registry = registry_for_fixture(|manifest_path| PackageSource::Git {
        url: source_repo.to_string_lossy().to_string(),
        reference: "v0.1.0".to_string(),
        manifest_path: manifest_path.to_string(),
    });
    let resolver = RegistryPackageResolver::new(registry);
    let plan = InstallPlan::from_registry(&resolver, "@nenjo/nenji", "0.1.0").unwrap();
    let packages: Vec<_> = plan.packages().collect();
    assert_eq!(packages[0].name, "@nenjo/core");
    assert_eq!(packages[1].modules[0].kind, PackageKind::Agent);
    fs::remove_dir_all(source_repo).unwrap();
}

#[test]
fn registry_resolves_from_artifact_source() {
    let source = create_fixture_package_repo("artifact-source");
    let artifact = temp_repo("artifact-output").join("packages.tar.gz");
    let file = fs::File::create(&artifact).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.append_dir_all(".", &source).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    let checksum = sha256_hex(&fs::read(&artifact).unwrap());

    let registry = registry_for_fixture(|manifest_path| PackageSource::Artifact {
        url: artifact.to_string_lossy().to_string(),
        checksum: checksum.clone(),
        manifest_path: manifest_path.to_string(),
    });
    let resolver = RegistryPackageResolver::new(registry);
    let plan = InstallPlan::from_registry(&resolver, "@nenjo/nenji", "0.1.0").unwrap();
    let packages: Vec<_> = plan.packages().collect();
    assert_eq!(packages[0].name, "@nenjo/core");
    assert_eq!(packages[1].name, "@nenjo/nenji");
    fs::remove_dir_all(source).unwrap();
    fs::remove_dir_all(artifact.parent().unwrap()).unwrap();
}

#[test]
fn registry_resolves_from_remote_manifest_source() {
    let root = temp_repo("remote-source");
    write_file(
        &root,
        "remote.package.yaml",
        r#"
schema: nenjo.package.v1
name: "@nenjo/remote"
version: "0.1.0"
"#,
    );
    let manifest = root.join("remote.package.yaml");
    let checksum = sha256_hex(&fs::read(&manifest).unwrap());
    let registry = InMemoryRegistry::new().with_version(RegistryPackageVersion {
        name: "@nenjo/remote".to_string(),
        version: "0.1.0".to_string(),
        source: PackageSource::Remote {
            url: manifest.to_string_lossy().to_string(),
            checksum: Some(checksum),
        },
        dependencies: BTreeMap::new(),
        checksum: None,
    });
    let resolver = RegistryPackageResolver::new(registry);
    let plan = InstallPlan::from_registry(&resolver, "@nenjo/remote", "0.1.0").unwrap();
    let packages: Vec<_> = plan.packages().collect();
    assert_eq!(packages[0].name, "@nenjo/remote");
    assert!(packages[0].modules.is_empty());
    fs::remove_dir_all(root).unwrap();
}
