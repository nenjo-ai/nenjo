use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::{info, warn};
use uuid::Uuid;

use crate::agent::MemoryProfile;
use crate::providers::{self, ChatMessage, Provider};

/// Options for chat history compaction.
pub struct CompactOptions<'a> {
    pub provider: &'a dyn Provider,
    pub model: &'a str,
    pub max_context_tokens: usize,
    pub memory_profile: Option<&'a MemoryProfile>,
}

/// File-based chat history store.
///
/// Persists full conversation messages (including tool calls and tool results)
/// to JSON files so history survives worker restarts and the LLM retains full
/// context across turns.
pub struct ChatHistory {
    workspace_dir: PathBuf,
}

impl ChatHistory {
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Path to the history file for a given project/agent/session.
    ///
    /// `{workspace}/{project_slug}/chat_history/{agent_name}_{session_id}.json`
    fn history_path(&self, project_slug: &str, agent_name: &str, session_id: Uuid) -> PathBuf {
        let dir = self.workspace_dir.join(project_slug).join("chat_history");
        let safe_name = agent_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        dir.join(format!("{}_{}.json", safe_name, session_id))
    }

    /// Read chat history from file. Returns `None` if the file doesn't exist.
    pub fn read(
        &self,
        project_slug: &str,
        agent_name: &str,
        session_id: Uuid,
    ) -> Option<Vec<ChatMessage>> {
        let path = self.history_path(project_slug, agent_name, session_id);
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return None,
        };
        match serde_json::from_str::<Vec<ChatMessage>>(&data) {
            Ok(messages) => Some(messages),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse chat history file");
                None
            }
        }
    }

    /// Write chat history to file, trimming to `max` *conversation turns*.
    ///
    /// A turn is a user message plus all subsequent non-user messages (assistant
    /// responses, tool calls, tool results) until the next user message. This
    /// prevents tool-heavy turns (e.g. MCP tool calls in creator mode) from
    /// pushing older conversation context out of the history buffer.
    ///
    /// Uses atomic write (write to temp + rename) to prevent corruption.
    pub fn write(
        &self,
        project_slug: &str,
        agent_name: &str,
        session_id: Uuid,
        messages: &[ChatMessage],
        max: usize,
    ) -> Result<()> {
        let path = self.history_path(project_slug, agent_name, session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Count conversation turns (each "user" message starts a new turn).
        // Trim by turns rather than raw message count so tool call/result
        // messages don't eat into the budget.
        let turn_count = messages.iter().filter(|m| m.role == "user").count();
        let trimmed = if turn_count > max {
            // Find the start of the (turn_count - max)th user message from the front
            let skip_turns = turn_count - max;
            let mut turns_seen = 0;
            let mut cutpoint = 0;
            for (i, msg) in messages.iter().enumerate() {
                if msg.role == "user" {
                    turns_seen += 1;
                    if turns_seen > skip_turns {
                        cutpoint = i;
                        break;
                    }
                }
            }
            &messages[cutpoint..]
        } else {
            messages
        };

        let json = serde_json::to_string(trimmed)?;

        // Atomic write: write to temp file then rename
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json.as_bytes())?;
        std::fs::rename(&tmp_path, &path)?;

        Ok(())
    }

    /// Delete the history file for a given session.
    ///
    /// Returns `Ok(true)` if a file was removed, `Ok(false)` if it didn't exist.
    pub fn delete(&self, project_slug: &str, agent_name: &str, session_id: Uuid) -> Result<bool> {
        let path = self.history_path(project_slug, agent_name, session_id);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                info!(path = %path.display(), "Deleted chat history file");
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Compact persisted chat history using an LLM to summarize older messages.
    ///
    /// When the history file exceeds 70% of `max_context_tokens`, splits
    /// messages into old (to be summarized) and recent (kept verbatim).
    /// The LLM generates a summary of the old messages which replaces them
    /// as a single assistant message.
    ///
    /// Returns `Ok(true)` if compaction happened, `Ok(false)` if not needed.
    pub async fn compact(
        &self,
        project_slug: &str,
        agent_name: &str,
        session_id: Uuid,
        opts: CompactOptions<'_>,
    ) -> Result<bool> {
        let CompactOptions {
            provider,
            model,
            max_context_tokens,
            memory_profile,
        } = opts;
        let messages = match self.read(project_slug, agent_name, session_id) {
            Some(m) if m.len() > 1 => m,
            _ => return Ok(false),
        };

        // Estimate tokens via len/4 heuristic
        let estimated_tokens: usize = messages.iter().map(|m| m.content.len() / 4).sum();
        let threshold = max_context_tokens * 7 / 10; // 70%

        if estimated_tokens < threshold {
            return Ok(false);
        }

        info!(
            estimated_tokens,
            threshold,
            message_count = messages.len(),
            "Chat history exceeds 70% of context window, compacting"
        );

        // Keep last ~40% of budget as recent messages
        let recent_budget = max_context_tokens * 4 / 10;
        let mut recent_tokens = 0usize;
        let mut cutpoint = messages.len();

        // Walk backwards to find cutpoint
        while cutpoint > 0 {
            let idx = cutpoint - 1;
            let msg_tokens = messages[idx].content.len() / 4;
            if recent_tokens + msg_tokens > recent_budget {
                break;
            }
            recent_tokens += msg_tokens;
            cutpoint -= 1;
        }

        // Snap cutpoint to a safe boundary: don't start recent on a "tool" message
        // (which would orphan it from its preceding assistant tool-call)
        while cutpoint < messages.len() && messages[cutpoint].role == "tool" {
            cutpoint += 1;
        }

        // Need at least 1 old message to summarize
        if cutpoint < 1 {
            return Ok(false);
        }

        let old = &messages[..cutpoint];
        let recent = &messages[cutpoint..];

        // Build the text representation of old messages for summarization
        let mut old_text = String::new();
        for msg in old {
            old_text.push_str(&format!("[{}]: {}\n\n", msg.role, msg.content));
        }

        // Build system prompt
        let mut system_prompt = String::from(
            "Summarize this conversation history concisely for an AI assistant to continue the conversation. \
             Include: what the user asked, what tools were used and their key results, decisions made, \
             and current state. Preserve specific details (file paths, command outputs, errors, code snippets) \
             that provide useful context.",
        );

        if let Some(profile) = memory_profile {
            let mut focus_areas = Vec::new();
            focus_areas.extend(profile.core_focus.iter().map(|s| s.as_str()));
            focus_areas.extend(profile.project_focus.iter().map(|s| s.as_str()));
            if !focus_areas.is_empty() {
                system_prompt.push_str(&format!(
                    " Pay special attention to: {}",
                    focus_areas.join(", ")
                ));
            }
        }

        // Call the LLM to summarize
        let summary =
            providers::one_shot(provider, Some(&system_prompt), &old_text, model, 0.2).await?;

        let summary_msg =
            ChatMessage::assistant(format!("Summary of earlier conversation: {}", summary));

        let messages_before = messages.len();

        // Build compacted history: summary + recent
        let mut compacted = Vec::with_capacity(1 + recent.len());
        compacted.push(summary_msg);
        compacted.extend_from_slice(recent);

        info!(
            messages_before,
            messages_after = compacted.len(),
            old_summarized = old.len(),
            recent_kept = recent.len(),
            "Chat history compacted"
        );

        // Write back using a large max to avoid re-trimming
        self.write(project_slug, agent_name, session_id, &compacted, usize::MAX)?;

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tempfile::TempDir;

    /// Simple mock provider that always returns a fixed response.
    struct MockProvider {
        response: String,
    }

    impl MockProvider {
        fn new(response: impl Into<String>) -> Self {
            Self {
                response: response.into(),
            }
        }
    }

    #[async_trait]
    impl providers::Provider for MockProvider {
        async fn chat(
            &self,
            _request: providers::ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<providers::ChatResponse> {
            Ok(providers::ChatResponse {
                text: Some(self.response.clone()),
                tool_calls: vec![],
                usage: providers::TokenUsage::default(),
            })
        }
    }

    #[test]
    fn read_returns_none_when_no_file() {
        let tmp = TempDir::new().unwrap();
        let store = ChatHistory::new(tmp.path());
        let result = store.read("test-project", "dev", Uuid::new_v4());
        assert!(result.is_none());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = ChatHistory::new(tmp.path());
        let project = "test-project";
        let agent = "dev";
        let session = Uuid::new_v4();

        let messages = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ];

        store
            .write(project, agent, session, &messages, 100)
            .unwrap();
        let loaded = store.read(project, agent, session).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, "user");
        assert_eq!(loaded[0].content, "hello");
        assert_eq!(loaded[1].role, "assistant");
        assert_eq!(loaded[1].content, "hi there");
    }

    #[test]
    fn delete_removes_file() {
        let tmp = TempDir::new().unwrap();
        let store = ChatHistory::new(tmp.path());
        let project = "test-project";
        let agent = "dev";
        let session = Uuid::new_v4();

        let messages = vec![ChatMessage::user("hello")];
        store
            .write(project, agent, session, &messages, 100)
            .unwrap();
        assert!(store.read(project, agent, session).is_some());

        let deleted = store.delete(project, agent, session).unwrap();
        assert!(deleted);
        assert!(store.read(project, agent, session).is_none());
    }

    #[test]
    fn delete_returns_false_when_no_file() {
        let tmp = TempDir::new().unwrap();
        let store = ChatHistory::new(tmp.path());
        let deleted = store
            .delete("nonexistent-project", "system", Uuid::new_v4())
            .unwrap();
        assert!(!deleted);
    }

    #[test]
    fn write_trims_to_max_turns() {
        let tmp = TempDir::new().unwrap();
        let store = ChatHistory::new(tmp.path());
        let project = "test-project";
        let agent = "architect";
        let session = Uuid::new_v4();

        // 10 turns, each with user + assistant
        let messages: Vec<ChatMessage> = (0..10)
            .flat_map(|i| {
                vec![
                    ChatMessage::user(format!("user {}", i)),
                    ChatMessage::assistant(format!("assistant {}", i)),
                ]
            })
            .collect();

        // Keep last 3 turns
        store.write(project, agent, session, &messages, 3).unwrap();
        let loaded = store.read(project, agent, session).unwrap();
        // 3 turns × 2 messages = 6 messages
        assert_eq!(loaded.len(), 6);
        assert_eq!(loaded[0].content, "user 7");
        assert_eq!(loaded[1].content, "assistant 7");
        assert_eq!(loaded[4].content, "user 9");
    }

    #[test]
    fn write_trims_preserves_tool_calls() {
        let tmp = TempDir::new().unwrap();
        let store = ChatHistory::new(tmp.path());
        let project = "test-project";
        let agent = "dev";
        let session = Uuid::new_v4();

        // 4 turns: first two are simple, last two have many tool calls
        let mut messages = vec![
            ChatMessage::user("turn 1"),
            ChatMessage::assistant("reply 1"),
            ChatMessage::user("turn 2"),
            ChatMessage::assistant("reply 2"),
        ];
        // Turn 3: user + assistant with 10 tool calls (20 messages)
        messages.push(ChatMessage::user("turn 3"));
        for i in 0..10 {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: format!("tool_call {i}"),
            });
            messages.push(ChatMessage {
                role: "tool".to_string(),
                content: format!("tool_result {i}"),
            });
        }
        messages.push(ChatMessage::assistant("final reply 3"));
        // Turn 4: simple
        messages.push(ChatMessage::user("turn 4"));
        messages.push(ChatMessage::assistant("reply 4"));

        // Keep 3 turns — should drop turn 1 but keep turns 2, 3, 4
        // (including all 20 tool messages from turn 3)
        store.write(project, agent, session, &messages, 3).unwrap();
        let loaded = store.read(project, agent, session).unwrap();

        // Turn 2 (2 msgs) + Turn 3 (1 user + 20 tool + 1 final = 22 msgs) + Turn 4 (2 msgs) = 26
        assert_eq!(loaded.len(), 26);
        assert_eq!(loaded[0].content, "turn 2");
        assert_eq!(loaded[2].content, "turn 3");
        assert_eq!(loaded[loaded.len() - 2].content, "turn 4");
    }

    #[test]
    fn session_uses_agent_name_and_session_id_in_filename() {
        let tmp = TempDir::new().unwrap();
        let store = ChatHistory::new(tmp.path());
        let project = "test-project";
        let agent = "system";
        let session = Uuid::new_v4();

        let path = store.history_path(project, agent, session);
        assert!(
            path.to_string_lossy()
                .contains(&format!("system_{}.json", session))
        );
    }

    #[test]
    fn compact_skips_when_under_threshold() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = TempDir::new().unwrap();
            let store = ChatHistory::new(tmp.path());
            let project = "test-project";
            let agent = "dev";

            // Write a small history
            let messages = vec![
                ChatMessage::user("hello"),
                ChatMessage::assistant("hi there"),
            ];
            let session = Uuid::new_v4();
            store
                .write(project, agent, session, &messages, 100)
                .unwrap();

            // Mock provider not needed since we won't reach the LLM call
            let provider = MockProvider::new("summary");
            let result = store
                .compact(
                    project,
                    agent,
                    session,
                    CompactOptions {
                        provider: &provider,
                        model: "test-model",
                        max_context_tokens: 100_000,
                        memory_profile: None,
                    },
                )
                .await
                .unwrap();
            assert!(!result, "Should not compact small history");

            // Verify history unchanged
            let loaded = store.read(project, agent, session).unwrap();
            assert_eq!(loaded.len(), 2);
        });
    }

    #[test]
    fn compact_summarizes_when_over_threshold() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = TempDir::new().unwrap();
            let store = ChatHistory::new(tmp.path());
            let project = "test-project";
            let agent = "dev";

            // Write a large history that exceeds 70% of a small context window
            // Each message ~25 chars = ~6 tokens. 20 messages = ~120 tokens.
            // With max_context_tokens=100, threshold=70 tokens → should compact.
            let mut messages = Vec::new();
            for i in 0..20 {
                messages.push(ChatMessage::user(format!("user message number {}", i)));
                messages.push(ChatMessage::assistant(format!(
                    "assistant reply number {}",
                    i
                )));
            }
            let session = Uuid::new_v4();
            store
                .write(project, agent, session, &messages, 1000)
                .unwrap();

            let provider =
                MockProvider::new("The user asked 20 questions and the assistant replied to each.");
            let result = store
                .compact(
                    project,
                    agent,
                    session,
                    CompactOptions {
                        provider: &provider,
                        model: "test-model",
                        max_context_tokens: 100,
                        memory_profile: None,
                    },
                )
                .await
                .unwrap();
            assert!(result, "Should compact large history");

            // Verify compacted: should have summary + some recent messages
            let loaded = store.read(project, agent, session).unwrap();
            assert!(
                loaded.len() < 40,
                "Should have fewer messages after compaction"
            );
            assert_eq!(loaded[0].role, "assistant");
            assert!(
                loaded[0]
                    .content
                    .starts_with("Summary of earlier conversation:")
            );
        });
    }
}
