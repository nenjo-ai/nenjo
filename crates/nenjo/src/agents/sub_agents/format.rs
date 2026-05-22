use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::SubAgentError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ResultFormat {
    pub(crate) fields: Vec<ResultField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ResultField {
    pub(crate) name: ResultFieldName,
    pub(crate) field_type: ResultFieldType,
    pub(crate) description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ResultFieldName(String);

impl ResultFieldName {
    pub(crate) fn parse(raw: impl Into<String>) -> Result<Self, SubAgentError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(SubAgentError::InvalidResultFieldName(
                "field name cannot be empty".into(),
            ));
        }
        if raw.len() > 64 {
            return Err(SubAgentError::InvalidResultFieldName(
                "field name cannot be longer than 64 characters".into(),
            ));
        }
        let mut chars = raw.chars();
        let Some(first) = chars.next() else {
            return Err(SubAgentError::InvalidResultFieldName(
                "field name cannot be empty".into(),
            ));
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(SubAgentError::InvalidResultFieldName(
                "field name must start with a letter or underscore".into(),
            ));
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(SubAgentError::InvalidResultFieldName(
                "field name may contain only letters, numbers, and underscores".into(),
            ));
        }
        Ok(Self(raw))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResultFieldType {
    String,
    Number,
    Boolean,
    List,
    Object,
}

impl ResultFieldType {
    fn matches(self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::List => value.is_array(),
            Self::Object => value.is_object(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawResultFormat {
    #[serde(default)]
    fields: Vec<RawResultField>,
}

#[derive(Debug, Deserialize)]
struct RawResultField {
    name: String,
    #[serde(default = "default_field_type")]
    #[serde(rename = "type")]
    field_type: ResultFieldType,
    #[serde(default)]
    description: String,
}

fn default_field_type() -> ResultFieldType {
    ResultFieldType::String
}

impl ResultFormat {
    pub(crate) fn parse(value: &Value) -> Result<Self, SubAgentError> {
        let raw: RawResultFormat = serde_json::from_value(value.clone()).map_err(|err| {
            SubAgentError::InvalidResultFieldName(format!("invalid result_format: {err}"))
        })?;
        let fields = raw
            .fields
            .into_iter()
            .map(|field| {
                Ok(ResultField {
                    name: ResultFieldName::parse(field.name)?,
                    field_type: field.field_type,
                    description: field.description,
                })
            })
            .collect::<Result<Vec<_>, SubAgentError>>()?;
        Ok(Self { fields })
    }

    pub(crate) fn instructions(&self) -> String {
        if self.fields.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\n\nReturn your final answer as a single JSON object with these fields:\n",
        );
        for field in &self.fields {
            out.push_str("- ");
            out.push_str(field.name.as_str());
            out.push_str(": ");
            out.push_str(match field.field_type {
                ResultFieldType::String => "string",
                ResultFieldType::Number => "number",
                ResultFieldType::Boolean => "boolean",
                ResultFieldType::List => "list",
                ResultFieldType::Object => "object",
            });
            if !field.description.is_empty() {
                out.push_str(" - ");
                out.push_str(&field.description);
            }
            out.push('\n');
        }
        out
    }

    pub(crate) fn validate_output(&self, text: &str) -> (Option<Value>, Option<bool>) {
        if self.fields.is_empty() {
            return (None, None);
        }
        let Ok(value) = serde_json::from_str::<Value>(text.trim()) else {
            return (None, Some(false));
        };
        let Some(object) = value.as_object() else {
            return (Some(value), Some(false));
        };
        let valid = self.fields.iter().all(|field| {
            object
                .get(field.name.as_str())
                .is_some_and(|value| field.field_type.matches(value))
        });
        (Some(value), Some(valid))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ResultFormat;

    #[test]
    fn parses_and_validates_result_format() {
        let format = ResultFormat::parse(&json!({
            "fields": [
                {"name": "summary", "type": "string"},
                {"name": "issues", "type": "list"}
            ]
        }))
        .unwrap();
        let (_, valid) = format.validate_output(r#"{"summary":"ok","issues":[]}"#);
        assert_eq!(valid, Some(true));
        let (_, valid) = format.validate_output(r#"{"summary":"ok","issues":"none"}"#);
        assert_eq!(valid, Some(false));
    }
}
