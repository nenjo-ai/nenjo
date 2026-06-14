//! Typed inline payloads for [`crate::Command::ManifestChanged`].

use serde::{Deserialize, Serialize};

/// Schema version for inline manifest resource envelopes.
pub const MANIFEST_RESOURCE_SCHEMA: &str = "manifest.resource.v1";

/// Envelope wrapping canonical inline manifest resource bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestResourcePayload<T> {
    pub schema: String,
    pub data: T,
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
    fn parse_rejects_wrong_schema() {
        let value = json!({
            "schema": "manifest.resource.v0",
            "data": {}
        });
        assert!(ManifestResourcePayload::<SampleResource>::parse(&value).is_none());
    }
}
