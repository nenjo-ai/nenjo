//! Domain contract for human-review routine steps.
//!
//! This module owns the package-facing review and immutable decision contracts.
//! Incoming edge handoffs are ordinary JSON Schema values; routing, artifact
//! storage, persistence, and authorization deliberately live outside this
//! module.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

/// Current normalized human-review contract identifier.
pub const HUMAN_REVIEW_CONTRACT: &str = "nenjo.human-review.v1";

/// Stable identifier for one immutable human request round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HumanRequestId(Uuid);

impl HumanRequestId {
    /// Construct an identifier from its UUID representation.
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Return the UUID representation.
    pub const fn into_uuid(self) -> Uuid {
        self.0
    }
}

/// Configuration of one human-review routine step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanStepSpec {
    /// Display title template evaluated against task context by the host.
    #[serde(rename = "title")]
    pub title_template: String,
    /// Optional structured fields collected only with approval.
    #[serde(default, rename = "approval", skip_serializing_if = "Option::is_none")]
    pub approval_schema: Option<ApprovalSchema>,
}

impl HumanStepSpec {
    /// Parse and validate a package-facing request object.
    pub fn parse(value: Value) -> Result<Self, HumanReviewError> {
        let spec: Self = serde_json::from_value(value)
            .map_err(|error| HumanReviewError::InvalidRequest(error.to_string()))?;
        if spec.title_template.trim().is_empty() {
            return Err(HumanReviewError::InvalidRequest(
                "request title must not be empty".to_string(),
            ));
        }
        if let Some(approval) = &spec.approval_schema {
            approval.validate()?;
        }
        Ok(spec)
    }

    /// Snapshot approval options against the activated incoming edge inputs.
    pub fn snapshot_options(
        &self,
        inputs: &[HumanReviewInput],
    ) -> Result<ApprovalOptionSnapshot, HumanReviewError> {
        match &self.approval_schema {
            Some(schema) => schema.snapshot_options(inputs),
            None => Ok(ApprovalOptionSnapshot::default()),
        }
    }
}

/// One activated incoming edge displayed as an independent review input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanReviewInput {
    /// Stable package input key. This is the source step slug in v1.
    pub input: String,
    /// Human-readable source step name.
    pub source_name: String,
    /// Optional edge purpose displayed as supporting context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Snapshotted handoff schema used to render this input.
    pub schema: Value,
    /// Validated handoff value produced by the source step.
    pub value: Value,
}

/// Package-defined approval form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalSchema {
    /// Ordered fields displayed by the dashboard.
    pub fields: Vec<ApprovalField>,
}

impl ApprovalSchema {
    fn validate(&self) -> Result<(), HumanReviewError> {
        let mut ids = HashSet::new();
        for field in &self.fields {
            field.validate()?;
            if !ids.insert(field.id.as_str()) {
                return Err(HumanReviewError::InvalidApprovalSchema(format!(
                    "duplicate approval field id '{}'",
                    field.id
                )));
            }
        }
        Ok(())
    }

    /// Resolve static and input-derived options into an immutable snapshot.
    pub fn snapshot_options(
        &self,
        inputs: &[HumanReviewInput],
    ) -> Result<ApprovalOptionSnapshot, HumanReviewError> {
        let mut fields = HashMap::with_capacity(self.fields.len());
        for field in &self.fields {
            let options = match &field.options {
                ApprovalOptions::Static { values } => values.clone(),
                ApprovalOptions::Inputs { inputs: sources } => {
                    resolve_input_options(inputs, sources, &field.id)?
                }
            };
            ensure_unique_option_values(&field.id, &options)?;
            fields.insert(field.id.clone(), options);
        }
        Ok(ApprovalOptionSnapshot { fields })
    }

