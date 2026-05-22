use std::collections::VecDeque;

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

impl SubAgentStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingForInput => "waiting_for_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    pub(crate) fn can_receive_input(self) -> bool {
        matches!(self, Self::Running | Self::WaitingForInput)
    }
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
    pub(crate) fn wakes_parent(&self) -> bool {
        matches!(
            self,
            Self::NeedsInput { .. }
                | Self::Completed { .. }
                | Self::Failed { .. }
                | Self::Stopped { .. }
        )
    }

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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SignalDigest {
    pub(crate) slug: String,
    pub(crate) events: Vec<SubAgentSignal>,
}

pub(crate) fn push_bounded<T>(queue: &mut VecDeque<T>, item: T, cap: usize) {
    queue.push_back(item);
    while queue.len() > cap {
        queue.pop_front();
    }
}
