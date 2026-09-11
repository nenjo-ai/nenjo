//! Generation controls and terminal errors shared by Responses API adapters.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::TokenUsage;

/// Wire-level reasoning effort. Availability and meaning depend on the served model.
/// `None` explicitly requests no thinking; an unspecified effort uses the adapter's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    /// Exact effort string sent in the Responses request.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Optional generation controls for a Responses endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResponsesOptions {
    /// Unspecified by default. vLLM resolves an unspecified effort to `max` and
    /// forwards the setting to the model's chat template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Total generation budget, including reasoning and visible output tokens.
    /// Zero is rejected when deserializing configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<NonZeroU32>,
}

/// Why a provider stopped without producing an executable, complete response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ResponseTermination {
    #[error("output token limit reached")]
    OutputLimit,
    #[error("content filtered")]
    ContentFilter,
    #[error("cancelled")]
    Cancelled,
    #[error("failed")]
    Failed,
    #[error("incomplete")]
    Incomplete,
}

/// A terminal Responses generation failure, retaining provider identity and usage.
///
/// These failures bypass automatic retries and fallback, including when no text
/// has been emitted. Partial function arguments are never exposed for execution.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Responses generation {reason} (response {response_id:?}): {message}")]
pub struct ResponseTerminationError {
    /// Terminal category used to decide whether the operation can complete.
    pub reason: ResponseTermination,
    /// Provider-generated response identity, when present in the terminal payload.
    pub response_id: Option<String>,
    /// Usage consumed by the unsuccessful generation, including reported breakdowns.
    pub usage: TokenUsage,
    /// Provider error message or incomplete reason, without partial generated output.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_omit_controls_and_invalid_budgets_or_efforts_are_rejected() {
        assert_eq!(
            serde_json::to_string(&ResponsesOptions::default()).unwrap(),
            "{}"
        );
        for json in [
            r#"{"max_output_tokens":0}"#,
            r#"{"max_output_tokens":-1}"#,
            r#"{"reasoning_effort":"automatic"}"#,
        ] {
            assert!(serde_json::from_str::<ResponsesOptions>(json).is_err());
        }
        let options: ResponsesOptions =
            serde_json::from_str(r#"{"reasoning_effort":"none","max_output_tokens":32}"#).unwrap();
        assert_eq!(options.reasoning_effort, Some(ReasoningEffort::None));
        assert_eq!(options.max_output_tokens.unwrap().get(), 32);
    }
}