    /// Validate approval answers against required fields and snapshotted options.
    pub fn validate_answers(
        &self,
        answers: &Value,
        snapshot: &ApprovalOptionSnapshot,
    ) -> Result<Value, HumanReviewError> {
        let object = answers.as_object().ok_or_else(|| {
            HumanReviewError::InvalidAnswers("approval answers must be an object".to_string())
        })?;
        let known = self
            .fields
            .iter()
            .map(|field| field.id.as_str())
            .collect::<HashSet<_>>();
        if let Some(unknown) = object.keys().find(|id| !known.contains(id.as_str())) {
            return Err(HumanReviewError::InvalidAnswers(format!(
                "unknown approval field '{unknown}'"
            )));
        }

        for field in &self.fields {
            let answer = object.get(&field.id);
            if field.required && answer.is_none() {
                return Err(HumanReviewError::InvalidAnswers(format!(
                    "approval field '{}' is required",
                    field.id
                )));
            }
            let Some(answer) = answer else { continue };
            let allowed = snapshot.fields.get(&field.id).ok_or_else(|| {
                HumanReviewError::InvalidAnswers(format!(
                    "approval option snapshot is missing field '{}'",
                    field.id
                ))
            })?;
            field.validate_answer(answer, allowed)?;
        }
        Ok(Value::Object(object.clone()))
    }
}

/// One single- or multi-selection approval field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalField {
    /// Stable key used in the approval answers object.
    pub id: String,
    /// Human-readable field label.
    pub label: String,
    /// Selection cardinality.
    #[serde(rename = "type")]
    pub field_type: ApprovalFieldType,
    /// Whether the field must be answered.
    #[serde(default)]
    pub required: bool,
    /// Minimum selected values for multi-select fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<usize>,
    /// Maximum selected values for multi-select fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    /// Static or subject-derived allowed options.
    pub options: ApprovalOptions,
}

impl ApprovalField {
    fn validate(&self) -> Result<(), HumanReviewError> {
        if self.id.trim().is_empty() || self.label.trim().is_empty() {
            return Err(HumanReviewError::InvalidApprovalSchema(
                "approval field id and label must not be empty".to_string(),
            ));
        }
        if self.field_type == ApprovalFieldType::SingleSelect
            && (self.min_items.is_some() || self.max_items.is_some())
        {
            return Err(HumanReviewError::InvalidApprovalSchema(format!(
                "single-select field '{}' cannot define min_items or max_items",
                self.id
            )));
        }
        if let (Some(min), Some(max)) = (self.min_items, self.max_items)
            && min > max
        {
            return Err(HumanReviewError::InvalidApprovalSchema(format!(
                "approval field '{}' has min_items greater than max_items",
                self.id
            )));
        }
        match &self.options {
            ApprovalOptions::Static { values } if values.is_empty() => {
                Err(HumanReviewError::InvalidApprovalSchema(format!(
                    "approval field '{}' must define at least one option",
                    self.id
                )))
            }
            ApprovalOptions::Static { values } => ensure_unique_option_values(&self.id, values),
            ApprovalOptions::Inputs { inputs } if inputs.is_empty() => {
                Err(HumanReviewError::InvalidApprovalSchema(format!(
                    "approval field '{}' must define at least one input option source",
                    self.id
                )))
            }
            ApprovalOptions::Inputs { inputs } => {
                let mut keys = HashSet::new();
                for source in inputs {
                    source.validate(&self.id)?;
                    if !keys.insert(source.input.as_str()) {
                        return Err(HumanReviewError::InvalidApprovalSchema(format!(
                            "approval field '{}' contains duplicate input source '{}'",
                            self.id, source.input
                        )));
                    }
                }
                Ok(())
            }
        }
    }

