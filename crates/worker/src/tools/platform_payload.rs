use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nenjo_platform::{ContentScope, ManifestKind, SensitivePayloadEncoder};

use crate::crypto::WorkerAuthProvider;
use crate::crypto::{decrypt_text_with_provider, encrypt_text_with_provider};

#[derive(Clone)]
pub(crate) struct PlatformPayloadEncoder {
    auth_provider: Arc<WorkerAuthProvider>,
}

impl PlatformPayloadEncoder {
    pub(crate) fn new(auth_provider: Arc<WorkerAuthProvider>) -> Self {
        Self { auth_provider }
    }
}

fn payload_scope_for_object_type(object_type: &str) -> ContentScope {
    ManifestKind::from_encrypted_object_type(object_type)
        .and_then(ManifestKind::encrypted_scope)
        .unwrap_or(ContentScope::User)
}

#[async_trait]
impl SensitivePayloadEncoder for PlatformPayloadEncoder {
    async fn encode_payload(
        &self,
        account_id: uuid::Uuid,
        object_id: uuid::Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let scope = payload_scope_for_object_type(object_type);
        let encrypted_payload = encrypt_text_with_provider(
            &self.auth_provider,
            scope,
            account_id,
            object_id,
            object_type,
            &serde_json::to_string(payload)?,
        )
        .await?;
        Ok(Some(serde_json::to_value(encrypted_payload)?))
    }

    async fn decode_payload(
        &self,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let encrypted_payload: nenjo_events::EncryptedPayload =
            serde_json::from_value(payload.clone()).context("invalid encrypted payload JSON")?;
        let plaintext = decrypt_text_with_provider(&self.auth_provider, &encrypted_payload).await?;
        Ok(Some(serde_json::from_str(&plaintext)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_scope_uses_org_for_org_owned_manifest_resources() {
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::Agent
                    .encrypted_object_type()
                    .expect("agent prompt object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::Ability
                    .encrypted_object_type()
                    .expect("ability prompt object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::Domain
                    .encrypted_object_type()
                    .expect("domain prompt object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::ContextBlock
                    .encrypted_object_type()
                    .expect("context block content object type"),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                ManifestKind::ProjectDocument
                    .encrypted_object_type()
                    .expect("document content object type"),
            ),
            ContentScope::Org
        );
    }

    #[test]
    fn payload_scope_falls_back_to_user_for_other_payloads() {
        assert_eq!(
            payload_scope_for_object_type("chat.message"),
            ContentScope::User
        );
    }
}
