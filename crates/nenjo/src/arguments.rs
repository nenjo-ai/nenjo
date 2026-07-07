//! Runtime argument definitions and bindings for package-authored prompts.
//!
//! Package arguments are values supplied by the host application at runtime and
//! rendered through the `args.*` template namespace.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::str::FromStr;

use quick_xml::events::Event;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Stable package-local argument name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArgumentName(String);

impl ArgumentName {
    /// Parse a package-local argument name.
    pub fn parse(value: impl Into<String>) -> Result<Self, ArgumentError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ArgumentError::InvalidName(
                "argument name cannot be empty".to_string(),
            ));
        }
        if trimmed.starts_with('_') {
            return Err(ArgumentError::InvalidName(format!(
                "argument name '{trimmed}' cannot start with '_'"
            )));
        }
        if !is_snake_case_ident(trimmed) {
            return Err(ArgumentError::InvalidName(format!(
                "argument name '{trimmed}' must be a snake_case identifier"
            )));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ArgumentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ArgumentName {
    type Err = ArgumentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ArgumentName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ArgumentName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Prompt-facing `args.*` selector.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArgumentSelector(String);

impl ArgumentSelector {
    /// Parse an `args.*` render selector.
    pub fn parse(value: impl Into<String>) -> Result<Self, ArgumentError> {
        let value = value.into();
        let trimmed = value.trim();
        let Some(rest) = trimmed.strip_prefix("args.") else {
            return Err(ArgumentError::InvalidSelector(format!(
                "argument selector '{trimmed}' must start with 'args.'"
            )));
        };
        if rest.is_empty() {
            return Err(ArgumentError::InvalidSelector(
                "argument selector must include a name after 'args.'".to_string(),
            ));
        }
        for segment in rest.split('.') {
            if segment.is_empty() || !is_jinja_ident(segment) {
                return Err(ArgumentError::InvalidSelector(format!(
                    "argument selector '{trimmed}' contains invalid segment '{segment}'"
                )));
            }
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ArgumentSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ArgumentSelector {
    type Err = ArgumentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ArgumentSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ArgumentSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Host-owned binding scope for a package argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentScope {
    Org,
    User,
}

/// Declared value shape for a package argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentValueType {
    Text,
    Markdown,
    Xml,
    Json,
}

impl ArgumentValueType {
    /// Validate and coerce a value into the string inserted into render vars.
    pub fn coerce_render_value(&self, value: &ArgumentValue) -> Result<String, ArgumentError> {
        match self {
            Self::Text | Self::Markdown => Ok(value.as_str().to_string()),
            Self::Json => {
                let parsed: serde_json::Value = serde_json::from_str(value.as_str())
                    .map_err(|error| ArgumentError::InvalidJson(error.to_string()))?;
                serde_json::to_string(&parsed)
                    .map_err(|error| ArgumentError::InvalidJson(error.to_string()))
            }
            Self::Xml => {
                validate_xml_fragment(value.as_str())?;
                Ok(value.as_str().to_string())
            }
        }
    }

    pub fn synthetic_value(&self, name: &ArgumentName) -> ArgumentValue {
        match self {
            Self::Text => ArgumentValue::new(format!("validation value for {}", name.as_str())),
            Self::Markdown => {
                ArgumentValue::new(format!("Validation markdown for `{}`.", name.as_str()))
            }
            Self::Xml => ArgumentValue::new(format!(
                "<argument name=\"{}\">validation</argument>",
                name.as_str()
            )),
            Self::Json => ArgumentValue::new(format!(
                r#"{{"argument":"{}","value":"validation"}}"#,
                name.as_str()
            )),
        }
    }
}

/// Raw argument value before type-specific coercion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArgumentValue(String);

impl ArgumentValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<String> for ArgumentValue {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ArgumentValue {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Package-authored runtime argument contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageArgumentDefinition {
    pub name: ArgumentName,
    pub selector: ArgumentSelector,
    pub scope: ArgumentScope,
    #[serde(rename = "type")]
    pub value_type: ArgumentValueType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<ArgumentValue>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sample: Option<ArgumentValue>,
}

impl PackageArgumentDefinition {
    /// Return the value package validation should use for strict rendering.
    pub fn validation_value(&self) -> Result<String, ArgumentError> {
        let value = self
            .sample
            .as_ref()
            .or(self.default.as_ref())
            .cloned()
            .unwrap_or_else(|| self.value_type.synthetic_value(&self.name));
        self.value_type.coerce_render_value(&value)
    }
}

/// Fully resolved argument binding ready for prompt rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedArgumentBinding {
    pub package: String,
    pub name: ArgumentName,
    pub selector: ArgumentSelector,
    #[serde(rename = "type")]
    pub value_type: ArgumentValueType,
    pub value: ArgumentValue,
}

impl ResolvedArgumentBinding {
    pub fn new(
        package: impl Into<String>,
        name: impl Into<String>,
        selector: impl Into<String>,
        value_type: ArgumentValueType,
        value: impl Into<ArgumentValue>,
    ) -> Result<Self, ArgumentError> {
        Ok(Self {
            package: package.into(),
            name: ArgumentName::parse(name.into())?,
            selector: ArgumentSelector::parse(selector.into())?,
            value_type,
            value: value.into(),
        })
    }

    pub fn render_value(&self) -> Result<String, ArgumentError> {
        self.value_type.coerce_render_value(&self.value)
    }
}

/// Merge provider-level and execution-level bindings into render vars.
pub fn merge_argument_bindings<'a>(
    provider: impl IntoIterator<Item = &'a ResolvedArgumentBinding>,
    execution: impl IntoIterator<Item = &'a ResolvedArgumentBinding>,
) -> Result<HashMap<String, String>, ArgumentError> {
    let mut by_argument = BTreeMap::<(String, ArgumentName), String>::new();
    let mut by_selector = BTreeMap::<ArgumentSelector, String>::new();
    let mut vars = HashMap::new();

    for binding in provider.into_iter().chain(execution) {
        let value = binding.render_value()?;
        let key = (binding.package.clone(), binding.name.clone());
        if let Some(existing) = by_argument.get(&key)
            && existing != &value
        {
            return Err(ArgumentError::DuplicateBinding {
                package: binding.package.clone(),
                name: binding.name.to_string(),
            });
        }
        if let Some(existing) = by_selector.get(&binding.selector)
            && existing != &value
        {
            return Err(ArgumentError::SelectorConflict {
                selector: binding.selector.to_string(),
            });
        }
        by_argument.insert(key, value.clone());
        by_selector.insert(binding.selector.clone(), value.clone());
        vars.insert(binding.selector.to_string(), value);
    }

    Ok(vars)
}

/// Return every `args.*` selector referenced in a template-like string.
pub fn scan_argument_selectors(value: &str) -> Vec<String> {
    scan_selector_path(value, "args.")
        .into_iter()
        .filter(|segments| !segments.is_empty())
        .map(|segments| format!("args.{}", segments.join(".")))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ArgumentError {
    #[error("{0}")]
    InvalidName(String),
    #[error("{0}")]
    InvalidSelector(String),
    #[error("invalid JSON argument value: {0}")]
    InvalidJson(String),
    #[error("invalid XML argument value: {0}")]
    InvalidXml(String),
    #[error("conflicting bindings for {package}.{name}")]
    DuplicateBinding { package: String, name: String },
    #[error("conflicting bindings for selector {selector}")]
    SelectorConflict { selector: String },
}

fn validate_xml_fragment(value: &str) -> Result<(), ArgumentError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ArgumentError::InvalidXml(
            "XML argument value cannot be empty".to_string(),
        ));
    }
    if !(trimmed.starts_with('<') && trimmed.ends_with('>')) {
        return Err(ArgumentError::InvalidXml(
            "XML argument value must be an XML fragment".to_string(),
        ));
    }

    let wrapped = format!("<argument-root>{trimmed}</argument-root>");
    let mut reader = quick_xml::Reader::from_str(&wrapped);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(ArgumentError::InvalidXml(error.to_string())),
        }
        buf.clear();
    }
    Ok(())
}

fn is_snake_case_ident(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase())
        && chars.all(|ch| ch == '_' || ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

fn is_jinja_ident(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn scan_selector_path(value: &str, prefix: &str) -> Vec<Vec<String>> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(offset) = value[index..].find(prefix) {
        let start = index + offset;
        if start > 0 {
            let previous = bytes[start - 1] as char;
            if is_ident_continue(previous) || previous == '.' {
                index = start + prefix.len();
                continue;
            }
        }
        if has_odd_preceding_backslashes(value, start) {
            index = start + prefix.len();
            continue;
        }
        let mut cursor = start + prefix.len();
        let mut segments = Vec::new();
        while let Some((segment, next_cursor)) = read_ident(value, cursor) {
            segments.push(segment.to_string());
            cursor = next_cursor;
            if value[cursor..].starts_with('.') {
                cursor += 1;
            } else {
                break;
            }
        }
        if !segments.is_empty() {
            out.push(segments);
        }
        index = (start + prefix.len()).min(value.len());
    }
    out
}

fn read_ident(value: &str, start: usize) -> Option<(&str, usize)> {
    let mut chars = value[start..].char_indices();
    let (_, first) = chars.next()?;
    if !is_ident_start(first) {
        return None;
    }
    let mut end = start + first.len_utf8();
    for (offset, ch) in chars {
        if !is_ident_continue(ch) {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    Some((&value[start..end], end))
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    is_ident_start(ch) || ch.is_ascii_digit()
}

fn has_odd_preceding_backslashes(value: &str, byte_index: usize) -> bool {
    value[..byte_index]
        .chars()
        .rev()
        .take_while(|ch| *ch == '\\')
        .count()
        % 2
        == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_argument_selectors() {
        assert_eq!(
            scan_argument_selectors("{{ args.company }} {{ args.profile.name }}"),
            vec!["args.company", "args.profile.name"]
        );
    }

    #[test]
    fn validates_json_values_canonically() {
        let binding = ResolvedArgumentBinding::new(
            "pkg",
            "profile",
            "args.profile",
            ArgumentValueType::Json,
            r#"{ "name": "Ada" }"#,
        )
        .unwrap();

        assert_eq!(binding.render_value().unwrap(), r#"{"name":"Ada"}"#);
    }

    #[test]
    fn rejects_conflicting_selector_values() {
        let first = ResolvedArgumentBinding::new(
            "pkg",
            "first",
            "args.profile",
            ArgumentValueType::Text,
            "Ada",
        )
        .unwrap();
        let second = ResolvedArgumentBinding::new(
            "other",
            "second",
            "args.profile",
            ArgumentValueType::Text,
            "Grace",
        )
        .unwrap();

        let error = merge_argument_bindings([&first], [&second]).unwrap_err();

        assert!(matches!(error, ArgumentError::SelectorConflict { .. }));
    }
}
