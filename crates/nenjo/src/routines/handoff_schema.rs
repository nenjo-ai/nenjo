use std::collections::HashSet;

use anyhow::{Result, bail};
use serde_json::Value;

pub const HANDOFF_SCHEMA_METADATA_KEY: &str = "handoff_schema";

const SUPPORTED_SCHEMA_KEYS: &[&str] = &[
    "$schema",
    "additionalProperties",
    "const",
    "description",
    "enum",
    "items",
    "maxItems",
    "minItems",
    "minLength",
    "properties",
    "required",
    "title",
    "type",
];

const SUPPORTED_TYPES: &[&str] = &[
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// Validate the canonical handoff schema stored on a routine edge.
///
/// This intentionally supports a small, enforceable JSON Schema subset. If a
/// schema uses a keyword outside this subset, validation fails instead of
/// silently accepting a contract the runtime cannot enforce.
pub fn validate_handoff_schema(schema: &Value) -> Result<()> {
    validate_schema_at(schema, HANDOFF_SCHEMA_METADATA_KEY, true)
}

pub(crate) fn edge_handoff_schema(metadata: &Value) -> Result<&Value> {
    let Some(schema) = metadata.get(HANDOFF_SCHEMA_METADATA_KEY) else {
        bail!("metadata.{HANDOFF_SCHEMA_METADATA_KEY} is required");
    };
    validate_handoff_schema(schema)?;
    Ok(schema)
}

pub(crate) fn validate_handoff_payload(schema: &Value, payload: &Value) -> Result<()> {
    validate_value_at(schema, payload, "handoff")
}

pub(crate) fn compact_schema(schema: &Value) -> String {
    serde_json::to_string(schema).unwrap_or_else(|_| schema.to_string())
}

fn validate_schema_at(schema: &Value, path: &str, root: bool) -> Result<()> {
    let Some(object) = schema.as_object() else {
        bail!("{path} must be a JSON schema object");
    };

    for key in object.keys() {
        if !SUPPORTED_SCHEMA_KEYS.contains(&key.as_str()) {
            bail!("{path}.{key} is not supported by the runtime handoff schema validator");
        }
    }

    let Some(schema_type) = object.get("type").and_then(Value::as_str) else {
        bail!("{path}.type is required");
    };
    if !SUPPORTED_TYPES.contains(&schema_type) {
        bail!("{path}.type '{schema_type}' is not supported");
    }
    if root && schema_type != "object" {
        bail!("metadata.{HANDOFF_SCHEMA_METADATA_KEY}.type must be object");
    }

    validate_enum(schema, path)?;
    validate_const(schema, path)?;

    match schema_type {
        "object" => validate_object_schema(object, path)?,
        "array" => validate_array_schema(object, path)?,
        "string" => validate_non_negative_integer(object, path, "minLength")?,
        _ => {}
    }

    Ok(())
}

fn validate_object_schema(object: &serde_json::Map<String, Value>, path: &str) -> Result<()> {
    let properties = match object.get("properties") {
        Some(value) => Some(
            value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("{path}.properties must be an object"))?,
        ),
        None => None,
    };

    if let Some(properties) = properties {
        for (field, schema) in properties {
            validate_schema_at(schema, &format!("{path}.properties.{field}"), false)?;
        }
    }

    let required = required_fields(object, path)?;
    if !required.is_empty() {
        let Some(properties) = properties else {
            bail!("{path}.required cannot be used without properties");
        };
        for field in &required {
            if !properties.contains_key(field) {
                bail!("{path}.required field '{field}' must be defined in properties");
            }
        }
    }

    if let Some(value) = object.get("additionalProperties")
        && !value.is_boolean()
    {
        bail!("{path}.additionalProperties must be a boolean");
    }

    Ok(())
}

fn validate_array_schema(object: &serde_json::Map<String, Value>, path: &str) -> Result<()> {
    validate_non_negative_integer(object, path, "minItems")?;
    validate_non_negative_integer(object, path, "maxItems")?;

    if let (Some(min), Some(max)) = (
        object.get("minItems").and_then(Value::as_u64),
        object.get("maxItems").and_then(Value::as_u64),
    ) && min > max
    {
        bail!("{path}.minItems must be less than or equal to maxItems");
    }

    let Some(items) = object.get("items") else {
        bail!("{path}.items is required for array schemas");
    };
    validate_schema_at(items, &format!("{path}.items"), false)
}

fn validate_non_negative_integer(
    object: &serde_json::Map<String, Value>,
    path: &str,
    key: &str,
) -> Result<()> {
    if let Some(value) = object.get(key)
        && value.as_u64().is_none()
    {
        bail!("{path}.{key} must be a non-negative integer");
    }
    Ok(())
}

