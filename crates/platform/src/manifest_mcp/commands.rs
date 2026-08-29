use nenjo::{ToolCategory, ToolSpec};

fn slug_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 255,
        "pattern": "^[a-z0-9](?:[a-z0-9_-]{0,253}[a-z0-9])?$",
        "description": description
    })
}

fn command_lookup_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Existing command name or slash command, such as `design` or `/design`."
    })
}

/// Return manifest MCP tool definitions for slash commands.
pub fn command_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_commands".to_string(),
            description: "List visible slash commands as content-free summaries. Use a returned `name` or `command` value with get_command or configure_command. This does not include command content; call get_command for the full CommandManifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_command".to_string(),
            description: "Get one slash command's full CommandManifest by name or slash command, including content, path grouping, hooks, source_type, and metadata."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": command_lookup_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "configure_command".to_string(),
            description: "Create or update one slash command atomically by required stable slug. If the slug does not exist, `name`, `command`, and `content` are required. Omitted fields are unchanged; set description to null to clear it. Returns the same canonical `command: CommandManifest` as get_command, not a patch echo."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["slug"],
                "properties": {
                    "slug": slug_schema("Required stable command slug. Configure never renames this slug."),
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Command display name. Required when the slug does not exist; omit on update to leave unchanged."
                    },
                    "path": {
                        "type": "string",
                        "description": "Folder path using lowercase slash-separated segments. Omit to leave unchanged."
                    },
                    "command": {
                        "type": "string",
                        "description": "Slash command trigger, such as `/deploy`. Required when the slug does not exist; omit on update to leave unchanged."
                    },
                    "description": {
                        "type": ["string", "null"],
                        "description": "Human-readable description. Omit to leave unchanged; set null to clear."
                    },
                    "content": {
                        "type": "string",
                        "description": "Markdown body for the command. Omit to leave unchanged on update."
                    }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_command_requires_stable_slug_and_is_flat() {
        let configure = command_tools()
            .into_iter()
            .find(|tool| tool.name == "configure_command")
            .expect("configure_command tool should exist");

        assert_eq!(
            configure.parameters["required"],
            serde_json::json!(["slug"])
        );
        assert!(
            configure.parameters["properties"]
                .get("command_ref")
                .is_none()
        );
        assert!(configure.parameters["properties"].get("metadata").is_none());
    }
}