    fn validate_answer(
        &self,
        answer: &Value,
        allowed: &[ApprovalOption],
    ) -> Result<(), HumanReviewError> {
        let allowed = allowed
            .iter()
            .map(|option| &option.value)
            .collect::<HashSet<_>>();
        match self.field_type {
            ApprovalFieldType::SingleSelect => {
                if !is_stable_scalar(answer) || !allowed.contains(answer) {
                    return Err(HumanReviewError::InvalidAnswers(format!(
                        "approval field '{}' contains an invalid selection",
                        self.id
                    )));
                }
            }
            ApprovalFieldType::MultiSelect => {
                let values = answer.as_array().ok_or_else(|| {
                    HumanReviewError::InvalidAnswers(format!(
                        "approval field '{}' must be an array",
                        self.id
                    ))
                })?;
                let minimum = self.min_items.unwrap_or(usize::from(self.required));
                if values.len() < minimum {
                    return Err(HumanReviewError::InvalidAnswers(format!(
                        "approval field '{}' requires at least {minimum} selection(s)",
                        self.id
                    )));
                }
                if self.max_items.is_some_and(|maximum| values.len() > maximum) {
                    return Err(HumanReviewError::InvalidAnswers(format!(
                        "approval field '{}' has too many selections",
                        self.id
                    )));
                }
                let mut unique = HashSet::new();
                if values.iter().any(|value| {
                    !is_stable_scalar(value) || !allowed.contains(value) || !unique.insert(value)
                }) {
                    return Err(HumanReviewError::InvalidAnswers(format!(
                        "approval field '{}' contains an invalid or duplicate selection",
                        self.id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Closed set of initial approval controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalFieldType {
    /// Exactly one scalar value.
    SingleSelect,
    /// A bounded set of scalar values.
    MultiSelect,
}

/// Static or incoming-input-derived options for an approval field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApprovalOptions {
    /// Options authored directly in the package.
    Static { values: Vec<ApprovalOption> },
    /// Options projected from one or more activated incoming handoffs.
    Inputs { inputs: Vec<InputOptionSource> },
}

/// One immutable approval choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalOption {
    /// Stable scalar submitted by clients.
    pub value: Value,
    /// Human-readable label rendered by clients.
    pub label: String,
}

/// JSON Pointer projection used to derive approval options from one input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputOptionSource {
    /// Source step slug identifying one possible incoming input.
    pub input: String,
    /// Pointer to the array containing option records.
    pub pointer: String,
    /// Pointer relative to each array item yielding a scalar value.
    pub value: String,
    /// Pointer relative to each array item yielding a label.
    pub label: String,
}

impl InputOptionSource {
    fn validate(&self, field_id: &str) -> Result<(), HumanReviewError> {
        if self.input.trim().is_empty() {
            return Err(HumanReviewError::InvalidApprovalSchema(format!(
                "approval field '{field_id}' options input must not be empty"
            )));
        }
        for (label, pointer) in [
            ("pointer", &self.pointer),
            ("value", &self.value),
            ("label", &self.label),
        ] {
            validate_json_pointer(pointer).map_err(|reason| {
                HumanReviewError::InvalidApprovalSchema(format!(
                    "approval field '{field_id}' has invalid {label}: {reason}"
                ))
            })?;
        }
        Ok(())
    }

    fn resolve(
        &self,
        input: &Value,
        field_id: &str,
    ) -> Result<Vec<ApprovalOption>, HumanReviewError> {
        let items = input
            .pointer(&self.pointer)
            .and_then(Value::as_array)
            .ok_or_else(|| HumanReviewError::OptionProjection {
                field: field_id.to_string(),
                reason: format!("pointer '{}' did not resolve to an array", self.pointer),
            })?;
        items
            .iter()
            .map(|item| {
                let value = item.pointer(&self.value).cloned().ok_or_else(|| {
                    HumanReviewError::OptionProjection {
                        field: field_id.to_string(),
                        reason: format!("value pointer '{}' did not resolve", self.value),
                    }
                })?;
                if !is_stable_scalar(&value) {
                    return Err(HumanReviewError::OptionProjection {
                        field: field_id.to_string(),
                        reason: "option values must be strings, numbers, or booleans".to_string(),
                    });
                }
                let label = item
                    .pointer(&self.label)
                    .and_then(Value::as_str)
                    .filter(|label| !label.trim().is_empty())
                    .ok_or_else(|| HumanReviewError::OptionProjection {
                        field: field_id.to_string(),
                        reason: format!(
                            "label pointer '{}' did not resolve to a non-empty string",
                            self.label
                        ),
                    })?
                    .to_string();
                Ok(ApprovalOption { value, label })
            })
            .collect()
    }
}

fn resolve_input_options(
    inputs: &[HumanReviewInput],
    sources: &[InputOptionSource],
    field_id: &str,
) -> Result<Vec<ApprovalOption>, HumanReviewError> {
    let mut options = Vec::new();
    let mut matched = false;
    for source in sources {
        let Some(input) = inputs.iter().find(|input| input.input == source.input) else {
            // An input source may describe an alternative incoming edge that
            // was not activated in this request round.
            continue;
        };
        matched = true;
        options.extend(source.resolve(&input.value, field_id)?);
    }
    if !matched {
        return Err(HumanReviewError::OptionProjection {
            field: field_id.to_string(),
            reason: "none of the configured input sources were activated".to_string(),
        });
    }
    if options.is_empty() {
        return Err(HumanReviewError::OptionProjection {
            field: field_id.to_string(),
            reason: "configured input sources produced no options".to_string(),
        });
    }
    Ok(options)
}

/// Immutable allowed choices captured when a request is opened.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApprovalOptionSnapshot {
    /// Options keyed by approval field identifier.
    pub fields: HashMap<String, Vec<ApprovalOption>>,
}

/// Exactly one human resolution for a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HumanDecision {
    /// Continue through the approved edge with optional structured answers.
    Approved {
        #[serde(default = "empty_object")]
        answers: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
    },
    /// Continue through the revision edge with required instructions.
    ChangesRequested { instructions: String },
    /// Continue through the rejected edge with a required reason.
    Rejected { reason: String },
}

impl HumanDecision {
    /// Validate and normalize a decision against the snapshotted request contract.
    pub fn validate(
        self,
        approval_schema: Option<&ApprovalSchema>,
        snapshot: &ApprovalOptionSnapshot,
    ) -> Result<Self, HumanReviewError> {
        match self {
            Self::Approved { answers, comment } => {
                let answers = match approval_schema {
                    Some(schema) => schema.validate_answers(&answers, snapshot)?,
                    None if answers.as_object().is_some_and(Map::is_empty) => empty_object(),
                    None => {
                        return Err(HumanReviewError::InvalidAnswers(
                            "approval answers are not allowed for this request".to_string(),
                        ));
                    }
                };
                Ok(Self::Approved {
                    answers,
                    comment: trim_optional(comment),
                })
            }
            Self::ChangesRequested { instructions } => Ok(Self::ChangesRequested {
                instructions: required_text("changes_requested instructions", instructions)?,
            }),
            Self::Rejected { reason } => Ok(Self::Rejected {
                reason: required_text("rejection reason", reason)?,
            }),
        }
    }

    /// Return the closed graph-routing outcome for this decision.
    pub const fn outcome(&self) -> HumanReviewOutcome {
        match self {
            Self::Approved { .. } => HumanReviewOutcome::Approved,
            Self::ChangesRequested { .. } => HumanReviewOutcome::ChangesRequested,
            Self::Rejected { .. } => HumanReviewOutcome::Rejected,
        }
    }
}

/// Closed set of human-review graph outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanReviewOutcome {
    /// Review accepted the activated inputs.
    Approved,
    /// Review requires a new request round after revision.
    ChangesRequested,
    /// Review rejected the activated inputs.
    Rejected,
}

impl HumanReviewOutcome {
    /// Wire value used by package edges and continuation handoffs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes_requested",
            Self::Rejected => "rejected",
        }
    }
}

