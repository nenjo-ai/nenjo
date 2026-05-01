use nenjo::{ToolCategory, ToolSpec};

fn model_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target model."
    })
}

fn string_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": { "type": "string" }
    })
}

fn model_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "model"],
        "properties": {
            "name": { "type": "string", "description": "Display name for this model config." },
            "description": { "type": ["string", "null"], "description": "Optional model description." },
            "model": { "type": "string", "description": "Provider model identifier, such as `gpt-4o`." },
            "model_provider": { "type": "string", "description": "Provider name, such as `openai` or `openrouter`." },
            "temperature": { "type": "number", "description": "Sampling temperature between 0.0 and 2.0." },
            "tags": string_list_schema("Optional tags for grouping models."),
            "base_url": { "type": ["string", "null"], "description": "Optional provider base URL override." }
        },
        "additionalProperties": false
    })
}

fn model_update_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for an existing model. Omit fields you do not want to change.",
        "properties": {
            "name": { "type": "string", "description": "Replace the display name." },
            "description": { "type": ["string", "null"], "description": "Update or clear the description. Omit to leave unchanged." },
            "model": { "type": "string", "description": "Replace the provider model identifier." },
            "model_provider": { "type": "string", "description": "Replace the provider name." },
            "temperature": { "type": "number", "description": "Replace the temperature." },
            "tags": string_list_schema("Full replacement tag list."),
            "base_url": { "type": ["string", "null"], "description": "Update or clear the base URL. Omit to leave unchanged." }
        },
        "additionalProperties": false
    })
}

pub fn model_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_models".to_string(),
            description: "List models so you can find a model id before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_model".to_string(),
            description: "Get one model's name, description, model identifier, provider, temperature, tags, and base_url by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": model_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_model".to_string(),
            description: "Create one model with top-level name, model, and optional description, model_provider, temperature, tags, or base_url."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name", "model"],
                "properties": model_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_model".to_string(),
            description: "Update one model's name, description, model, model_provider, temperature, tags, or base_url by id using top-level fields."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": model_id_schema(),
                    "name": model_update_schema()["properties"]["name"].clone(),
                    "description": model_update_schema()["properties"]["description"].clone(),
                    "model": model_update_schema()["properties"]["model"].clone(),
                    "model_provider": model_update_schema()["properties"]["model_provider"].clone(),
                    "temperature": model_update_schema()["properties"]["temperature"].clone(),
                    "tags": model_update_schema()["properties"]["tags"].clone(),
                    "base_url": model_update_schema()["properties"]["base_url"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_model".to_string(),
            description: "Delete one model by id when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": model_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
