use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead as AesAead, KeyInit},
};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use super::types::{ACK_LEN, AccountContentKey, WrappedAccountContentKey};

pub(super) const WRAP_ALGORITHM_AES_GCM: &str = "x25519-hkdf-sha256-aes-256-gcm";
const HKDF_INFO: &[u8] = b"nenjo-worker-ack-wrap-v1";

pub(super) fn unwrap_ack(
    recipient_secret_key: &StaticSecret,
    wrapped: &WrappedAccountContentKey,
) -> Result<AccountContentKey> {
    let ephemeral_public_bytes =
        decode_fixed::<32>(&wrapped.ephemeral_public_key, "ephemeral_public_key")?;
    let ciphertext = decode_vec(&wrapped.ciphertext, "ciphertext")?;

    let ephemeral_public = X25519PublicKey::from(ephemeral_public_bytes);
    let shared_secret = recipient_secret_key.diffie_hellman(&ephemeral_public);
    let key = derive_wrap_key(shared_secret.as_bytes())?;
    if wrapped.algorithm != WRAP_ALGORITHM_AES_GCM {
        bail!("Unsupported ACK wrap algorithm: {}", wrapped.algorithm);
    }
    let nonce = decode_fixed::<12>(&wrapped.nonce, "nonce")?;
    let cipher = Aes256Gcm::new((&key).into());
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .context("Failed to decrypt wrapped ACK")?;

    if plaintext.len() != ACK_LEN {
        bail!("Unexpected ACK length: {}", plaintext.len());
    }

    let mut ack = [0_u8; ACK_LEN];
    ack.copy_from_slice(&plaintext);
    Ok(AccountContentKey::from_bytes(ack))
}

#[cfg(test)]
pub(crate) fn wrap_ack_for_recipient(
    recipient_public_key: &str,
    ack: &[u8; ACK_LEN],
    key_version: u32,
) -> Result<WrappedAccountContentKey> {
    let recipient_public_bytes = decode_fixed::<32>(recipient_public_key, "recipient_public_key")?;
    let recipient_public = X25519PublicKey::from(recipient_public_bytes);
    let ephemeral_secret = StaticSecret::random_from_rng(rand_core::OsRng);
    let ephemeral_public = X25519PublicKey::from(&ephemeral_secret);
    let shared_secret = ephemeral_secret.diffie_hellman(&recipient_public);
    let wrap_key = derive_wrap_key(shared_secret.as_bytes())?;

    let cipher = Aes256Gcm::new((&wrap_key).into());
    let mut nonce = [0_u8; 12];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), ack.as_ref())
        .context("Failed to wrap ACK for recipient")?;

    Ok(WrappedAccountContentKey {
        key_version,
        algorithm: WRAP_ALGORITHM_AES_GCM.to_string(),
        ephemeral_public_key: BASE64.encode(ephemeral_public.as_bytes()),
        nonce: BASE64.encode(nonce),
        ciphertext: BASE64.encode(ciphertext),
        created_at: chrono::Utc::now(),
    })
}

fn derive_wrap_key(shared_secret: &[u8; 32]) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(None, shared_secret);
    let mut out = [0_u8; 32];
    hk.expand(HKDF_INFO, &mut out)
        .map_err(|_| anyhow::anyhow!("Failed to derive ACK wrap key"))?;
    Ok(out)
}

fn decode_vec(raw: &str, field: &str) -> Result<Vec<u8>> {
    BASE64
        .decode(raw)
        .with_context(|| format!("Invalid base64 in {field}"))
}

fn decode_fixed<const N: usize>(raw: &str, field: &str) -> Result<[u8; N]> {
    let bytes = decode_vec(raw, field)?;
    if bytes.len() != N {
        bail!("Invalid {field} length: expected {N}, got {}", bytes.len());
    }
    let mut out = [0_u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}
