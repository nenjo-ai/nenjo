//! Filesystem-agnostic slash command contracts and rendering helpers.

use anyhow::Result;
use async_trait::async_trait;

use crate::manifest::CommandManifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCommand {
    pub markdown: String,
    pub source_file: String,
    pub command_dir: String,
    pub plugin_root: String,
}

#[async_trait]
pub trait CommandProvider: Send + Sync {
    fn list_commands(&self) -> Vec<CommandManifest>;

    fn resolve_command(&self, requested: &str) -> Option<CommandManifest> {
        find_command_manifest(&self.list_commands(), requested).cloned()
    }

    fn resolve_invoked_command(&self, content: &str) -> Option<CommandManifest> {
        find_invoked_command_manifest(&self.list_commands(), content).cloned()
    }

    async fn load_command(&self, command: &CommandManifest) -> Result<LoadedCommand>;
}

pub fn find_command_manifest<'a>(
    commands: &'a [CommandManifest],
    requested: &str,
) -> Option<&'a CommandManifest> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    let requested_name = requested.trim_start_matches('/');
    commands.iter().find(|command| {
        command.command.trim() == requested
            || command.command.trim().trim_start_matches('/') == requested_name
            || command.name == requested
            || command.name == requested_name
    })
}

pub fn find_invoked_command_manifest<'a>(
    commands: &'a [CommandManifest],
    content: &str,
) -> Option<&'a CommandManifest> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    commands
        .iter()
        .filter(|command| content_invokes_command(trimmed, command.command.trim()))
        .max_by_key(|command| command.command.len())
}

pub fn content_invokes_command(content: &str, command: &str) -> bool {
    if command.is_empty() {
        return false;
    }
    let Some(rest) = content.strip_prefix(command) else {
        return false;
    };
    match rest.chars().next() {
        None => true,
        Some(ch) => ch.is_whitespace(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(name: &str, slash: &str) -> CommandManifest {
        serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "name": name,
            "path": "",
            "command": slash,
            "entry_path": "command.md",
            "root_dir": "/virtual/commands"
        }))
        .unwrap()
    }

    #[test]
    fn command_selector_matches_name_and_slash() {
        let commands = vec![command("ralph-loop", "/ralph-loop")];

        assert!(find_command_manifest(&commands, "/ralph-loop").is_some());
        assert!(find_command_manifest(&commands, "ralph-loop").is_some());
    }

    #[test]
    fn invoked_command_uses_longest_prefix() {
        let commands = vec![command("help", "/help"), command("help-me", "/help me")];

        let matched = find_invoked_command_manifest(&commands, "/help me now").unwrap();

        assert_eq!(matched.name, "help-me");
    }
}
