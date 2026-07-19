use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) use crate::agents::runner::types::SubAgentTranscriptEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubAgentStatus {
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SubAgentSignal {
    Started {
        task_summary: String,
    },
    Progress {
        summary: String,
        details: Option<String>,
    },
    NeedsInput {
        question: String,
        context: Option<String>,
    },
    Completed {
        summary: String,
        structured_result: Option<Value>,
        result_format_valid: Option<bool>,
    },
    Failed {
        error: String,
    },
    Stopped {
        reason: Option<String>,
    },
}

impl SubAgentSignal {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Progress { .. } => "progress",
            Self::NeedsInput { .. } => "needs_input",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Stopped { .. } => "stopped",
        }
    }
}
