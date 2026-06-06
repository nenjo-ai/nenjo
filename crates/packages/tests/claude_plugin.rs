use std::path::{Path, PathBuf};

use nenjo_packages::{
    PackageKind, claude_plugin_resources, parse_claude_plugin_command, parse_claude_plugin_hooks,
    parse_claude_plugin_manifest, parse_claude_plugin_skill,
};
use serde_json::json;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude_plugin")
        .join(name)
}

fn read_fixture(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap()
}

#[test]
fn adapts_claude_plugin_fixture_to_native_resources() {
    let root = fixture("ralph-loop");
    let plugin =
        parse_claude_plugin_manifest(&read_fixture(&root, ".claude-plugin/plugin.json")).unwrap();
    let command = parse_claude_plugin_command(
        &read_fixture(&root, "commands/ralph-loop.md"),
        "commands/ralph-loop.md",
    )
    .unwrap();
    let skill = parse_claude_plugin_skill(
        &read_fixture(&root, "skills/ralph-loop/SKILL.md"),
        "skills/ralph-loop/SKILL.md",
    )
    .unwrap();
    let hooks = parse_claude_plugin_hooks(&read_fixture(&root, "hooks/hooks.json")).unwrap();

    let resources = claude_plugin_resources(
        &plugin,
        std::slice::from_ref(&skill),
        std::slice::from_ref(&command),
        &hooks,
        &[],
        &[],
        ".",
    )
    .unwrap();

    assert_eq!(plugin.slug, "ralph_loop");
    assert_eq!(resources.len(), 4);
    assert!(
        resources
            .iter()
            .any(|resource| resource.kind == PackageKind::Plugin)
    );

    let command_resource = resources
        .iter()
        .find(|resource| resource.kind == PackageKind::Command)
        .unwrap();
    assert_eq!(
        command_resource.manifest.manifest["name"],
        "ralph_loop__ralph_loop"
    );
    assert_eq!(command_resource.manifest.manifest["command"], "/ralph-loop");
    assert_eq!(
        command_resource.manifest.manifest["hooks"],
        json!(["ralph_loop__stop_ralph_loop_stop"])
    );

    let skill_resource = resources
        .iter()
        .find(|resource| resource.kind == PackageKind::Skill)
        .unwrap();
    assert_eq!(skill.hooks, vec!["Stop ralph-loop-stop"]);
    assert_eq!(
        skill_resource.manifest.manifest["name"],
        "ralph_loop__ralph_loop"
    );
    assert_eq!(
        skill_resource.manifest.manifest["hooks"],
        json!(["ralph_loop__stop_ralph_loop_stop"])
    );

    let hook_resource = resources
        .iter()
        .find(|resource| resource.kind == PackageKind::Hook)
        .unwrap();
    assert_eq!(hooks[0].slug, "stop_ralph_loop_stop");
    assert_eq!(hook_resource.manifest.manifest["event"], "Stop");
    assert_eq!(
        hook_resource.manifest.manifest["command"]["path"],
        "scripts/ralph-loop-stop.sh"
    );
}
