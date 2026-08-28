use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand_core::OsRng;
use serde::Serialize;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use super::types::{
    BoundWorkerEnrollment, CURRENT_CRYPTO_VERSION, StoredWorkerEnrollment, StoredWorkerEnrollments,
    StoredWorkerIdentity, WorkerEnrollmentBinding,
};

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

pub(super) fn load_enrollments(root: &Path) -> Result<StoredWorkerEnrollments> {
    let path = root.join(ENROLLMENT_FILE);
    if !path.exists() {
        return Ok(StoredWorkerEnrollments::default());
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read crypto state file: {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse crypto state file: {}", path.display()))?;

    if value.get("schema_version").is_some() {
        let stored: StoredWorkerEnrollments = serde_json::from_value(value)
            .with_context(|| format!("Failed to parse crypto state file: {}", path.display()))?;
        anyhow::ensure!(
            stored.schema_version == 2,
            "Unsupported worker enrollment schema version {}",
            stored.schema_version
        );
        validate_enrollments(&stored)?;
        return Ok(stored);
    }

    // Migrate the original single-slot format. Only a certificate can prove
    // which account and API key own the wrapped material; unbound material is
    // intentionally not carried into the account-indexed store.
    let legacy: StoredWorkerEnrollment = serde_json::from_value(value).with_context(|| {
        format!(
            "Failed to parse legacy crypto state file: {}",
            path.display()
        )
    })?;
    let mut stored = StoredWorkerEnrollments::default();
    if let Some(certificate) = legacy.certificate.as_ref() {
        let binding = WorkerEnrollmentBinding::new(certificate.account_id, certificate.api_key_id);
        stored.selected_binding = Some(binding);
        stored.entries.push(BoundWorkerEnrollment {
            binding,
            enrollment: legacy,
        });
    }
    persist_enrollments(root, &stored)?;
    Ok(stored)
}

fn validate_enrollments(stored: &StoredWorkerEnrollments) -> Result<()> {
    let mut bindings = HashSet::with_capacity(stored.entries.len());
    for entry in &stored.entries {
        anyhow::ensure!(
            bindings.insert(entry.binding),
            "Duplicate persisted worker enrollment binding for account {} and API key {}",
            entry.binding.account_id(),
            entry.binding.api_key_id()
        );
        if let Some(certificate) = entry.enrollment.certificate.as_ref() {
            anyhow::ensure!(
                certificate.account_id == entry.binding.account_id()
                    && certificate.api_key_id == entry.binding.api_key_id(),
                "Persisted worker certificate did not match its enrollment binding"
            );
        }
    }
    if let Some(selected) = stored.selected_binding {
        anyhow::ensure!(
            bindings.contains(&selected),
            "Selected worker enrollment binding does not have a persisted entry"
        );
    }
    Ok(())
}

pub(super) fn persist_enrollments(
    root: &Path,
    enrollments: &StoredWorkerEnrollments,
) -> Result<()> {
    write_json_atomic(&root.join(ENROLLMENT_FILE), enrollments)
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
