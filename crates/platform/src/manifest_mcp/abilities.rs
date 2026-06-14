use nenjo::{ToolCategory, ToolSpec};

fn ability_ref_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Existing ability slug. Use `name` from list_abilities or get_ability. For configure_ability, omit `ability` to create a new ability."
    })
}

fn slug_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": { "type": "string" }
    })
}

fn prompt_config_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Ability prompt configuration. Omit to leave unchanged on update.",
        "properties": {
            "developer_prompt": {
                "type": "string",
                "description": "Developer prompt applied while the ability sub-execution runs."
            }
        },
        "additionalProperties": false
    })
}

fn configure_metadata_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Ability metadata patch. Required on create because metadata.name is required when ability is omitted. On update, omitted fields are unchanged.",
        "properties": {
            "name": {
                "type": "string",
                "description": "Ability runtime/display name. Required when creating a new ability."
            },
            "path": {
                "type": "string",
                "description": "Folder path for this ability. Omit to leave unchanged."
            },
            "description": {
                "type": ["string", "null"],
                "description": "Human-readable description. Omit to leave unchanged; set null to clear."
            },
            "activation_condition": {
                "type": "string",
                "description": "Condition text that tells an agent when this ability should be invoked."
            }
        },
        "additionalProperties": false
    })
}

fn configure_assignments_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Full replacement assignment lists. Omit a field to leave that assignment type unchanged; pass an empty array to clear it.",
        "properties": {
            "mcp_servers": slug_list_schema("Full replacement list of MCP server slugs available while this ability runs."),
            "script_tools": slug_list_schema("Full replacement list of native script tool slugs available while this ability runs.")
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for ability resources.
pub fn ability_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_abilities".to_string(),
            description: "List visible abilities as prompt-free summaries. Use a returned `name` as the `ability` value in get_ability or configure_ability. This does not include prompt_config; call get_ability for the full ability document."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_ability".to_string(),
            description: "Get one ability's full AbilityDocument by slug, including prompt_config, activation_condition, platform_scopes, and tool assignments."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["ability"],
                "properties": {
                    "ability": ability_ref_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "configure_ability".to_string(),
            description: "Create or update one ability in a single backend-owned sequence. Omit `ability` to create; include `ability` to update by slug. On create, metadata.name and prompt_config.developer_prompt are required. Omitted fields are unchanged on update. assignment arrays are full replacements when present; pass an empty array to clear that assignment type. Returns `ability: AbilityDocument`."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "ability": ability_ref_schema(),
                    "metadata": configure_metadata_schema(),
                    "prompt_config": prompt_config_schema(),
                    "assignments": configure_assignments_schema()
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
    fn configure_ability_exposes_assignment_replacements() {
        let tools = ability_tools();
        let configure_ability = tools
            .iter()
            .find(|tool| tool.name == "configure_ability")
            .expect("configure_ability tool should exist");

        assert_eq!(
            configure_ability.parameters["properties"]["assignments"]["properties"]["mcp_servers"]
                ["description"],
            serde_json::json!(
                "Full replacement list of MCP server slugs available while this ability runs."
            )
        );
        assert!(
            configure_ability
                .description
                .contains("assignment arrays are full replacements")
        );
    }
}
