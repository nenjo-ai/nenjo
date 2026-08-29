use nenjo::{ToolCategory, ToolSpec};

fn domain_ref_schema() -> serde_json::Value {
    slug_schema("Existing domain slug. Use `slug` from list_domains or get_domain.")
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

fn string_list_schema(description: &str) -> serde_json::Value {
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
        "description": "Domain prompt configuration. Omit to leave unchanged on update.",
        "properties": {
            "developer_prompt_addon": {
                "type": ["string", "null"],
                "description": "Developer prompt addon applied while the domain is active. Set null to clear."
            }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for domain resources.
pub fn domain_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_domains".to_string(),
            description: "List visible domains as prompt-free summaries. Use a returned `slug` as the `domain` value in get_domain or configure_domain. This does not include prompt_config; call get_domain for the full domain document."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_domain".to_string(),
            description: "Get one domain's full DomainDocument by slug, including prompt_config, command, platform_scopes, abilities, and tool assignments."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["domain"],
                "properties": {
                    "domain": domain_ref_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "configure_domain".to_string(),
            description: "Create or update one domain atomically by required stable slug. If the slug does not exist, `name` and `command` are required. Omitted fields are unchanged; set description to null to clear it. Assignment arrays are full replacements when present; pass an empty array to clear that assignment type. Returns the same canonical `domain: DomainDocument` as get_domain, not a patch echo."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["slug"],
                "properties": {
                    "slug": slug_schema("Required stable domain slug. Configure never renames this slug."),
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Domain runtime/display name. Required when the slug does not exist; omit on update to leave unchanged."
                    },
                    "path": {
                        "type": "string",
                        "description": "Folder path for this domain. Omit to leave unchanged."
                    },
                    "description": {
                        "type": ["string", "null"],
                        "description": "Human-readable description. Omit to leave unchanged; set null to clear."
                    },
                    "command": {
                        "type": "string",
                        "description": "Slash/hash-style command used to activate this domain. Required when the slug does not exist; omit on update to leave unchanged."
                    },
                    "prompt_config": prompt_config_schema(),
                    "abilities": string_list_schema("Full replacement list of ability slugs activated by this domain. Omit to leave unchanged."),
                    "mcp_servers": string_list_schema("Full replacement list of MCP server slugs activated by this domain. Omit to leave unchanged.")
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
    fn configure_domain_exposes_assignment_replacements() {
        let tools = domain_tools();
        let configure_domain = tools
            .iter()
            .find(|tool| tool.name == "configure_domain")
            .expect("configure_domain tool should exist");

        assert_eq!(
            configure_domain.parameters["properties"]["abilities"]["description"],
            serde_json::json!(
                "Full replacement list of ability slugs activated by this domain. Omit to leave unchanged."
            )
        );
        assert!(
            configure_domain
                .description
                .contains("Assignment arrays are full replacements")
        );
        assert_eq!(
            configure_domain.parameters["required"],
            serde_json::json!(["slug"])
        );
        assert!(
            configure_domain.parameters["properties"]
                .get("script_tools")
                .is_none()
        );
    }
}
