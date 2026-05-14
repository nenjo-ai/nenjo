//! Shared prompt-config merge helpers for manifest backends.

use anyhow::Result;
use nenjo::manifest::PromptConfig;

fn merge_json_patch(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(existing) => merge_json_patch(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

pub(crate) fn merge_prompt_config(
    current: &PromptConfig,
    patch: serde_json::Value,
) -> Result<PromptConfig> {
    let mut value = serde_json::to_value(current)?;
    merge_json_patch(&mut value, patch);
    Ok(serde_json::from_value(value)?)
}
