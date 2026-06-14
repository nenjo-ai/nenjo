use serde::Deserialize;
use uuid::Uuid;

pub use nenjo_platform::{
    AbilityDocument, AbilityPromptDocument, AgentDocument, AgentPromptDocument,
    ContextBlockContentDocument, ContextBlockDocument, CouncilDocument, DomainDocument,
    DomainPromptDocument, ProjectDocument,
};

#[derive(Debug, Deserialize)]
pub(super) struct ManifestResourcePayload {
    pub schema: String,
    pub data: serde_json::Value,
}

pub(super) fn canonical_resource_payload_data(
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    match serde_json::from_value::<ManifestResourcePayload>(value.clone()) {
        Ok(envelope) if envelope.schema == "manifest.resource.v1" => Some(envelope.data),
        _ => None,
    }
}

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
