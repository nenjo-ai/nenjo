use serde::de::DeserializeOwned;
use uuid::Uuid;

use nenjo_events::{ManifestResourcePayload, MANIFEST_RESOURCE_SCHEMA};

pub(super) struct DecryptedManifestPayload<'a> {
    pub object_type: &'a str,
    pub object_id: Uuid,
    pub inline_payload: Option<&'a serde_json::Value>,
    pub decrypted_payload: &'a serde_json::Value,
}

pub(super) fn parse_decrypted_manifest_payload(
    data: &serde_json::Value,
) -> Option<DecryptedManifestPayload<'_>> {
    let object = data.as_object()?;
    let marker = object
        .get("__nenjo_decrypted_manifest_payload")?
        .as_bool()?;
    if !marker {
        return None;
    }

    Some(DecryptedManifestPayload {
        object_type: object.get("object_type")?.as_str()?,
        object_id: serde_json::from_value(object.get("object_id")?.clone()).ok()?,
        inline_payload: object
            .get("inline_payload")
            .filter(|value| !value.is_null()),
        decrypted_payload: object.get("decrypted_payload")?,
    })
}

pub(super) fn parse_inline_record<T: DeserializeOwned>(
    value: &serde_json::Value,
) -> Option<T> {
    ManifestResourcePayload::<T>::parse(value).map(|envelope| envelope.data)
}

pub(super) fn is_canonical_inline_envelope(value: &serde_json::Value) -> bool {
    serde_json::from_value::<ManifestResourcePayload<serde_json::Value>>(value.clone())
        .ok()
        .is_some_and(|envelope| envelope.schema == MANIFEST_RESOURCE_SCHEMA)
}

pub(super) fn envelope_data_field<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Option<&'a serde_json::Value> {
    if !is_canonical_inline_envelope(value) {
        return None;
    }
    value.get("data")?.get(field)
}