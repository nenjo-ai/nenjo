use aes_gcm::{
    Aes256Gcm, Nonce as AesNonce,
    aead::{Aead as AesAead, KeyInit as AesKeyInit, OsRng as AesOsRng},
};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use nenjo_crypto_auth::{ContentKey, ContentScope};
use nenjo_events::EncryptedPayload;
use rand_core::RngCore;
use uuid::Uuid;

const CONTENT_ALGORITHM_AES_256_GCM: &str = "aes-256-gcm";

pub fn encrypt_text(
    key: &ContentKey,
    account_id: Uuid,
    object_id: Uuid,
    object_type: impl Into<String>,
    plaintext: &str,
    key_version: u32,
) -> Result<EncryptedPayload> {
    encrypt_text_for_scope(
        key,
        ContentScope::User,
        account_id,
        object_id,
        object_type.into(),
        plaintext,
        key_version,
    )
}

pub fn encrypt_text_for_scope(
    key: &ContentKey,
    scope: ContentScope,
    account_id: Uuid,
    object_id: Uuid,
    object_type: impl Into<String>,
    plaintext: &str,
    key_version: u32,
) -> Result<EncryptedPayload> {
    encrypt_with_aes_256_gcm(
        key,
        scope,
        account_id,
        object_id,
        object_type.into(),
        plaintext,
        key_version,
    )
}

pub fn decrypt_text(key: &ContentKey, payload: &EncryptedPayload) -> Result<String> {
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

struct EncryptedPayloadParts {
    account_id: Uuid,
    encryption_scope: Option<String>,
    object_id: Uuid,
    object_type: String,
    algorithm: String,
    key_version: u32,
    nonce: String,
    ciphertext: String,
}

impl From<EncryptedPayloadParts> for EncryptedPayload {
    fn from(parts: EncryptedPayloadParts) -> Self {
        Self {
            account_id: parts.account_id,
            encryption_scope: parts.encryption_scope,
            object_id: parts.object_id,
            object_type: parts.object_type,
            algorithm: parts.algorithm,
            key_version: parts.key_version,
            nonce: parts.nonce,
            ciphertext: parts.ciphertext,
        }
    }
}

fn encrypt_with_aes_256_gcm(
    key: &ContentKey,
    scope: ContentScope,
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

    Ok(EncryptedPayloadParts {
        account_id,
        encryption_scope: scope.encryption_scope_value().map(str::to_string),
        object_id,
        object_type,
        algorithm: CONTENT_ALGORITHM_AES_256_GCM.to_string(),
        key_version,
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(ciphertext),
    }
    .into())
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
    key: &ContentKey,
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
