//! Typed inline payloads for [`crate::Command::ManifestChanged`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema version for inline manifest resource envelopes.
pub const MANIFEST_RESOURCE_SCHEMA: &str = "manifest.resource.v1";

/// Envelope wrapping canonical inline manifest resource bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestResourcePayload<T> {
    pub schema: String,
    pub data: T,
}

/// Complete replacement snapshot for one agent's configured model bindings.
///
/// It is sent inline with a `model_assignment` manifest event. Replacing the
/// complete agent slice makes clearing assignments just as unambiguous as
/// adding or changing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAssignmentsManifestUpdate {
    pub agent_id: Uuid,
    pub assignments: Vec<ModelAssignmentBinding>,
}

/// One configured model binding in an agent assignment snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAssignmentBinding {
    pub capability: String,
    pub model_id: Uuid,
    pub assignment_source: String,
}

/// Complete replacement snapshot for organization capability defaults.
///
/// It is sent inline with a `model_capability_default` manifest event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilityDefaultsManifestUpdate {
    pub defaults: Vec<ModelCapabilityDefaultBinding>,
}

/// One configured model binding in an organization default snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilityDefaultBinding {
    pub capability: String,
    pub model_id: Uuid,
}

impl<T> ManifestResourcePayload<T> {
    pub fn new(data: T) -> Self {
        Self {
            schema: MANIFEST_RESOURCE_SCHEMA.to_string(),
            data,
        }
    }
}

impl<T: Serialize> ManifestResourcePayload<T> {
    pub fn into_value(self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

impl<T: for<'de> Deserialize<'de>> ManifestResourcePayload<T> {
    pub fn parse(value: &serde_json::Value) -> Option<Self> {
        let payload: Self = serde_json::from_value(value.clone()).ok()?;
        (payload.schema == MANIFEST_RESOURCE_SCHEMA).then_some(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SampleResource {
        name: String,
    }

    #[test]
    fn manifest_resource_payload_round_trips() {
        let payload = ManifestResourcePayload::new(SampleResource {
            name: "alpha".into(),
        });
        let value = payload.into_value();
        let parsed =
            ManifestResourcePayload::<SampleResource>::parse(&value).expect("payload should parse");
        assert_eq!(parsed.data.name, "alpha");
    }

    #[test]
    fn model_assignment_update_round_trips_as_an_inline_manifest_payload() {
        let update = ModelAssignmentsManifestUpdate {
            agent_id: Uuid::from_u128(1),
            assignments: vec![ModelAssignmentBinding {
                capability: "transcribe_audio".into(),
                model_id: Uuid::from_u128(2),
                assignment_source: "local".into(),
            }],
        };

        let payload = ManifestResourcePayload::new(update.clone()).into_value();
        let parsed = ManifestResourcePayload::<ModelAssignmentsManifestUpdate>::parse(&payload)
            .expect("model assignment inline payload should deserialize");

        assert_eq!(parsed.data, update);
    }

    #[test]
    fn parse_rejects_wrong_schema() {
        let value = json!({
            "schema": "manifest.resource.v0",
            "data": {}
        });
        assert!(ManifestResourcePayload::<SampleResource>::parse(&value).is_none());
    }
}
