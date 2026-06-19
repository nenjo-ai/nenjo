use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nenjo_platform::{
    ContentScope as PlatformContentScope, SensitiveContentKind, SensitivePayloadEncoder,
};

use crate::crypto::ContentScope;
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
    if object_type == "push.notification" {
        return ContentScope::Org;
    }
    SensitiveContentKind::from_encrypted_object_type(object_type)
        .map(|kind| kind.encrypted_scope())
        .map(|scope| match scope {
            nenjo_platform::ContentScope::User => ContentScope::User,
            nenjo_platform::ContentScope::Org => ContentScope::Org,
        })
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
        self.encode_payload_for_worker_scope(scope, account_id, object_id, object_type, payload)
            .await
    }

    async fn encode_payload_with_scope(
        &self,
        scope: PlatformContentScope,
        account_id: uuid::Uuid,
        object_id: uuid::Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        let scope = match scope {
            PlatformContentScope::User => ContentScope::User,
            PlatformContentScope::Org => ContentScope::Org,
        };
        self.encode_payload_for_worker_scope(scope, account_id, object_id, object_type, payload)
            .await
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

impl PlatformPayloadEncoder {
    async fn encode_payload_for_worker_scope(
        &self,
        scope: ContentScope,
        account_id: uuid::Uuid,
        object_id: uuid::Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_scope_uses_org_for_org_owned_manifest_resources() {
        assert_eq!(
            payload_scope_for_object_type(
                SensitiveContentKind::AgentPrompt.encrypted_object_type(),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                SensitiveContentKind::AbilityPrompt.encrypted_object_type(),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                SensitiveContentKind::DomainPrompt.encrypted_object_type(),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                SensitiveContentKind::ContextBlockContent.encrypted_object_type(),
            ),
            ContentScope::Org
        );
        assert_eq!(
            payload_scope_for_object_type(
                SensitiveContentKind::DocumentContent.encrypted_object_type(),
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

    #[test]
    fn payload_scope_uses_org_for_push_notifications() {
        assert_eq!(
            payload_scope_for_object_type("push.notification"),
            ContentScope::Org
        );
    }
}
