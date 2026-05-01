use nenjo::{ToolCategory, ToolSpec};

fn agent_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target agent."
    })
}

fn string_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": {
            "type": "string"
        }
    })
}

fn agent_update_data_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for the target agent. Omit any field you do not want to change.",
        "properties": {
            "name": {
                "type": "string",
                "description": "The agent's runtime name. Omit to leave the current name unchanged."
            },
            "description": {
                "type": ["string", "null"],
                "description": "Human-readable description of what the agent is responsible for. Set to null to clear it, or omit to leave it unchanged."
            },
            "color": {
                "type": ["string", "null"],
                "description": "Hex color used to render the agent in the dashboard. Set to null to clear it, or omit to leave it unchanged."
            },
            "model_id": {
                "type": ["string", "null"],
                "format": "uuid",
                "description": "Directly assigned model id for this agent. Use null to clear the current model assignment, or omit to leave it unchanged."
            },
            "platform_scopes": string_list_schema("Platform API scopes granted to this agent, such as `projects:read` or `agents:write`. Provide the full replacement list to change scopes, or omit to leave them unchanged.")
        },
        "additionalProperties": false
    })
}

fn prompt_config_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial prompt configuration patch for the target agent. Omit any field you do not want to change.",
        "properties": {
            "system_prompt": {
                "type": "string",
                "description": "Highest-level instruction for the agent. Defines the agent's role, boundaries, and non-negotiable behavior. Omit to leave unchanged."
            },
            "developer_prompt": {
                "type": "string",
                "description": "Secondary guidance for the agent. Used for implementation detail, workflow rules, and contextual guidance beneath the system prompt. Omit to leave unchanged."
            },
            "templates": {
                "type": "object",
                "description": "Agent template slot patch. Only provided keys are updated.",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Template used when the agent executes a normal task. Omit to leave unchanged."
                    },
                    "chat": {
                        "type": "string",
                        "description": "Template used when the agent responds in chat. Omit to leave unchanged."
                    },
                    "gate": {
                        "type": "string",
                        "description": "Template used when the agent evaluates a gate. Omit to leave unchanged."
                    },
                    "cron": {
                        "type": "string",
                        "description": "Template used when the agent is invoked by a cron schedule. Omit to leave unchanged."
                    },
                    "heartbeat": {
                        "type": "string",
                        "description": "Template used when the agent is invoked by a heartbeat schedule. Omit to leave unchanged."
                    }
                },
                "additionalProperties": true
            },
            "memory_profile": {
                "type": "object",
                "description": "Partial memory extraction and retrieval preference patch for the target agent.",
                "properties": {
                    "core_focus": {
                        "type": "array",
                        "description": "Cross-project topics this agent wants remembered as durable core knowledge. Provide the full replacement list for this field.",
                        "items": { "type": "string" }
                    },
                    "project_focus": {
                        "type": "array",
                        "description": "Project-specific topics this agent wants remembered within the active project context. Provide the full replacement list for this field.",
                        "items": { "type": "string" }
                    },
                    "shared_focus": {
                        "type": "array",
                        "description": "Topics this agent should prefer to store into shared memory for reuse by other agents. Provide the full replacement list for this field.",
                        "items": { "type": "string" }
                    }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

pub fn agent_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_agents".to_string(),
            description: "List agents so you can find an agent id before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_agent".to_string(),
            description: "Get one agent's name, description, color, model_id, domains, abilities, scopes, MCP assignments, flags, and heartbeat by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": agent_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_agent_prompt".to_string(),
            description: "Get one agent's prompt_config, including system_prompt, developer_prompt, templates, and memory_profile, by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": agent_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_agent".to_string(),
            description: "Create one agent with top-level name, description, color, model_id, and platform_scopes."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The agent's runtime name."
                    },
                    "description": agent_update_data_schema()["properties"]["description"].clone(),
                    "color": agent_update_data_schema()["properties"]["color"].clone(),
                    "model_id": agent_update_data_schema()["properties"]["model_id"].clone(),
                    "platform_scopes": agent_update_data_schema()["properties"]["platform_scopes"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_agent".to_string(),
            description: "Update one agent's top-level fields by id: name, description, color, model_id, or platform_scopes; use update_agent_prompt for prompt_config. Valid platform scope strings are agents:read, agents:write, abilities:read, abilities:write, domains:read, domains:write, projects:read, projects:write, routines:read, routines:write, models:read, models:write, councils:read, councils:write, context_blocks:read, context_blocks:write, mcp_servers:read, mcp_servers:write, chat:read, and chat:write."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": agent_id_schema(),
                    "name": agent_update_data_schema()["properties"]["name"].clone(),
                    "description": agent_update_data_schema()["properties"]["description"].clone(),
                    "color": agent_update_data_schema()["properties"]["color"].clone(),
                    "model_id": agent_update_data_schema()["properties"]["model_id"].clone(),
                    "platform_scopes": agent_update_data_schema()["properties"]["platform_scopes"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_agent_prompt".to_string(),
            description: "Update one agent's prompt_config by id using prompt_config.system_prompt, prompt_config.developer_prompt, prompt_config.templates, or prompt_config.memory_profile."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": agent_id_schema(),
                    "prompt_config": prompt_config_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_agent".to_string(),
            description: "Delete one agent by id when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": agent_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
