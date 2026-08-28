//! Typed routine execution policy.

use serde::{Deserialize, Serialize};

/// A bounded number of retry-edge traversals after the initial gate evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GateRetryLimit(u32);

impl GateRetryLimit {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for GateRetryLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Default for GateRetryLimit {
    fn default() -> Self {
        Self::new(3)
    }
}

/// Routine execution limits applied by a provider runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineExecutionConfig {
    pub default_gate_max_retries: GateRetryLimit,
    pub max_gate_max_retries: GateRetryLimit,
}

impl RoutineExecutionConfig {
    pub const fn new(
        default_gate_max_retries: GateRetryLimit,
        max_gate_max_retries: GateRetryLimit,
    ) -> Result<Self, RoutineExecutionConfigError> {
        if default_gate_max_retries.get() > max_gate_max_retries.get() {
            return Err(RoutineExecutionConfigError::DefaultExceedsMaximum);
        }
        Ok(Self {
            default_gate_max_retries,
            max_gate_max_retries,
        })
    }

    pub fn effective_limit(
        self,
        override_limit: Option<GateRetryLimit>,
    ) -> Result<GateRetryLimit, RoutineExecutionConfigError> {
        let limit = override_limit.unwrap_or(self.default_gate_max_retries);
        if limit > self.max_gate_max_retries {
            return Err(RoutineExecutionConfigError::EdgeExceedsMaximum {
                requested: limit,
                maximum: self.max_gate_max_retries,
            });
        }
        Ok(limit)
    }
}

impl Default for RoutineExecutionConfig {
    fn default() -> Self {
        Self {
            default_gate_max_retries: GateRetryLimit::new(3),
            max_gate_max_retries: GateRetryLimit::new(10),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RoutineExecutionConfigError {
    #[error("default gate retry limit exceeds the worker maximum")]
    DefaultExceedsMaximum,
    #[error("gate retry override {requested} exceeds the worker maximum {maximum}")]
    EdgeExceedsMaximum {
        requested: GateRetryLimit,
        maximum: GateRetryLimit,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_edge_override_uses_worker_default() {
        let config =
            RoutineExecutionConfig::new(GateRetryLimit::new(5), GateRetryLimit::new(8)).unwrap();

        assert_eq!(config.effective_limit(None).unwrap().get(), 5);
    }

    #[test]
    fn edge_override_is_rejected_above_worker_ceiling() {
        let config =
            RoutineExecutionConfig::new(GateRetryLimit::new(3), GateRetryLimit::new(4)).unwrap();

        assert!(matches!(
            config.effective_limit(Some(GateRetryLimit::new(5))),
            Err(RoutineExecutionConfigError::EdgeExceedsMaximum { .. })
        ));
    }

    #[test]
    fn configuration_rejects_default_above_ceiling() {
        assert_eq!(
            RoutineExecutionConfig::new(GateRetryLimit::new(5), GateRetryLimit::new(4)),
            Err(RoutineExecutionConfigError::DefaultExceedsMaximum)
        );
    }
}
