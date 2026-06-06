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

pub fn command_arguments<'a>(requested_command: &str, user_content: &'a str) -> &'a str {
    let trimmed = user_content.trim();
    let command = requested_command.trim();
    let Some(rest) = trimmed.strip_prefix(command) else {
        return trimmed;
    };
    match rest.chars().next() {
        None => "",
        Some(ch) if ch.is_whitespace() => rest.trim(),
        Some(_) => trimmed,
    }
}

pub fn render_command_invocation(
    command: &CommandManifest,
    requested_command: &str,
    user_content: &str,
    loaded: &LoadedCommand,
) -> String {
    let arguments = command_arguments(requested_command, user_content);
    let display_name = command
        .display_name
        .as_deref()
        .unwrap_or(command.name.as_str());

    format!(
        "Installed slash command invocation\n\
         \n\
         Follow the installed command markdown below using the user's arguments.\n\
         \n\
         Command: {command_name}\n\
         Display name: {display_name}\n\
         Source file: {entry_file}\n\
         Command directory: {command_dir}\n\
         Plugin directory: {plugin_root}\n\
         \n\
         User message:\n\
         {user_content}\n\
         \n\
         Arguments:\n\
         {arguments}\n\
         \n\
         Runtime path rules:\n\
         - Resolve relative paths in the command markdown from the command directory above.\n\
         - Treat CLAUDE_PLUGIN_ROOT and CLAUDE_PLUGIN_DIR as the plugin directory above for Claude plugin compatibility.\n\
         - Use absolute paths when invoking referenced files or scripts.\n\
         \n\
         BEGIN COMMAND MARKDOWN\n\
         {command_markdown}\n\
         END COMMAND MARKDOWN",
        command_name = command.command,
        display_name = display_name,
        entry_file = loaded.source_file,
        command_dir = loaded.command_dir,
        plugin_root = loaded.plugin_root,
        user_content = user_content.trim(),
        arguments = arguments,
        command_markdown = loaded.markdown.trim(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(name: &str, slash: &str) -> CommandManifest {
        serde_json::from_value(json!({
            "id": uuid::Uuid::nil(),
            "name": name,
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

    #[test]
    fn command_arguments_strip_requested_command() {
        assert_eq!(
            command_arguments("/ralph-loop", "/ralph-loop do thing"),
            "do thing"
        );
        assert_eq!(command_arguments("/ralph-loop", "do thing"), "do thing");
    }
}
