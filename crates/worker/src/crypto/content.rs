use anyhow::{Context, Result, bail};
use nenjo_crypto_auth::{ContentScope, WorkerAuthProvider};
use nenjo_events::EncryptedPayload;
use nenjo_secure_envelope::{decrypt_text, encrypt_text_for_scope};
use uuid::Uuid;

pub async fn encrypt_text_with_provider(
    auth_provider: &WorkerAuthProvider,
    scope: ContentScope,
    account_id: Uuid,
    object_id: Uuid,
    object_type: impl Into<String>,
    plaintext: &str,
) -> Result<EncryptedPayload> {
    let key = match scope {
        ContentScope::User => auth_provider.load_ack_for_user(account_id).await?,
        ContentScope::Org => auth_provider.load_ock().await?,
    }
    .with_context(|| match scope {
        ContentScope::User => format!("worker has no enrolled ACK for user {account_id}"),
        ContentScope::Org => "worker has no enrolled OCK".to_string(),
    })?;
    let key_version = match scope {
        ContentScope::User => auth_provider.current_key_version_for_user(account_id).await,
        ContentScope::Org => auth_provider.current_ock_key_version().await,
    }
    .unwrap_or(1);
    encrypt_text_for_scope(
        &key,
        scope,
        account_id,
        object_id,
        object_type,
        plaintext,
        key_version,
    )
}

pub async fn decrypt_text_with_provider(
    auth_provider: &WorkerAuthProvider,
    payload: &EncryptedPayload,
) -> Result<String> {
    let scope = ContentScope::from_payload(payload);
    if scope == ContentScope::Org {
        let enrollment = auth_provider.enrollment().await;
        let enrolled_org_id = enrollment
            .certificate
            .as_ref()
            .map(|certificate| certificate.account_id)
            .context("worker enrollment missing org certificate")?;
        if payload.account_id != enrolled_org_id {
            bail!(
                "org-scoped payload account mismatch: payload={}, worker={}",
                payload.account_id,
                enrolled_org_id
            );
        }
    }
    let key = match scope {
        ContentScope::User => auth_provider.load_ack_for_user(payload.account_id).await?,
        ContentScope::Org => auth_provider.load_ock().await?,
    }
    .with_context(|| match scope {
        ContentScope::User => format!("worker has no enrolled ACK for user {}", payload.account_id),
        ContentScope::Org => "worker has no enrolled OCK".to_string(),
    })?;
    decrypt_text(&key, payload)
}

#[cfg(test)]
mod tests {
    use super::decrypt_text_with_provider;
    use crate::crypto::{
        ContentKey, ContentScope, WorkerAuthProvider, WorkerCertificate, decrypt_text,
        encrypt_text, encrypt_text_for_scope,
    };
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn aad_mismatch_fails() {
        let key = ContentKey::from_bytes([8_u8; 32]);
        let mut payload = encrypt_text(
            &key,
            Uuid::new_v4(),
            Uuid::new_v4(),
            "agent_prompt",
            "secret",
            1,
        )
        .unwrap();
        payload.object_type = "agent_response".into();

        assert!(decrypt_text(&key, &payload).is_err());
    }

    #[test]
    fn round_trip_text_payload_aes_gcm() {
        let key = ContentKey::from_bytes([4_u8; 32]);
        let payload = encrypt_text(
            &key,
            Uuid::new_v4(),
            Uuid::new_v4(),
            "agent_prompt",
            "hello from browser crypto",
            1,
        )
        .unwrap();

        let decrypted = decrypt_text(&key, &payload).unwrap();
        assert_eq!(decrypted, "hello from browser crypto");
    }

    #[tokio::test]
    async fn rejects_foreign_org_scoped_payload_for_enrolled_worker() {
        let dir = tempfile::tempdir().unwrap();
        let provider = WorkerAuthProvider::load_or_create(dir.path()).unwrap();
        let identity = provider.identity().clone();
        let enrolled_org_id = Uuid::new_v4();
        provider
            .store_enrollment(
                Some(WorkerCertificate {
                    account_id: enrolled_org_id,
                    api_key_id: Uuid::new_v4(),
                    issued_at: Utc::now(),
                    enc_public_key: identity.enc_public_key.clone(),
                    sign_public_key: identity.sign_public_key.clone(),
                    signature: "test-signature".into(),
                }),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let payload = encrypt_text_for_scope(
            &ContentKey::from_bytes([5_u8; 32]),
            ContentScope::Org,
            Uuid::new_v4(),
            Uuid::new_v4(),
            "manifest.agent.prompt",
            "foreign-org secret",
            1,
        )
        .unwrap();

        let error = decrypt_text_with_provider(&provider, &payload)
            .await
            .expect_err("foreign org payload should be rejected before decrypt");
        assert!(
            error
                .to_string()
                .contains("org-scoped payload account mismatch"),
            "unexpected error: {error:#}",
        );
    }
}
