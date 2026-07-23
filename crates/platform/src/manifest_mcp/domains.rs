use nenjo::{ToolCategory, ToolSpec};

fn domain_ref_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Existing domain slug. Use `slug` from list_domains or get_domain. For configure_domain, omit `domain` to create a new domain."
    })
}

fn string_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": { "type": "string" }
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

fn configure_metadata_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Domain metadata patch. metadata.slug, metadata.name, and metadata.command are required when creating; omitted fields are unchanged on update.",
        "properties": {
            "slug": {
                "type": "string",
                "description": "Stable domain slug. Required when creating a new domain."
            },
            "name": {
                "type": "string",
                "description": "Domain runtime/display name. Required when creating a new domain."
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
                "description": "Slash/hash-style command used to activate this domain, such as `#creator`. Required when creating a new domain."
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
            "abilities": string_list_schema("Full replacement list of ability names/slugs activated by this domain."),
            "mcp_servers": string_list_schema("Full replacement list of MCP server slugs activated by this domain."),
            "script_tools": string_list_schema("Full replacement list of native script tool slugs activated by this domain.")
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
            description: "Create or update one domain in a single backend-owned sequence. Omit `domain` to create; include `domain` to update by slug. On create, metadata.slug, metadata.name, and metadata.command are required. Omitted fields are unchanged on update. assignment arrays are full replacements when present; pass an empty array to clear that assignment type. Returns `domain: DomainDocument`."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "domain": domain_ref_schema(),
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
    fn configure_domain_exposes_assignment_replacements() {
        let tools = domain_tools();
        let configure_domain = tools
            .iter()
            .find(|tool| tool.name == "configure_domain")
            .expect("configure_domain tool should exist");

        assert_eq!(
            configure_domain.parameters["properties"]["assignments"]["properties"]["abilities"]["description"],
            serde_json::json!(
                "Full replacement list of ability names/slugs activated by this domain."
            )
        );
        assert!(
            configure_domain
                .description
                .contains("assignment arrays are full replacements")
        );
    }
}
