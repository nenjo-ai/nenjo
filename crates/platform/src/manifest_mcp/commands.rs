use nenjo::{ToolCategory, ToolSpec};

fn command_ref_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Existing command name or slash command, such as `deploy` or `/deploy`. Omit to create a new command."
    })
}

fn command_lookup_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Existing command name or slash command, such as `design` or `/design`."
    })
}

fn configure_metadata_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Optional stable internal command name. Omit on create to derive it from the slash command."
            },
            "path": {
                "type": "string",
                "description": "Optional folder path, using lowercase slash-separated segments."
            },
            "command": {
                "type": "string",
                "description": "Slash command trigger, such as `/deploy`. Required when creating a command."
            },
            "description": {
                "type": ["string", "null"],
                "description": "Optional command description. Pass null to clear."
            }
        },
        "additionalProperties": false
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
            description: "Create or update one slash command in a single backend-owned sequence. Omit `command_ref` to create; include `command_ref` to update by name or slash command. On create, metadata.command and content are required; metadata.name is optional and derived from the slash command when omitted. Command content is encrypted before it is sent to the platform. Returns `command: CommandManifest`."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command_ref": command_ref_schema(),
                    "metadata": configure_metadata_schema(),
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
