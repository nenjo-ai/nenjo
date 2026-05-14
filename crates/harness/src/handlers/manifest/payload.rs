use serde::Deserialize;
use uuid::Uuid;

pub use nenjo_platform::{
    AbilityDocument, AbilityPromptDocument, AgentDocument, AgentPromptDocument,
    ContextBlockContentDocument, ContextBlockDocument, CouncilDocument, DomainDocument,
    DomainPromptDocument, ManifestKind, ProjectDocument,
};

#[derive(Debug, Deserialize)]
pub(super) struct InlineDocumentMeta {
    pub id: Uuid,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub pack_id: Option<Uuid>,
    #[serde(default)]
    pub slug: Option<String>,
    pub filename: String,
    pub path: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub authority: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub size_bytes: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub(super) struct DecryptedManifestPayload<'a> {
    pub object_type: &'a str,
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
        inline_payload: object
            .get("inline_payload")
            .filter(|value| !value.is_null()),
        decrypted_payload: object.get("decrypted_payload")?,
    })
}
