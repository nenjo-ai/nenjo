mod support;

use std::fs;

use nenjo_nenpm::{
    InstallOptions, NenpmLock, PackageInstallIndex, PackageSource, install, package_install_path,
    package_instance_key,
};

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
    assert!(
        lock.packages[1]
            .modules
            .iter()
            .any(|module| module.path == "agents/support.yaml"
                && module.resource.as_deref() == Some("support_agent")
                && module.imports.len() == 1)
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
                && module.name == "acme_tools__review")
    );
    assert!(package.modules.iter().any(|module| module.kind
        == nenjo_packages::PackageKind::McpServer
        && module.name == "acme_tools__review_server"));

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
                && module.name == "support_agent")
    );
    assert!(
        agent_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Ability
                && module.name == "design_agent")
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
                && module.name == "acme_tools")
    );
    assert!(
        plugin_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Skill
                && module.name == "acme_tools__review")
    );
    assert!(
        plugin_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Command
                && module.name == "acme_tools__audit")
    );
    assert!(
        plugin_package
            .modules
            .iter()
            .any(|module| module.kind == nenjo_packages::PackageKind::Hook
                && module.name == "acme_tools__stop_audit_stop")
    );
    assert!(plugin_package.modules.iter().any(|module| module.kind
        == nenjo_packages::PackageKind::McpServer
        && module.name == "acme_tools__review_server"));

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
            .any(|module| module.name == "support_agent" && module.imports.is_empty())
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
