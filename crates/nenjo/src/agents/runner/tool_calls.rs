//! Tool-call lookup and boundary normalization.

use std::sync::Arc;

use crate::tools::Tool;

pub(super) fn tool_for_call<'a>(
    tools: &'a [Arc<dyn Tool>],
    tool_call: &nenjo_models::ToolCall,
) -> Option<&'a Arc<dyn Tool>> {
    tools.iter().find(|tool| {
        let name = tool.name();
        name == tool_call.name
            || nenjo_models::sanitize_tool_name(name) == tool_call.name
            || nenjo_models::sanitize_tool_name_lenient(name) == tool_call.name
    })
}

/// Repair JSON-encoded object and array values produced by tool-call parsers.
///
/// Some OpenAI-compatible model stacks return a syntactically valid outer
/// argument object while encoding nested structured fields as JSON strings.
/// Decode only when the tool's JSON Schema requires an object or array and
/// does not also allow a string, so legitimate string arguments are preserved.
fn normalize_json_encoded_structures(
    value: &mut serde_json::Value,
    schema: &serde_json::Value,
) -> usize {
    let schema_allows = |expected: &str| match schema.get("type") {
        Some(serde_json::Value::String(kind)) => kind == expected,
        Some(serde_json::Value::Array(kinds)) => kinds.iter().any(|kind| kind == expected),
        _ => false,
    };

    let allows_object = schema_allows("object");
    let allows_array = schema_allows("array");
    let allows_string = schema_allows("string");
    let mut normalized = 0;

    if !allows_string
        && (allows_object || allows_array)
        && let serde_json::Value::String(encoded) = value
        && let Ok(decoded) = serde_json::from_str::<serde_json::Value>(encoded)
        && ((allows_object && decoded.is_object()) || (allows_array && decoded.is_array()))
    {
        *value = decoded;
        normalized += 1;
    }

    match value {
        serde_json::Value::Object(object) if allows_object => {
            let properties = schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            let additional = schema
                .get("additionalProperties")
                .filter(|additional| additional.is_object());
            for (name, child) in object {
                let child_schema = properties
                    .and_then(|properties| properties.get(name))
                    .or(additional);
                if let Some(child_schema) = child_schema {
                    normalized += normalize_json_encoded_structures(child, child_schema);
                }
            }
        }
        serde_json::Value::Array(items) if allows_array => {
            if let Some(item_schema) = schema.get("items") {
                for item in items {
                    normalized += normalize_json_encoded_structures(item, item_schema);
                }
            }
        }
        _ => {}
    }

    normalized
}

pub(super) fn normalize_tool_call_arguments(
    tools: &[Arc<dyn Tool>],
    tool_call: &mut nenjo_models::ToolCall,
) -> usize {
    let Some(tool) = tool_for_call(tools, tool_call) else {
        return 0;
    };
    let Ok(mut arguments) = serde_json::from_str::<serde_json::Value>(&tool_call.arguments) else {
        return 0;
    };

    let normalized = normalize_json_encoded_structures(&mut arguments, &tool.parameters_schema());
    if normalized > 0 {
        tool_call.arguments = arguments.to_string();
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_json_encoded_values_required_to_be_structured() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "graph": {
                    "type": "object",
                    "properties": {
                        "entry_steps": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                }
            }
        });
        let mut arguments = serde_json::json!({
            "graph": "{\"entry_steps\":\"[\\\"implement\\\"]\"}"
        });

        let normalized = normalize_json_encoded_structures(&mut arguments, &schema);

        assert_eq!(normalized, 2);
        assert_eq!(
            arguments,
            serde_json::json!({ "graph": { "entry_steps": ["implement"] } })
        );
    }

    #[test]
    fn preserves_json_text_when_schema_allows_strings() {
        let schema = serde_json::json!({ "type": ["object", "string"] });
        let mut value = serde_json::json!("{\"mode\":\"literal\"}");

        let normalized = normalize_json_encoded_structures(&mut value, &schema);

        assert_eq!(normalized, 0);
        assert_eq!(value, serde_json::json!("{\"mode\":\"literal\"}"));
    }

    #[test]
    fn preserves_encoded_value_with_the_wrong_structured_type() {
        let schema = serde_json::json!({ "type": "object" });
        let mut value = serde_json::json!("[1,2,3]");

        let normalized = normalize_json_encoded_structures(&mut value, &schema);

        assert_eq!(normalized, 0);
        assert_eq!(value, serde_json::json!("[1,2,3]"));
    }
}
