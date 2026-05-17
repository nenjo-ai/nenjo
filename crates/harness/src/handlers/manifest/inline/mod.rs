mod decrypted;
mod plain;

pub(super) use decrypted::apply_decrypted_manifest_upsert;
pub(super) use plain::apply_inline_upsert;
