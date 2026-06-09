//! Shared helpers for `manifest.resource.v1` envelopes.

use nenjo_events::{ManifestResourcePayload, MANIFEST_RESOURCE_SCHEMA};
use serde::{de::DeserializeOwned, Serialize};
use uuid::Uuid;

/// Minimal identity shared by manifest wire records.
pub trait PlatformRecord {
    fn id(&self) -> Uuid;
    fn slug(&self) -> &str;
}

/// Parse a typed manifest inline envelope.
pub fn parse_resource_payload<T: DeserializeOwned>(value: &serde_json::Value) -> Option<T> {
    ManifestResourcePayload::<T>::parse(value).map(|envelope| envelope.data)
}

/// Wrap a record for manifest event emission.
pub fn wrap_resource_record<T: Serialize>(record: T) -> serde_json::Value {
    ManifestResourcePayload::new(record).into_value()
}

/// Return whether an envelope explicitly included a JSON field on `data`.
pub fn data_field_present(envelope: &serde_json::Value, field: &str) -> Option<bool> {
    let data = envelope.get("data")?;
    data.as_object().map(|object| object.contains_key(field))
}

pub const MANIFEST_RESOURCE_SCHEMA_VERSION: &str = MANIFEST_RESOURCE_SCHEMA;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SampleRecord {
        id: Uuid,
        slug: String,
    }

    impl PlatformRecord for SampleRecord {
        fn id(&self) -> Uuid {
            self.id
        }

        fn slug(&self) -> &str {
            &self.slug
        }
    }

    #[test]
    fn wrap_and_parse_resource_record() {
        let record = SampleRecord {
            id: Uuid::from_u128(1),
            slug: "alpha".into(),
        };
        let value = wrap_resource_record(record.clone());
        let parsed = parse_resource_payload::<SampleRecord>(&value).expect("should parse");
        assert_eq!(parsed, record);
    }
}