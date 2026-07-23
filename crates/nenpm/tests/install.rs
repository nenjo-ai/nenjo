mod support;

use std::fs;

use nenjo_nenpm::{
    DependencyManifest, FetchMode, InstallOptions, NenpmLock, PackageInstallIndex, PackageSource,
    ResolveOptions, install, package_install_path, package_instance_key, resolve,
};
use nenjo_packages::{PackageResourceInstanceKey, PackageResourceLogicalKey};

use support::{copy_dir, fixture, temp_workspace, write_file};

#[test]
fn install_resolves_local_repository_override_and_writes_lockfile() {
    let workspace = temp_workspace("local-workspace");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");

    let report = install(InstallOptions::new(&project)).unwrap();

    assert!(report.wrote_lockfile);
    assert_eq!(report.materialization.installed, 2);
    assert_eq!(report.materialization.reused, 0);
    assert_eq!(report.materialization.pruned, 0);
    assert_eq!(report.lockfile_path, project.join("nenpm.lock.yml"));
    assert!(report.lockfile_path.exists());
    assert_eq!(
        report
            .plan
            .packages()
            .map(|package| package.name.to_string())
            .collect::<Vec<_>>(),
        vec!["core".to_string(), "agent".to_string()]
    );

    let lock: NenpmLock =
        serde_yaml::from_str(&fs::read_to_string(&report.lockfile_path).unwrap()).unwrap();
    assert_eq!(lock.schema, "nenjo.lock.v1");
    assert_eq!(lock.packages.len(), 2);
    assert_eq!(lock.packages[0].name, "core");
    assert_eq!(lock.packages[1].name, "agent");
    assert_eq!(lock.packages[0].modules.len(), 2);
    assert_eq!(lock.packages[1].modules.len(), 3);
    let support_agent = lock.packages[1]
        .modules
        .iter()
        .find(|module| module.path == "agents/support.yaml")
        .expect("support agent should be locked");
    assert_eq!(support_agent.resource, "support_agent");
    assert_eq!(support_agent.imports.len(), 1);
    let identity_name = support_agent
        .resource_path
        .as_ref()
        .expect("resource path should be locked")
        .identity_name();
    let expected_logical_ref = PackageResourceLogicalKey::legacy(
        "agent",
        support_agent.kind,
        &support_agent.path,
        identity_name.as_str(),
    )
    .unwrap();
    let expected_instance_key = PackageResourceInstanceKey::legacy(
        "agent",
        "0.1.0",
        support_agent.kind,
        &support_agent.path,
        identity_name.as_str(),
    )
    .unwrap();
    assert_eq!(
        support_agent.logical_ref.as_ref(),
        Some(&expected_logical_ref)
    );
    assert_eq!(
        support_agent.instance_key.as_ref(),
        Some(&expected_instance_key)
    );
    let agent_install = package_install_path(&project, "agent", "0.1.0");
    assert!(agent_install.join("nenjo.package.yaml").exists());
    assert!(agent_install.join("agents/support.yaml").exists());
    assert!(agent_install.join("abilities/design.yaml").exists());
    assert!(!agent_install.join("packages.yaml").exists());
    assert!(
        !agent_install
            .join("packages/core/nenjo.package.yaml")
            .exists()
    );

    let index =
        PackageInstallIndex::load_file(project.join(".nenjo/packages/.nenpm-index.json")).unwrap();
    let entry = index.get_package("agent", "0.1.0").unwrap();
    assert_eq!(entry.manifest_path, "nenjo.package.yaml");

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_adapts_local_claude_plugin_override() {
    let workspace = temp_workspace("claude-plugin-override");
    let project = workspace.join("project");
    let plugin = workspace.join("acme-plugin");
    fs::create_dir_all(&project).unwrap();
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  acme_tools: "0.1.0"

overrides:
  acme_tools:
    kind: local
    root: ../acme-plugin
    manifest_path: .claude-plugin/plugin.json
"#,
    );
    write_file(
        &plugin,
        ".claude-plugin/plugin.json",
        r#"{
  "name": "Acme Tools",
  "version": "0.1.0",
  "description": "Plugin fixture"
}"#,
    );
    write_file(
        &plugin,
        "skills/review/SKILL.md",
        r#"---
description: Review code changes.
---
Run $CLAUDE_SKILL_DIR/scripts/review.sh when useful.
"#,
    );
    write_file(
        &plugin,
        "skills/review/scripts/review.sh",
        "#!/usr/bin/env bash\necho review\n",
    );
    write_file(
        &plugin,
        ".mcp.json",
        r#"{
  "mcpServers": {
    "review-server": {
      "command": "node",
      "args": ["servers/review.js"]
    }
  }
}"#,
    );
    write_file(&plugin, "servers/review.js", "console.log('review');\n");

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.materialization.installed, 1);
    assert_eq!(report.lockfile.packages.len(), 1);
    let package = &report.lockfile.packages[0];
    assert_eq!(package.name, "acme_tools");
    assert_eq!(package.manifest_path, "package.yaml");
    assert!(
        package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Skill
                && module.resource == "Acme Tools: review")
    );
    assert!(package.modules.iter().any(|module| module.kind
        == nenjo_packages::PackageKind::McpServer
        && module.resource == "Acme Tools: review-server"));

    let install_root = package_install_path(&project, "acme_tools", "0.1.0");
    assert!(install_root.join("package.yaml").exists());
    assert!(
        install_root
            .join(".nenjo/generated/claude-plugin/skills/review.yaml")
            .exists()
    );
    assert!(install_root.join("skills/review/SKILL.md").exists());
    assert!(
        install_root
            .join("skills/review/scripts/review.sh")
            .exists()
    );

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_materializes_mixed_native_and_claude_plugin_dependencies() {
    let workspace = temp_workspace("mixed-native-and-claude-plugin");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");
    let plugin = workspace.join("acme-plugin");
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  "agent": "^0.1.0"
  acme_tools: "0.1.0"

overrides:
  "agent": file:../packages
  acme_tools:
    kind: local
    root: ../acme-plugin
    manifest_path: .claude-plugin/plugin.json
"#,
    );
    write_file(
        &plugin,
        ".claude-plugin/plugin.json",
        r#"{
  "name": "Acme Tools",
  "version": "0.1.0",
  "description": "Plugin fixture"
}"#,
    );
    write_file(
        &plugin,
        "commands/audit.md",
        r#"---
description: Run the audit loop.
---
Use the review skill and stop hook.
"#,
    );
    write_file(
        &plugin,
        "skills/review/SKILL.md",
        r#"---
description: Review code changes.
hooks:
  - Stop audit-stop
---
Run $CLAUDE_SKILL_DIR/scripts/review.sh when useful.
"#,
    );
    write_file(
        &plugin,
        "skills/review/scripts/review.sh",
        "#!/usr/bin/env bash\necho review\n",
    );
    write_file(
        &plugin,
        "hooks/hooks.json",
        r#"{
  "hooks": {
    "Stop": [
      {
        "matcher": "*",
        "hooks": [
          { "type": "command", "command": "scripts/audit-stop.sh" }
        ]
      }
    ]
  }
}"#,
    );
    write_file(
        &plugin,
        "scripts/audit-stop.sh",
        "#!/usr/bin/env bash\necho stop\n",
    );
    write_file(
        &plugin,
        ".mcp.json",
        r#"{
  "mcpServers": {
    "review-server": {
      "command": "node",
      "args": ["servers/review.js"]
    }
  }
}"#,
    );
    write_file(&plugin, "servers/review.js", "console.log('review');\n");

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.materialization.installed, 3);
    assert_eq!(report.lockfile.packages.len(), 3);
    let mut package_names = report
        .lockfile
        .packages
        .iter()
        .map(|package| format!("{}@{}", package.name, package.version))
        .collect::<Vec<_>>();
    package_names.sort();
    assert_eq!(
        package_names,
        vec![
            "acme_tools@0.1.0".to_string(),
            "agent@0.1.0".to_string(),
            "core@0.1.0".to_string()
        ]
    );

    let agent_package = report
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "agent")
        .unwrap();
    assert!(
        agent_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Agent
                && module.resource == "support_agent")
    );
    assert!(
        agent_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Ability
                && module.resource == "design_agent")
    );

    let plugin_package = report
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "acme_tools")
        .unwrap();
    assert_eq!(plugin_package.manifest_path, "package.yaml");
    assert!(
        plugin_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Plugin
                && module.resource == "Acme Tools")
    );
    assert!(
        plugin_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Skill
                && module.resource == "Acme Tools: review")
    );
    assert!(
        plugin_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Command
                && module.resource == "Acme Tools: audit")
    );
    assert!(
        plugin_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Hook
                && module.resource == "Acme Tools: Stop_audit-stop")
    );
    assert!(plugin_package.modules.iter().any(|module| module.kind
        == nenjo_packages::PackageKind::McpServer
        && module.resource == "Acme Tools: review-server"));

    let agent_install = package_install_path(&project, "agent", "0.1.0");
    assert!(agent_install.join("agents/support.yaml").exists());
    assert!(agent_install.join("abilities/design.yaml").exists());
    let plugin_install = package_install_path(&project, "acme_tools", "0.1.0");
    assert!(plugin_install.join("package.yaml").exists());
    assert!(
        plugin_install
            .join(".nenjo/generated/claude-plugin/commands/audit.yaml")
            .exists()
    );
    assert!(
        plugin_install
            .join(".nenjo/generated/claude-plugin/skills/review.yaml")
            .exists()
    );
    assert!(
        plugin_install
            .join(".nenjo/generated/claude-plugin/hooks/stop_audit_stop.yaml")
            .exists()
    );
    assert!(
        plugin_install
            .join(".nenjo/generated/claude-plugin/mcp/review_server.yaml")
            .exists()
    );
    assert!(plugin_install.join("skills/review/SKILL.md").exists());
    assert!(plugin_install.join("scripts/audit-stop.sh").exists());

    let index =
        PackageInstallIndex::load_file(project.join(".nenjo/packages/.nenpm-index.json")).unwrap();
    assert!(index.get_package("agent", "0.1.0").is_some());
    assert!(index.get_package("acme_tools", "0.1.0").is_some());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_reuses_verified_package_tree_on_second_run() {
    let workspace = temp_workspace("reuse-package-tree");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");

    let first = install(InstallOptions::new(&project)).unwrap();
    assert_eq!(first.materialization.installed, 2);
    assert_eq!(first.materialization.reused, 0);

    let agent_install = package_install_path(&project, "agent", "0.1.0");
    write_file(&agent_install, "cache-sentinel.txt", "kept when reused");

    let second = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(second.materialization.installed, 0);
    assert_eq!(second.materialization.reused, 2);
    assert_eq!(second.materialization.pruned, 0);
    assert!(agent_install.join("cache-sentinel.txt").exists());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_replaces_corrupt_cached_package_tree() {
    let workspace = temp_workspace("replace-corrupt-cache");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");
    install(InstallOptions::new(&project)).unwrap();

    let agent_install = package_install_path(&project, "agent", "0.1.0");
    write_file(
        &agent_install,
        "cache-sentinel.txt",
        "removed when replaced",
    );
    write_file(
        &agent_install,
        "agents/support.yaml",
        r#"schema: nenjo.agent.v1
manifest:
  name: support_agent
  prompt_config:
    system_prompt: corrupted
"#,
    );

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(report.materialization.installed, 1);
    assert_eq!(report.materialization.reused, 1);
    assert_eq!(report.materialization.pruned, 0);
    assert!(!agent_install.join("cache-sentinel.txt").exists());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_locks_and_verifies_command_content_sidecars() {
    let workspace = temp_workspace("command-sidecars");
    let project = workspace.join("project");
    let packages = workspace.join("packages");
    fs::create_dir_all(&project).unwrap();
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  "nenji": "^1.0.0"

overrides:
  "nenji": file:../packages
"#,
    );
    write_file(
        &packages,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "nenji": nenjo/nenji/package.yaml
"#,
    );
    write_file(
        &packages,
        "nenjo/nenji/package.yaml",
        r#"schema: nenjo.package.v1
name: "nenji"
version: "1.0.0"
modules:
  - commands/
"#,
    );
    write_file(
        &packages,
        "nenjo/nenji/commands/index.yml",
        r#"schema: nenjo.module_index.v1
modules:
  - design.yaml
"#,
    );
    write_file(
        &packages,
        "nenjo/nenji/commands/design.yaml",
        r#"schema: nenjo.command.v1
manifest:
  name: design
  command: /design
  content_path: nenjo/nenji/commands/design/command.md
"#,
    );
    write_file(
        &packages,
        "nenjo/nenji/commands/design/command.md",
        "Design the requested artifact.\n",
    );

    let first = install(InstallOptions::new(&project)).unwrap();
    let package = first
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "nenji")
        .unwrap();
    let command = package
        .modules
        .iter()
        .find(|module| module.kind == nenjo_packages::PackageKind::Command)
        .unwrap();
    assert_eq!(command.path, "commands/design.yaml");
    assert_eq!(command.files[0].path, "commands/design/command.md");

    let install_root = package_install_path(&project, "nenji", "1.0.0");
    let content_file = install_root.join("commands/design/command.md");
    assert!(content_file.exists());
    fs::remove_file(&content_file).unwrap();

    let second = install(InstallOptions::new(&project)).unwrap();
    assert_eq!(second.materialization.installed, 1);
    assert_eq!(second.materialization.reused, 0);
    assert!(content_file.exists());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_resolves_file_repository_manifest_registry() {
    let workspace = temp_workspace("file-repository-registry");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  "@acme/agent": "^0.1.0"

registries:
  - kind: local
    scope: "@acme"
    root: ../packages
    manifest_path: packages.yaml
"#,
    );

    let report = install(InstallOptions::new(&project)).unwrap();

    assert!(report.wrote_lockfile);
    assert_eq!(
        report
            .plan
            .packages()
            .map(|package| package.name.to_string())
            .collect::<Vec<_>>(),
        vec!["@acme/core".to_string(), "@acme/agent".to_string()]
    );
    assert!(package_install_path(&project, "@acme/agent", "0.1.0").exists());

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_can_materialize_into_custom_packages_dir() {
    let workspace = temp_workspace("custom-packages-dir");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");
    let packages_dir = workspace.join("custom-packages");

    let report = install(InstallOptions::new(&project).packages_dir(&packages_dir)).unwrap();

    assert_eq!(report.materialization.installed, 2);
    assert!(packages_dir.join("agent@0.1.0/nenjo.package.yaml").exists());
    assert!(packages_dir.join(".nenpm-index.json").exists());
    assert!(!project.join(".nenjo").exists());

    let second = install(InstallOptions::new(&project).packages_dir(&packages_dir)).unwrap();

    assert_eq!(second.materialization.installed, 0);
    assert_eq!(second.materialization.reused, 2);

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_includes_multiple_direct_dependencies() {
    let workspace = temp_workspace("multiple-direct-dependencies");
    copy_dir(&fixture("local-workspace"), &workspace);
    let project = workspace.join("project");

    write_file(
        &workspace,
        "packages/packages/tools/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: "tools"
version: "0.1.0"
modules:
  - context/tools.yaml
"#,
    );
    write_file(
        &workspace,
        "packages/packages/tools/context/tools.yaml",
        r#"schema: nenjo.context_block.v1
manifest:
  name: tools
  template: Use project tools carefully.
"#,
    );
    write_file(
        &workspace,
        "packages/packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  "core": packages/core/nenjo.package.yaml
  "agent": packages/agent/nenjo.package.yaml
  "tools": packages/tools/nenjo.package.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  "agent": "^0.1.0"
  "tools": "^0.1.0"

overrides:
  "agent": file:../packages
  "tools": file:../packages
"#,
    );

    let report = install(InstallOptions::new(&project)).unwrap();

    assert_eq!(
        report
            .plan
            .packages()
            .map(|package| package.name.to_string())
            .collect::<Vec<_>>(),
        vec!["core".to_string(), "agent".to_string(), "tools".to_string()]
    );
    assert!(package_install_path(&project, "agent", "0.1.0").exists());
    assert!(package_install_path(&project, "tools", "0.1.0").exists());

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
    assert!(!project.join("packages").exists());
    assert!(!project.join(".nenjo").exists());
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
    assert_eq!(report.lockfile.packages[0].name, "core");
    assert_eq!(report.lockfile.packages[1].name, "agent");
    assert_eq!(report.lockfile.packages[1].dependencies["core"], "^0.1.0");

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
        vec!["core@0.2.0", "agent@0.2.0"]
    );
    assert_eq!(report.lockfile.packages[1].dependencies["core"], "^0.2.0");
    assert_eq!(
        report.lockfile.packages[1].resolved_dependencies["core"],
        "0.2.0"
    );
    assert!(
        report.lockfile.packages[1]
            .modules
            .iter()
            .any(|module| module.resource == "support_agent" && module.imports.is_empty())
    );
    let package_root = package_install_path(&project, "agent", "0.2.0");
    assert!(package_root.join("nenjo.package.yaml").exists());
    assert!(package_root.join("agents/support.yaml").exists());
    assert!(!package_root.join("packages/agent-v020").exists());
    assert!(!package_root.join(".git").exists());
    let index =
        PackageInstallIndex::load_file(project.join(".nenjo/packages/.nenpm-index.json")).unwrap();
    let entry = index
        .packages
        .get(&package_instance_key("agent", "0.2.0"))
        .unwrap();
    assert_eq!(entry.root, ".nenjo/packages/agent@0.2.0");
    assert_eq!(entry.manifest_path, "nenjo.package.yaml");

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_accumulates_constraints_before_finalizing_registry_version() {
    let workspace = temp_workspace("registry-constraint-aggregation");
    let project = workspace.join("project");
    let registry = workspace.join("registry");
    let packages = workspace.join("packages");
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  app_a: "1.0.0"
  app_b: "1.0.0"

registries:
  - ../registry/registry.yaml
"#,
    );
    write_file(
        &registry,
        "registry.yaml",
        r#"schema: nenjo.registry.v1
packages:
  core:
    - version: "1.2.0"
      source:
        kind: local
        root: ../packages
        manifest_path: core-v120/nenjo.package.yaml
    - version: "1.9.0"
      source:
        kind: local
        root: ../packages
        manifest_path: core-v190/nenjo.package.yaml
  app_a:
    - version: "1.0.0"
      source:
        kind: local
        root: ../packages
        manifest_path: app-a/nenjo.package.yaml
      dependencies:
        core: "1.2.0"
  app_b:
    - version: "1.0.0"
      source:
        kind: local
        root: ../packages
        manifest_path: app-b/nenjo.package.yaml
      dependencies:
        core: "^1.0.0"
"#,
    );
    for (path, name, version, dependency) in [
        ("core-v120", "core", "1.2.0", None),
        ("core-v190", "core", "1.9.0", None),
        ("app-a", "app_a", "1.0.0", Some("1.2.0")),
        ("app-b", "app_b", "1.0.0", Some("^1.0.0")),
    ] {
        let dependencies = dependency.map_or_else(String::new, |requirement| {
            format!("dependencies:\n  core: \"{requirement}\"\n")
        });
        write_file(
            &packages,
            &format!("{path}/nenjo.package.yaml"),
            &format!(
                "schema: nenjo.package.v1\nname: {name}\nversion: \"{version}\"\n{dependencies}"
            ),
        );
    }

    let report = install(InstallOptions::new(&project).dry_run(true)).unwrap();
    let core = report
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "core")
        .expect("core is resolved");

    assert_eq!(core.version, "1.2.0");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_routes_scoped_packages_to_repository_registry_source() {
    let workspace = temp_workspace("scoped-repository-registry");
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

dependencies:
  "@acme/agent": "^0.2.0"

registries:
  - kind: local
    scope: "@acme"
    root: ../packages
    manifest_path: packages.yaml
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
        vec!["@acme/core@0.2.0", "@acme/agent@0.2.0"]
    );
    assert!(matches!(
        report.lockfile.packages[1].source.as_ref().unwrap(),
        PackageSource::Local { manifest_path, .. } if manifest_path == "packages/agent-v020/nenjo.package.yaml"
    ));

    let locked = install(InstallOptions::new(&project).locked(true)).unwrap();
    assert_eq!(locked.lockfile, report.lockfile);

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_resolves_external_registry_dependencies_advertised_by_registry_source() {
    let workspace = temp_workspace("external-registry-dependencies");
    let project = workspace.join("project");
    let foo = workspace.join("foo");
    let bar = workspace.join("bar");
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  "@foo/app": "^1.0.0"

registries:
  - kind: local
    scope: "@foo"
    root: ../foo
    manifest_path: packages.yaml
"#,
    );
    write_file(
        &foo,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
registries:
  - kind: local
    scope: "@bar"
    root: ../bar
    manifest_path: packages.yaml
packages:
  app: packages/app/nenjo.package.yaml
"#,
    );
    write_file(
        &foo,
        "packages/app/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: app
version: "1.0.0"
dependencies:
  "@bar/core": "^1.0.0"
"#,
    );
    write_file(
        &bar,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  core: packages/core/nenjo.package.yaml
"#,
    );
    write_file(
        &bar,
        "packages/core/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: core
version: "1.0.0"
"#,
    );

    let report = install(InstallOptions::new(&project).dry_run(true)).unwrap();

    let mut packages = report
        .lockfile
        .packages
        .iter()
        .map(|package| format!("{}@{}", package.name, package.version))
        .collect::<Vec<_>>();
    packages.sort();
    assert_eq!(
        packages,
        vec!["@bar/core@1.0.0".to_string(), "@foo/app@1.0.0".to_string()]
    );
    let app = report
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "@foo/app")
        .unwrap();
    assert_eq!(app.dependencies["@bar/core"], "^1.0.0");
    assert_eq!(app.resolved_dependencies["@bar/core"], "1.0.0");
    assert!(matches!(
        app.source.as_ref().unwrap(),
        PackageSource::Local { manifest_path, scope, .. }
            if manifest_path == "packages/app/nenjo.package.yaml"
                && scope.as_deref() == Some("@foo")
    ));
    let core = report
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "@bar/core")
        .unwrap();
    assert!(matches!(
        core.source.as_ref().unwrap(),
        PackageSource::Local { manifest_path, scope, .. }
            if manifest_path == "packages/core/nenjo.package.yaml"
                && scope.as_deref() == Some("@bar")
    ));

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn install_applies_source_scope_to_override_package_names() {
    let workspace = temp_workspace("source-scoped-override-package-names");
    let project = workspace.join("project");
    let repo = workspace.join("repo");
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  "@foo/app": "1.0.0"

overrides:
  "@foo/app":
    kind: local
    root: ../repo
    manifest_path: packages/app/nenjo.package.yaml
    scope: "@foo"
  "@foo/core":
    kind: local
    root: ../repo
    manifest_path: packages/core/nenjo.package.yaml
    scope: "@foo"
"#,
    );
    write_file(
        &repo,
        "packages/app/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: app
version: "1.0.0"
dependencies:
  core: "^1.0.0"
"#,
    );
    write_file(
        &repo,
        "packages/core/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: core
version: "1.0.0"
"#,
    );

    let report = install(InstallOptions::new(&project).dry_run(true)).unwrap();

    let mut packages = report
        .lockfile
        .packages
        .iter()
        .map(|package| format!("{}@{}", package.name, package.version))
        .collect::<Vec<_>>();
    packages.sort();
    assert_eq!(
        packages,
        vec!["@foo/app@1.0.0".to_string(), "@foo/core@1.0.0".to_string()]
    );
    let app = report
        .lockfile
        .packages
        .iter()
        .find(|package| package.name == "@foo/app")
        .unwrap();
    assert_eq!(app.dependencies["@foo/core"], "^1.0.0");
    assert_eq!(app.resolved_dependencies["@foo/core"], "1.0.0");

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn locked_install_uses_lockfile_without_loading_registries() {
    let workspace = temp_workspace("locked-install-without-registry-resolution");
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"schema: nenjo.dependencies.v1

dependencies:
  "@foo/app": "1.0.0"

registries:
  - kind: local
    scope: "@foo"
    root: ../missing-registry
    manifest_path: packages.yaml
"#,
    );
    write_file(
        &project,
        "nenpm.lock.yml",
        r#"schema: nenjo.lock.v1
packages:
  - name: "@foo/app"
    version: "1.0.0"
    manifest_path: packages/app/nenjo.package.yaml
    hash: sha256:app
    dependencies: {}
    modules: []
"#,
    );

    let report = install(InstallOptions::new(&project).locked(true).dry_run(true)).unwrap();

    assert_eq!(report.lockfile.packages[0].name, "@foo/app");
    assert_eq!(
        report
            .plan
            .packages()
            .map(|package| format!("{}@{}", package.name, package.version))
            .collect::<Vec<_>>(),
        vec!["@foo/app@1.0.0".to_string()]
    );

    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn resolve_accepts_manifest_without_dependency_file() {
    let workspace = temp_workspace("resolve-external-registry");
    let project = workspace.join("project");
    let foo = workspace.join("foo");
    let bar = workspace.join("bar");
    fs::create_dir_all(&project).unwrap();
    let manifest_yml = r#"schema: nenjo.dependencies.v1

dependencies:
  "@foo/app": "^1.0.0"

registries:
  - kind: local
    scope: "@foo"
    root: ../foo
    manifest_path: packages.yaml
"#;
    write_file(
        &foo,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
registries:
  - kind: local
    scope: "@bar"
    root: ../bar
    manifest_path: packages.yaml
packages:
  app: packages/app/nenjo.package.yaml
"#,
    );
    write_file(
        &foo,
        "packages/app/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: app
version: "1.0.0"
dependencies:
  "@bar/core": "^1.0.0"
"#,
    );
    write_file(
        &bar,
        "packages.yaml",
        r#"schema: nenjo.registry.v1
packages:
  core: packages/core/nenjo.package.yaml
"#,
    );
    write_file(
        &bar,
        "packages/core/nenjo.package.yaml",
        r#"schema: nenjo.package.v1
name: core
version: "1.0.0"
"#,
    );

    let manifest = DependencyManifest::parse_yaml(manifest_yml).unwrap();
    let resolved = resolve(ResolveOptions::new(&project, manifest)).unwrap();

    assert!(!project.join("nenpm.yml").exists());
    let mut packages = resolved
        .lockfile
        .packages
        .iter()
        .map(|package| format!("{}@{}", package.name, package.version))
        .collect::<Vec<_>>();
    packages.sort();
    assert_eq!(
        packages,
        vec!["@bar/core@1.0.0".to_string(), "@foo/app@1.0.0".to_string()]
    );

    write_file(&project, "nenpm.yml", manifest_yml);
    let installed = install(InstallOptions::new(&project).dry_run(true)).unwrap();
    assert_eq!(resolved.lockfile, installed.lockfile);

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
  "agent": "^0.1.0"

registries:
  - ../registry/registry.yaml

overrides:
  "agent": file:../override-packages#packages/agent/nenjo.package.yaml
  "core": file:../override-packages#packages/core/nenjo.package.yaml
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
        vec!["core@0.1.0", "agent@0.1.0"]
    );

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
fn install_uses_explicit_provider_fetch_mode() {
    let workspace = temp_workspace("provider-fetch-mode");
    let project = workspace.join("project");
    write_file(
        &project,
        "nenpm.yml",
        r#"
schema: nenjo.dependencies.v1

dependencies:
  "@acme/core": "0.1.0"

overrides:
  "@acme/core":
    kind: git
    url: https://example.com/acme/core.git
    reference: main
    manifest_path: nenjo.package.yaml
"#,
    );

    let err = format!(
        "{:?}",
        install(InstallOptions::new(&project).fetch_mode(FetchMode::Provider)).unwrap_err()
    );

    assert!(err.contains("provider fetch mode does not support git source"));
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
  - ../registry/registry.yaml
"#,
    );

    let err = install(InstallOptions::new(&project))
        .unwrap_err()
        .to_string();

    assert!(err.contains("no configured registry contains it"));
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

    assert!(err.contains("agent resolved to 0.1.0"));
    assert!(err.contains("does not satisfy ^2.0.0"));
    fs::remove_dir_all(workspace).unwrap();
}
