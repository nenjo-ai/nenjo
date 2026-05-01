use aes_gcm::{
    Aes256Gcm, Nonce as AesNonce,
    aead::{Aead as AesAead, KeyInit as AesKeyInit, OsRng as AesOsRng},
};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use nenjo_events::EncryptedPayload;
use rand_core::RngCore;
use uuid::Uuid;

use super::provider::AccountContentKey;

const CONTENT_ALGORITHM_AES_256_GCM: &str = "aes-256-gcm";

pub fn encrypt_text(
    key: &AccountContentKey,
    account_id: Uuid,
    object_id: Uuid,
    object_type: impl Into<String>,
    plaintext: &str,
    key_version: u32,
) -> Result<EncryptedPayload> {
    encrypt_with_aes_256_gcm(
        key,
        account_id,
        object_id,
        object_type.into(),
        plaintext,
        key_version,
    )
}

pub fn decrypt_text(key: &AccountContentKey, payload: &EncryptedPayload) -> Result<String> {
    let ciphertext = BASE64
        .decode(&payload.ciphertext)
        .context("Invalid base64 ciphertext")?;
    let aad = payload_aad(payload.account_id, payload.object_id, &payload.object_type);
    let plaintext = match payload.algorithm.as_str() {
        CONTENT_ALGORITHM_AES_256_GCM => {
            decrypt_with_aes_256_gcm(key, &payload.nonce, ciphertext.as_ref(), &aad)?
        }
        other => bail!("Unsupported encrypted payload algorithm: {other}"),
    };

    String::from_utf8(plaintext).context("Decrypted payload was not valid UTF-8")
}

fn payload_aad(account_id: Uuid, object_id: Uuid, object_type: &str) -> Vec<u8> {
    format!("{account_id}:{object_id}:{object_type}").into_bytes()
}

fn build_encrypted_payload(
    account_id: Uuid,
    object_id: Uuid,
    object_type: String,
    algorithm: &str,
    key_version: u32,
    nonce: &[u8],
    ciphertext: &[u8],
) -> EncryptedPayload {
    EncryptedPayload {
        account_id,
        object_id,
        object_type,
        algorithm: algorithm.to_string(),
        key_version,
        nonce: BASE64.encode(nonce),
        ciphertext: BASE64.encode(ciphertext),
    }
}

fn encrypt_with_aes_256_gcm(
    key: &AccountContentKey,
    account_id: Uuid,
    object_id: Uuid,
    object_type: String,
    plaintext: &str,
    key_version: u32,
) -> Result<EncryptedPayload> {
    let aad = payload_aad(account_id, object_id, &object_type);
    let cipher = Aes256Gcm::new(key.as_bytes().into());
    let mut nonce_bytes = [0_u8; 12];
    AesOsRng.fill_bytes(&mut nonce_bytes);
    let nonce = AesNonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            aes_gcm::aead::Payload {
                msg: plaintext.as_bytes(),
                aad: &aad,
            },
        )
        .context("Failed to encrypt AES-GCM content payload")?;

    Ok(build_encrypted_payload(
        account_id,
        object_id,
        object_type,
        CONTENT_ALGORITHM_AES_256_GCM,
        key_version,
        &nonce_bytes,
        &ciphertext,
    ))
}

fn decode_fixed<const N: usize>(raw: &str, field: &str) -> Result<[u8; N]> {
    let bytes = BASE64
        .decode(raw)
        .with_context(|| format!("Invalid base64 in {field}"))?;
    if bytes.len() != N {
        bail!("Invalid {field} length: expected {N}, got {}", bytes.len());
    }
    let mut out = [0_u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn decrypt_with_aes_256_gcm(
    key: &AccountContentKey,
    nonce_b64: &str,
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let nonce = AesNonce::from(decode_fixed::<12>(nonce_b64, "nonce")?);
    let cipher = Aes256Gcm::new(key.as_bytes().into());
    cipher
        .decrypt(
            &nonce,
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad,
            },
        )
        .context("Failed to decrypt AES-256-GCM content payload")
}

#[cfg(test)]
mod tests {
    use super::{decrypt_text, encrypt_text};
    use crate::crypto::AccountContentKey;
    use uuid::Uuid;

    #[test]
    fn aad_mismatch_fails() {
        let key = AccountContentKey::from_bytes([8_u8; 32]);
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
        let key = AccountContentKey::from_bytes([4_u8; 32]);
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
}
