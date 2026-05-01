use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand_core::OsRng;
use serde::Serialize;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use super::types::{CURRENT_CRYPTO_VERSION, StoredWorkerEnrollment, StoredWorkerIdentity};

pub(super) const IDENTITY_FILE: &str = "identity.json";
pub(super) const ENROLLMENT_FILE: &str = "enrollment.json";

pub(super) fn load_or_create_identity(root: &Path) -> Result<StoredWorkerIdentity> {
    let path = root.join(IDENTITY_FILE);
    if path.exists() {
        return read_json(&path);
    }

    let enc_secret = StaticSecret::random_from_rng(OsRng);
    let enc_public = X25519PublicKey::from(&enc_secret);
    let sign_secret = SigningKey::generate(&mut OsRng);
    let sign_public = VerifyingKey::from(&sign_secret);

    let identity = StoredWorkerIdentity {
        worker_id: uuid::Uuid::new_v4(),
        created_at: Utc::now(),
        crypto_version: CURRENT_CRYPTO_VERSION,
        enc_secret_key: BASE64.encode(enc_secret.to_bytes()),
        enc_public_key: BASE64.encode(enc_public.as_bytes()),
        sign_secret_key: BASE64.encode(sign_secret.to_bytes()),
        sign_public_key: BASE64.encode(sign_public.to_bytes()),
    };
    write_json_atomic(&path, &identity)?;
    Ok(identity)
}

pub(super) fn generate_verification_code() -> String {
    format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000)
}

pub(super) fn load_enrollment(root: &Path) -> Result<StoredWorkerEnrollment> {
    let path = root.join(ENROLLMENT_FILE);
    if path.exists() {
        read_json(&path)
    } else {
        Ok(StoredWorkerEnrollment::default())
    }
}

pub(super) fn persist_enrollment(root: &Path, enrollment: &StoredWorkerEnrollment) -> Result<()> {
    write_json_atomic(&root.join(ENROLLMENT_FILE), enrollment)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read crypto state file: {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse crypto state file: {}", path.display()))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create crypto state dir: {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_vec_pretty(value)
        .with_context(|| format!("Failed to serialize crypto state: {}", path.display()))?;
    fs::write(&tmp, body)
        .with_context(|| format!("Failed to write temp crypto state: {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "Failed to persist crypto state file {} from {}",
            path.display(),
            tmp.display()
        )
    })?;
    Ok(())
}
