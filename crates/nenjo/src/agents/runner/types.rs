//! Types for the agent runner: events, output, config.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nenjo_models::ChatMessage;
use nenjo_tools::ToolResult;
use tokio::sync::Notify;

/// A single tool call with its name and arguments.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub tool_name: String,
    pub tool_args: String,
    pub text_preview: Option<String>,
}

/// Events yielded by the turn loop during execution.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// An ability sub-execution started.
    AbilityStarted {
        ability_tool_name: String,
        ability_name: String,
        task_input: String,
        caller_history: Vec<ChatMessage>,
    },
    /// One or more tool calls are starting.
    ToolCallStart {
        parent_tool_name: Option<String>,
        calls: Vec<ToolCall>,
    },
    /// A tool call completed with a result.
    ToolCallEnd {
        parent_tool_name: Option<String>,
        tool_name: String,
        result: ToolResult,
    },
    /// An ability sub-execution finished.
    AbilityCompleted {
        ability_tool_name: String,
        ability_name: String,
        success: bool,
        final_output: String,
    },
    /// Older history was compacted into a summary.
    MessageCompacted {
        messages_before: usize,
        messages_after: usize,
    },
    /// Execution was paused by the caller.
    Paused,
    /// Execution was resumed after a pause.
    Resumed,
    /// Execution finished.
    Done { output: TurnOutput },
}

/// Token for pausing and resuming an agent execution.
///
/// Shared between the `ExecutionHandle` (caller side) and the turn loop.
/// The turn loop checks `is_paused()` before each LLM call and waits
/// until `resume()` is called.
#[derive(Clone)]
pub struct PauseToken {
    paused: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Default for PauseToken {
    fn default() -> Self {
        Self::new()
    }
}

impl PauseToken {
    pub fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Pause execution. The turn loop will block before the next LLM call.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    /// Resume execution. Wakes the turn loop if it's waiting.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Check if execution is currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Wait until resumed. Returns immediately if not paused.
    pub async fn wait_if_paused(&self) {
        while self.is_paused() {
            self.notify.notified().await;
        }
    }
}

/// Final output of a turn loop execution.
#[derive(Debug, Clone)]
pub struct TurnOutput {
    /// The agent's final text response.
    pub text: String,
    /// Total input tokens consumed across all LLM calls.
    pub input_tokens: u64,
    /// Total output tokens generated across all LLM calls.
    pub output_tokens: u64,
    /// Number of tool calls executed.
    pub tool_calls: u32,
    /// Full conversation messages (for history persistence).
    pub messages: Vec<ChatMessage>,
}

/// Configuration for the turn loop.
#[derive(Debug, Clone)]
pub struct TurnLoopConfig {
    /// Maximum number of LLM call iterations before forcing a stop.
    pub max_iterations: u32,
    /// Whether to execute multiple tool calls in parallel.
    pub parallel_tools: bool,
}

impl Default for TurnLoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            parallel_tools: true,
        }
    }
}