impl fmt::Display for HumanReviewOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Typed human-review contract error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HumanReviewError {
    /// The request object is malformed or contains forbidden fields.
    #[error("invalid human request: {0}")]
    InvalidRequest(String),
    /// The approval schema is malformed.
    #[error("invalid approval schema: {0}")]
    InvalidApprovalSchema(String),
    /// A dynamic approval option could not be projected.
    #[error("could not derive options for approval field '{field}': {reason}")]
    OptionProjection { field: String, reason: String },
    /// Approval answers or required decision text are invalid.
    #[error("invalid human decision: {0}")]
    InvalidAnswers(String),
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

fn required_text(label: &str, value: String) -> Result<String, HumanReviewError> {
    let value = value.trim();
    if value.is_empty() {
        Err(HumanReviewError::InvalidAnswers(format!(
            "{label} must not be empty"
        )))
    } else {
        Ok(value.to_string())
    }
}

fn trim_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn ensure_unique_option_values(
    field_id: &str,
    options: &[ApprovalOption],
) -> Result<(), HumanReviewError> {
    let mut values = HashSet::new();
    for option in options {
        if !is_stable_scalar(&option.value) || option.label.trim().is_empty() {
            return Err(HumanReviewError::InvalidApprovalSchema(format!(
                "approval field '{field_id}' options require scalar values and non-empty labels"
            )));
        }
        let encoded = serde_json::to_string(&option.value).expect("JSON scalar serializes");
        if !values.insert(encoded) {
            return Err(HumanReviewError::InvalidApprovalSchema(format!(
                "approval field '{field_id}' contains duplicate option values"
            )));
        }
    }
    Ok(())
}

fn is_stable_scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn validate_json_pointer(pointer: &str) -> Result<(), &'static str> {
    if pointer.is_empty() {
        return Ok(());
    }
    if !pointer.starts_with('/') {
        return Err("JSON Pointer must be empty or start with '/'");
    }
    if pointer.split('/').skip(1).any(|token| {
        token
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'~' && !matches!(pair[1], b'0' | b'1'))
            || token.ends_with('~')
    }) {
        return Err("JSON Pointer contains an invalid '~' escape");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_spec() -> HumanStepSpec {
        HumanStepSpec::parse(json!({
            "title": "Review {{ task.title }}",
            "approval": {
                "fields": [{
                    "id": "selected_components",
                    "label": "Components to approve",
                    "type": "multi_select",
                    "required": true,
                    "min_items": 1,
                    "options": {
                        "type": "inputs",
                        "inputs": [
                            {"input": "api", "pointer": "/components", "value": "/id", "label": "/name"},
                            {"input": "dashboard", "pointer": "/components", "value": "/id", "label": "/name"}
                        ]
                    }
                }]
            }
        }))
        .unwrap()
    }

    fn release_inputs() -> Vec<HumanReviewInput> {
        vec![
            HumanReviewInput {
                input: "api".into(),
                source_name: "API".into(),
                purpose: None,
                schema: json!({"type": "object"}),
                value: json!({"components": [{"id": "api", "name": "Platform API"}]}),
            },
            HumanReviewInput {
                input: "dashboard".into(),
                source_name: "Dashboard".into(),
                purpose: None,
                schema: json!({"type": "object"}),
                value: json!({"components": [{"id": "dashboard", "name": "Dashboard"}]}),
            },
        ]
    }

    #[test]
    fn rejects_forbidden_policy_and_timeout_fields() {
        for forbidden in ["policy", "timeout", "eligible_scope", "quorum", "revision"] {
            let mut value = serde_json::to_value(release_spec()).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert(forbidden.into(), json!(true));
            let error = HumanStepSpec::parse(value).unwrap_err();
            assert!(error.to_string().contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn snapshots_options_from_multiple_activated_inputs() {
        let spec = release_spec();
        let snapshot = spec.snapshot_options(&release_inputs()).unwrap();
        assert_eq!(
            snapshot.fields["selected_components"],
            vec![
                ApprovalOption {
                    value: json!("api"),
                    label: "Platform API".into()
                },
                ApprovalOption {
                    value: json!("dashboard"),
                    label: "Dashboard".into()
                }
            ]
        );
    }

    #[test]
    fn approval_answers_are_checked_against_the_snapshot() {
        let spec = release_spec();
        let snapshot = spec.snapshot_options(&release_inputs()).unwrap();
        let approval = spec.approval_schema.as_ref().unwrap();
        approval
            .validate_answers(
                &json!({"selected_components": ["api", "dashboard"]}),
                &snapshot,
            )
            .unwrap();
        assert!(
            approval
                .validate_answers(&json!({"selected_components": ["worker"]}), &snapshot)
                .is_err()
        );
        assert!(approval.validate_answers(&json!({}), &snapshot).is_err());
    }

    #[test]
    fn decisions_enforce_non_empty_text_and_normalize_empty_approval() {
        assert!(
            HumanDecision::ChangesRequested {
                instructions: "  ".into()
            }
            .validate(None, &ApprovalOptionSnapshot::default())
            .is_err()
        );
        assert!(
            HumanDecision::Rejected {
                reason: "\n".into()
            }
            .validate(None, &ApprovalOptionSnapshot::default())
            .is_err()
        );
        assert_eq!(
            HumanDecision::Approved {
                answers: json!({}),
                comment: Some("  ok  ".into())
            }
            .validate(None, &ApprovalOptionSnapshot::default())
            .unwrap(),
            HumanDecision::Approved {
                answers: json!({}),
                comment: Some("ok".into())
            }
        );
    }
}
