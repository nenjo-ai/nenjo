use nenjo::{ToolCategory, ToolSpec};

fn ability_ref_schema() -> serde_json::Value {
    slug_schema("Existing ability slug. Use `slug` from list_abilities or get_ability.")
}

fn slug_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 255,
        "pattern": "^[a-z0-9](?:[a-z0-9_-]{0,253}[a-z0-9])?$",
        "description": description
    })
}

fn slug_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": slug_schema("Assigned resource slug."),
        "uniqueItems": true
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

/// Return manifest MCP tool definitions for ability resources.
pub fn ability_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_abilities".to_string(),
            description: "List visible abilities as prompt-free summaries. Use a returned `slug` as the `ability` value in get_ability or configure_ability. This does not include prompt_config; call get_ability for the full ability document."
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
            description: "Create or update one ability atomically by required stable slug. If the slug does not exist, `name` and prompt_config.developer_prompt are required. Omitted fields are unchanged; set description to null to clear it. mcp_servers is a full replacement when present; pass an empty array to clear it. Returns the same canonical `ability: AbilityDocument` as get_ability, not a patch echo."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["slug"],
                "properties": {
                    "slug": slug_schema("Required stable ability slug. Configure never renames this slug."),
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Ability runtime/display name. Required when the slug does not exist; omit on update to leave unchanged."
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
                        "description": "Condition text that tells an agent when this ability should be invoked. Omit to leave unchanged."
                    },
                    "prompt_config": prompt_config_schema(),
                    "mcp_servers": slug_list_schema("Full replacement list of MCP server slugs available while this ability runs. Omit to leave unchanged.")
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
            configure_ability.parameters["properties"]["mcp_servers"]["description"],
            serde_json::json!(
                "Full replacement list of MCP server slugs available while this ability runs. Omit to leave unchanged."
            )
        );
        assert!(
            configure_ability
                .description
                .contains("mcp_servers is a full replacement")
        );
        assert_eq!(
            configure_ability.parameters["required"],
            serde_json::json!(["slug"])
        );
        assert!(
            configure_ability.parameters["properties"]
                .get("script_tools")
                .is_none()
        );
    }
}