fn validate_enum(schema: &Value, path: &str) -> Result<()> {
    let Some(values) = schema.get("enum") else {
        return Ok(());
    };
    let Some(values) = values.as_array() else {
        bail!("{path}.enum must be an array");
    };
    if values.is_empty() {
        bail!("{path}.enum must contain at least one value");
    }
    Ok(())
}

fn validate_const(_schema: &Value, _path: &str) -> Result<()> {
    Ok(())
}

fn required_fields(object: &serde_json::Map<String, Value>, path: &str) -> Result<HashSet<String>> {
    let Some(required) = object.get("required") else {
        return Ok(HashSet::new());
    };
    let Some(values) = required.as_array() else {
        bail!("{path}.required must be an array");
    };
    let mut fields = HashSet::new();
    for value in values {
        let Some(field) = value.as_str() else {
            bail!("{path}.required entries must be strings");
        };
        if field.trim().is_empty() {
            bail!("{path}.required entries must not be empty");
        }
        if !fields.insert(field.to_string()) {
            bail!("{path}.required contains duplicate field '{field}'");
        }
    }
    Ok(fields)
}

fn validate_value_at(schema: &Value, value: &Value, path: &str) -> Result<()> {
    let object = schema
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{path} schema must be an object"))?;
    let schema_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{path} schema is missing type"))?;

    validate_value_type(schema_type, value, path)?;

    if let Some(expected) = object.get("const")
        && value != expected
    {
        bail!("{path} must equal {}", compact_schema(expected));
    }
    if let Some(enum_values) = object.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|allowed| allowed == value)
    {
        bail!(
            "{path} must be one of {}",
            compact_schema(&Value::Array(enum_values.clone()))
        );
    }

    match schema_type {
        "object" => validate_object_value(object, value, path),
        "array" => validate_array_value(object, value, path),
        "string" => validate_string_value(object, value, path),
        _ => Ok(()),
    }
}

fn validate_value_type(schema_type: &str, value: &Value, path: &str) -> Result<()> {
    let valid = match schema_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    };

    if valid {
        Ok(())
    } else {
        bail!("{path} must be {schema_type}")
    }
}

fn validate_object_value(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
) -> Result<()> {
    let Some(value_object) = value.as_object() else {
        bail!("{path} must be object");
    };
    let properties = schema.get("properties").and_then(Value::as_object);

    for required in required_fields(schema, path)? {
        if !value_object.contains_key(&required) {
            bail!("{}.{} is required", path, required);
        }
    }

    if let Some(false) = schema.get("additionalProperties").and_then(Value::as_bool) {
        let allowed = properties
            .map(|properties| properties.keys().cloned().collect::<HashSet<_>>())
            .unwrap_or_default();
        for key in value_object.keys() {
            if !allowed.contains(key) {
                bail!("{}.{} is not allowed by schema", path, key);
            }
        }
    }

    if let Some(properties) = properties {
        for (key, child_schema) in properties {
            if let Some(child_value) = value_object.get(key) {
                validate_value_at(child_schema, child_value, &format!("{}.{}", path, key))?;
            }
        }
    }

    Ok(())
}

fn validate_array_value(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
) -> Result<()> {
    let Some(values) = value.as_array() else {
        bail!("{path} must be array");
    };
    if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
        && values.len() < min as usize
    {
        bail!("{path} must contain at least {min} item(s)");
    }
    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
        && values.len() > max as usize
    {
        bail!("{path} must contain at most {max} item(s)");
    }
    if let Some(items_schema) = schema.get("items") {
        for (index, item) in values.iter().enumerate() {
            validate_value_at(items_schema, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_string_value(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
) -> Result<()> {
    let Some(value) = value.as_str() else {
        bail!("{path} must be string");
    };
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
        && value.chars().count() < min as usize
    {
        bail!("{path} must contain at least {min} character(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["work"],
            "properties": {
                "work": {"type": "string", "minLength": 1},
                "files": {
                    "type": "array",
                    "items": {"type": "string", "minLength": 1},
                    "minItems": 1
                }
            },
            "additionalProperties": false
        })
    }

    #[test]
    fn validates_payload_against_supported_schema_subset() {
        validate_handoff_schema(&schema()).expect("schema should validate");
        validate_handoff_payload(
            &schema(),
            &serde_json::json!({"work": "review this", "files": ["src/lib.rs"]}),
        )
        .expect("payload should validate");
    }

    #[test]
    fn rejects_missing_required_payload_field() {
        let error = validate_handoff_payload(&schema(), &serde_json::json!({}))
            .expect_err("payload should fail");
        assert!(error.to_string().contains("handoff.work is required"));
    }

    #[test]
    fn rejects_unsupported_schema_keyword() {
        let error = validate_handoff_schema(&serde_json::json!({
            "type": "object",
            "oneOf": []
        }))
        .expect_err("schema should fail");
        assert!(error.to_string().contains("oneOf is not supported"));
    }
}
