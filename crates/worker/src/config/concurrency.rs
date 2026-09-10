//! Worker admission pools and explicit provider-to-resource bindings.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// A model endpoint's physical request and waiting-room limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderPoolConfig {
    /// Maximum HTTP attempts simultaneously using this pool.
    pub max_concurrent_requests: usize,
    /// Maximum waiting HTTP attempts; zero disables queueing.
    pub max_queued_requests: usize,
    /// Maximum admission wait in seconds; must be positive.
    pub queue_timeout_secs: u64,
    /// Total HTTP deadline override; zero disables and omission inherits reliability settings.
    pub request_timeout_secs: Option<u64>,
    /// Idle HTTP read deadline override; zero disables and omission inherits reliability settings.
    pub read_timeout_secs: Option<u64>,
    /// HTTP connection deadline override; zero disables and omission inherits reliability settings.
    pub connect_timeout_secs: Option<u64>,
}

impl Default for ProviderPoolConfig {
    fn default() -> Self {
        Self {
            max_concurrent_requests: 6,
            max_queued_requests: 64,
            queue_timeout_secs: 300,
            request_timeout_secs: None,
            read_timeout_secs: None,
            connect_timeout_secs: None,
        }
    }
}

/// Bind a provider/tag to a named pool, optionally selecting one exact configured URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPoolBinding {
    /// Provider name including any OpenAI-compatible tag.
    pub provider: String,
    #[serde(default)]
    /// Optional exact configured URL; omitted bindings match every URL for this provider.
    pub base_url: Option<String>,
    /// Name of the shared capacity pool.
    pub pool: String,
}

/// Defaults for unbound providers, plus explicitly shared capacity pools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRuntimeConfig {
    /// Maximum HTTP attempts simultaneously using this pool.
    pub max_concurrent_requests: usize,
    /// Maximum waiting HTTP attempts; zero disables queueing.
    pub max_queued_requests: usize,
    /// Maximum admission wait in seconds; must be positive.
    pub queue_timeout_secs: u64,
    /// Named resources that bindings may share.
    pub pools: HashMap<String, ProviderPoolConfig>,
    /// Provider-to-pool assignments; exact URL matches precede provider-wide matches.
    pub bindings: Vec<ProviderPoolBinding>,
}

impl Default for ModelRuntimeConfig {
    fn default() -> Self {
        let defaults = ProviderPoolConfig::default();
        Self {
            max_concurrent_requests: defaults.max_concurrent_requests,
            max_queued_requests: defaults.max_queued_requests,
            queue_timeout_secs: defaults.queue_timeout_secs,
            pools: HashMap::new(),
            bindings: Vec::new(),
        }
    }
}

impl ModelRuntimeConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_limits(
            "model_runtime",
            self.max_concurrent_requests,
            self.max_queued_requests,
            self.queue_timeout_secs,
        )?;
        for (name, pool) in &self.pools {
            if name.trim().is_empty() {
                bail!("model_runtime pool names must not be empty");
            }
            validate_limits(
                name,
                pool.max_concurrent_requests,
                pool.max_queued_requests,
                pool.queue_timeout_secs,
            )?;
        }
        let mut seen = HashSet::new();
        for binding in &self.bindings {
            if binding.provider.trim().is_empty() || !self.pools.contains_key(&binding.pool) {
                bail!(
                    "provider binding '{}' references missing pool '{}' or has an empty provider",
                    binding.provider,
                    binding.pool
                );
            }
            if !seen.insert((&binding.provider, &binding.base_url)) {
                bail!("duplicate provider pool binding for '{}'", binding.provider);
            }
        }
        Ok(())
    }
}

/// Root executions include both chats and tasks using this worker's SDK provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionConfig {
    /// Maximum simultaneous root chats and tasks; child runs bypass this gate.
    pub max_active_roots: usize,
    /// Maximum roots waiting for execution admission.
    pub max_queued_roots: usize,
    /// Maximum admission wait in seconds; must be positive.
    pub queue_timeout_secs: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            max_active_roots: 5,
            max_queued_roots: 64,
            queue_timeout_secs: 300,
        }
    }
}

impl ExecutionConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_limits(
            "execution",
            self.max_active_roots,
            self.max_queued_roots,
            self.queue_timeout_secs,
        )
    }
}

fn validate_limits(name: &str, active: usize, queued: usize, seconds: u64) -> Result<()> {
    if !(1..=64).contains(&active) {
        bail!("{name}: concurrency must be between 1 and 64");
    }
    if queued > 4096 {
        bail!("{name}: queue capacity must not exceed 4096");
    }
    if !(1..=86400).contains(&seconds) {
        bail!("{name}: queue timeout must be between 1 and 86400 seconds");
    }
    Ok(())
}

/// Shared capacity for shell processes, including commands running in the background.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// Maximum managed shell processes, including background operations.
    pub max_concurrent_processes: usize,
    /// Maximum shell calls waiting to launch a process.
    pub max_queued_processes: usize,
    /// Maximum admission wait in seconds; must be positive.
    pub queue_timeout_secs: u64,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            max_concurrent_processes: 4,
            max_queued_processes: 64,
            queue_timeout_secs: 300,
        }
    }
}

impl ShellConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_limits(
            "shell",
            self.max_concurrent_processes,
            self.max_queued_processes,
            self.queue_timeout_secs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_pools_and_ambiguous_bindings() {
        for input in [
            "queue_timeout_secs = 0",
            "[pools.bad]\nmax_concurrent_requests = 0",
            "[[bindings]]\nprovider = 'vllm'\npool = 'missing'",
            "[pools.local]\n[[bindings]]\nprovider = 'vllm'\npool = 'local'\n[[bindings]]\nprovider = 'vllm'\npool = 'local'",
        ] {
            let config: ModelRuntimeConfig = toml::from_str(input).unwrap();
            assert!(config.validate().is_err(), "accepted {input}");
        }
        let defaults: ModelRuntimeConfig = toml::from_str("").unwrap();
        defaults.validate().unwrap();
        assert_eq!(defaults.max_concurrent_requests, 6);
    }
}
