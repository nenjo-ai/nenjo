use std::ops::Range;

use anyhow::Result;
use serde::Serialize;
use tokio::sync::mpsc;

use nenjo_models::{ChatRequest, ChatRole, ConversationMessage, ModelProvider, ToolOutputPart};

use super::types::TurnEvent;

const HISTORY_SUMMARY_MARKER: &str = "[history summary]";
const PHASE4_MIN_MESSAGES: usize = 4;
const PHASE4_MIN_TOKENS: usize = 800;
const PHASE4_MAX_CHARS: usize = 1_200;
const PAYLOAD_TOOL_RESULT_MAX_CHARS: usize = 4_000;
const PAYLOAD_MESSAGE_MAX_CHARS: usize = 8_000;

/// Estimate total token count across all messages using the chars/4 heuristic.
pub(crate) fn estimate_tokens(messages: &[ConversationMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

fn estimate_message_tokens(message: &ConversationMessage) -> usize {
    match message {
        ConversationMessage::Chat(chat) => {
            (chat.content.len() + estimate_serialized_bytes(&chat.artifacts)) / 4
        }
        ConversationMessage::AssistantToolCalls { text, tool_calls } => {
            (text.as_deref().map_or(0, str::len) + estimate_serialized_bytes(tool_calls)) / 4
        }
        ConversationMessage::ToolResults(results) => estimate_serialized_bytes(results) / 4,
        ConversationMessage::ArtifactAnalysis(analysis) => estimate_serialized_bytes(analysis) / 4,
    }
}

pub(crate) fn estimate_serialized_bytes<T>(value: &T) -> usize
where
    T: Serialize + ?Sized,
{
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

pub(crate) fn estimate_serialized_messages_bytes(messages: &[ConversationMessage]) -> usize {
    let serialized = estimate_serialized_bytes(messages);
    if serialized > 0 {
        return serialized;
    }

    messages.iter().map(estimate_serialized_bytes).sum()
}

pub(crate) async fn compact_messages_with_summary<P>(
    provider: &P,
    model: &str,
    temperature: f64,
    messages: &mut Vec<ConversationMessage>,
    max_tokens: usize,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
) -> Result<()>
where
    P: ModelProvider + ?Sized,
{
    let messages_before = messages.len();

    compact_messages_without_drop(messages, max_tokens);

    let summarized = if estimate_tokens(messages) > max_tokens {
        if let Some(candidate) = find_phase3_candidate(messages, max_tokens) {
            if let Some(summary) =
                summarize_message_span(provider, model, temperature, &messages[candidate.clone()])
                    .await?
            {
                let candidate_tokens = estimate_tokens(&messages[candidate.clone()]);
                let summary_tokens = estimate_tokens(std::slice::from_ref(&summary));
                if summary_tokens * 5 <= candidate_tokens * 4 {
                    replace_range_with_summary(messages, candidate, summary);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    if estimate_tokens(messages) > max_tokens {
        drop_oldest_messages(messages, max_tokens);
    }

    if summarized && let Some(tx) = events_tx {
        let _ = tx.send(TurnEvent::MessageCompacted {
            messages_before,
            messages_after: messages.len(),
        });
    }

    Ok(())
}

/// Progressively compact conversation messages to stay within a token budget.
///
/// Strategy (preserves recent context, compacts old):
/// 1. If under budget, no-op.
/// 2. Phase 1: Truncate old tool-result content (oldest first, skip recent 6).
/// 3. Phase 2: Summarize old assistant tool-call arguments to just tool names.
/// 4. Phase 2.5: Truncate large plain-text assistant messages (artifact content).
/// 5. Phase 3: Summarize old completed turn groups into one assistant summary.
/// 6. Phase 4: Drop oldest non-system messages until under budget (keep last 4).
#[cfg(test)]
fn compact_messages(messages: &mut Vec<ConversationMessage>, max_tokens: usize) {
    compact_messages_without_drop(messages, max_tokens);
    if estimate_tokens(messages) > max_tokens {
        drop_oldest_messages(messages, max_tokens);
    }
}

fn compact_messages_without_drop(messages: &mut [ConversationMessage], max_tokens: usize) {
    if estimate_tokens(messages) <= max_tokens {
        return;
    }

    let len = messages.len();
    let protect_tail = 6.min(len.saturating_sub(1));
    let compactable_end = len - protect_tail;

    for i in 1..compactable_end {
        let ConversationMessage::ToolResults(results) = &mut messages[i] else {
            continue;
        };
        compact_tool_results(results, 200, "compacted");

        if estimate_tokens(messages) <= max_tokens {
            return;
        }
    }

    for i in 1..compactable_end {
        let ConversationMessage::AssistantToolCalls { tool_calls, .. } = &mut messages[i] else {
            continue;
        };
        for call in tool_calls {
            call.arguments = "{}".to_string();
        }
        if estimate_tokens(messages) <= max_tokens {
            return;
        }
    }

    for i in 1..compactable_end {
        let Some(chat) = messages[i].as_chat_mut() else {
            continue;
        };
        if chat.role != ChatRole::Assistant || chat.content.len() <= 600 {
            continue;
        }
        let original_len = chat.content.len();
        chat.content = format!(
            "{}\n[compacted — {original_len} chars total]",
            truncate(&chat.content, 300)
        );
        if estimate_tokens(messages) <= max_tokens {
            return;
        }
    }
}

fn drop_oldest_messages(messages: &mut Vec<ConversationMessage>, max_tokens: usize) {
    let min_keep = 5;
    while messages.len() > min_keep && estimate_tokens(messages) > max_tokens {
        let group = message_group_range(messages, 1, messages.len());
        if messages[group.clone()]
            .iter()
            .any(ConversationMessage::has_artifact_references)
        {
            // Artifact inputs are durable user/tool intent. Leave the request
            // oversized so the explicit input-preparation guard reports it;
            // silently dropping a reference would change the conversation.
            break;
        }
        let removed = messages.remove(1);
        if matches!(removed, ConversationMessage::AssistantToolCalls { .. }) {
            while messages.len() > min_keep
                && messages
                    .get(1)
                    .is_some_and(|message| matches!(message, ConversationMessage::ToolResults(_)))
            {
                messages.remove(1);
            }
        }
    }
}

pub(crate) fn compact_messages_for_payload(
    messages: &mut [ConversationMessage],
    max_payload_bytes: usize,
) -> bool {
    let original_size = estimate_serialized_messages_bytes(messages);
    if original_size <= max_payload_bytes {
        return false;
    }

    let latest_user_index = messages
        .iter()
        .rposition(|message| message.is_role(ChatRole::User));

    for index in (1..messages.len()).rev() {
        if !matches!(messages[index], ConversationMessage::ToolResults(_)) {
            continue;
        }
        compact_tool_result_message(&mut messages[index], PAYLOAD_TOOL_RESULT_MAX_CHARS);
        if estimate_serialized_messages_bytes(messages) <= max_payload_bytes {
            return true;
        }
    }

    for index in 1..messages.len() {
        if !matches!(
            messages[index],
            ConversationMessage::AssistantToolCalls { .. }
        ) {
            continue;
        }
        if compact_assistant_tool_arguments(&mut messages[index])
            && estimate_serialized_messages_bytes(messages) <= max_payload_bytes
        {
            return true;
        }
    }

    for index in (1..messages.len()).rev() {
        if messages[index].is_role(ChatRole::System) || latest_user_index == Some(index) {
            continue;
        }
        compact_payload_message(&mut messages[index], PAYLOAD_MESSAGE_MAX_CHARS);
        if estimate_serialized_messages_bytes(messages) <= max_payload_bytes {
            return true;
        }
    }

    for index in (1..messages.len()).rev() {
        if messages[index].is_role(ChatRole::System) {
            continue;
        }
        compact_payload_message(&mut messages[index], PAYLOAD_MESSAGE_MAX_CHARS);
        if estimate_serialized_messages_bytes(messages) <= max_payload_bytes {
            return true;
        }
    }

    estimate_serialized_messages_bytes(messages) < original_size
}

fn compact_assistant_tool_arguments(message: &mut ConversationMessage) -> bool {
    let ConversationMessage::AssistantToolCalls { tool_calls, .. } = message else {
        return false;
    };
    let mut changed = false;
    for call in tool_calls {
        let compacted = truncate_tool_arguments(&call.name, &call.arguments);
        if compacted != call.arguments {
            call.arguments = compacted;
            changed = true;
        }
    }
    changed
}

fn compact_tool_result_message(message: &mut ConversationMessage, max_content_chars: usize) {
    let ConversationMessage::ToolResults(results) = message else {
        return;
    };
    compact_tool_results(results, max_content_chars, "payload compacted");
}

fn compact_payload_message(message: &mut ConversationMessage, max_content_chars: usize) {
    match message {
        ConversationMessage::Chat(chat) => {
            compact_text(&mut chat.content, max_content_chars, "payload compacted")
        }
        ConversationMessage::AssistantToolCalls { text, .. } => {
            if let Some(text) = text {
                compact_text(text, max_content_chars, "payload compacted");
            }
        }
        ConversationMessage::ToolResults(results) => {
            compact_tool_results(results, max_content_chars, "payload compacted");
        }
        ConversationMessage::ArtifactAnalysis(analysis) => {
            compact_text(&mut analysis.text, max_content_chars, "payload compacted");
        }
    }
}

fn compact_tool_results(
    results: &mut [nenjo_models::ToolResultMessage],
    max_content_chars: usize,
    marker: &str,
) {
    for result in results {
        for part in result.output.parts_mut() {
            if let ToolOutputPart::Text(text) = part {
                compact_text(text, max_content_chars, marker);
            }
        }
    }
}

fn compact_text(text: &mut String, max_content_chars: usize, marker: &str) {
    if text.len() <= max_content_chars {
        return;
    }
    let original_len = text.len();
    *text = format!(
        "{}\n[{marker} — {original_len} chars total]",
        truncate(text, max_content_chars)
    );
}

fn replace_range_with_summary(
    messages: &mut Vec<ConversationMessage>,
    range: Range<usize>,
    summary: ConversationMessage,
) {
    messages.splice(range, [summary]);
}

fn find_phase3_candidate(
    messages: &[ConversationMessage],
    max_tokens: usize,
) -> Option<Range<usize>> {
    if messages.len() < 8 {
        return None;
    }

    let len = messages.len();
    let protect_tail = 6.min(len.saturating_sub(1));
    let compactable_end = len.saturating_sub(protect_tail);
    if compactable_end <= 1 {
        return None;
    }

    let max_candidate_end = (1 + (len.saturating_sub(1) * 3 / 5)).min(compactable_end);
    let mut start = 1;
    while start < compactable_end && is_summary_message(&messages[start]) {
        start += 1;
    }
    if start >= compactable_end {
        return None;
    }

    let mut end = start;
    let mut included_tokens = 0;
    while end < max_candidate_end {
        if is_summary_message(&messages[end]) {
            break;
        }
        let group = message_group_range(messages, end, compactable_end);
        included_tokens += estimate_tokens(&messages[group.clone()]);
        end = group.end;

        if end - start >= PHASE4_MIN_MESSAGES && included_tokens >= PHASE4_MIN_TOKENS {
            break;
        }
    }

    if end <= start {
        return None;
    }

    let candidate = &messages[start..end];
    let candidate_tokens = estimate_tokens(candidate);
    let has_dialogue = candidate.iter().any(|message| {
        message.is_role(ChatRole::User)
            || message.is_role(ChatRole::Assistant)
            || matches!(message, ConversationMessage::AssistantToolCalls { .. })
    });
    if !has_dialogue || candidate_tokens < max_tokens / 10 {
        return None;
    }

    Some(start..end)
}

fn message_group_range(
    messages: &[ConversationMessage],
    start: usize,
    compactable_end: usize,
) -> Range<usize> {
    let Some(message) = messages.get(start) else {
        return start..start;
    };

    if matches!(message, ConversationMessage::AssistantToolCalls { .. }) {
        let mut end = start + 1;
        while end < compactable_end
            && messages
                .get(end)
                .is_some_and(|message| matches!(message, ConversationMessage::ToolResults(_)))
        {
            end += 1;
        }
        return start..end;
    }

    start..(start + 1).min(compactable_end)
}

fn is_summary_message(message: &ConversationMessage) -> bool {
    matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.role == ChatRole::Assistant
                && chat.content.trim_start().starts_with(HISTORY_SUMMARY_MARKER)
    )
}

async fn summarize_message_span<P>(
    provider: &P,
    model: &str,
    temperature: f64,
    candidate: &[ConversationMessage],
) -> Result<Option<ConversationMessage>>
where
    P: ModelProvider + ?Sized,
{
    if candidate.is_empty() {
        return Ok(None);
    }

    let rendered = render_messages_for_summary(candidate);
    if rendered.trim().is_empty() {
        return Ok(None);
    }

    let prompt = format!(
        "Summarize these older conversation turns for future continuation.\n\
Preserve:\n\
- user requests and intent\n\
- decisions and conclusions\n\
- important tool calls and results\n\
- files, branches, artifacts, or paths that matter\n\
- unresolved work or constraints\n\
\n\
Do not include filler, repeated reasoning, or chain-of-thought.\n\
Output plain text beginning with \"{HISTORY_SUMMARY_MARKER}\".\n\
Keep the answer under {PHASE4_MAX_CHARS} characters.\n\
\n\
Conversation:\n{rendered}"
    );
    let messages = vec![
        ConversationMessage::system(
            "You compress old conversation context into a concise continuation summary.",
        ),
        ConversationMessage::user(prompt),
    ];
    let mut response = provider
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                native_tools: None,
                prepared_artifacts: None,
            },
            model,
            temperature,
        )
        .await?;

    if !response.tool_calls.is_empty() {
        return Ok(None);
    }

    let Some(text) = response.text.take() else {
        return Ok(None);
    };
    let summary = nenjo_models::strip_thinking(&text);
    if summary.trim().is_empty() {
        return Ok(None);
    }

    let summary = if summary.trim_start().starts_with(HISTORY_SUMMARY_MARKER) {
        summary
    } else {
        format!("{HISTORY_SUMMARY_MARKER}\n{}", summary.trim())
    };

    if summary.chars().count() > PHASE4_MAX_CHARS {
        return Ok(None);
    }

    Ok(Some(ConversationMessage::assistant(summary)))
}

fn render_messages_for_summary(messages: &[ConversationMessage]) -> String {
    let mut rendered = String::new();
    for message in messages {
        match message {
            ConversationMessage::AssistantToolCalls { text, tool_calls } => {
                if let Some(content) = text.as_deref().filter(|text| !text.is_empty()) {
                    rendered.push_str("assistant: ");
                    rendered.push_str(&truncate(content, 500));
                    rendered.push('\n');
                }
                for call in tool_calls {
                    rendered.push_str("assistant_tool_call: ");
                    rendered.push_str(&call.name);
                    if !call.arguments.trim().is_empty() {
                        rendered.push_str(" args=");
                        rendered.push_str(&truncate(&call.arguments, 240));
                    }
                    rendered.push('\n');
                }
            }
            ConversationMessage::ToolResults(results) => {
                for result in results {
                    rendered.push_str("tool_result: id=");
                    rendered.push_str(&result.tool_call_id);
                    rendered.push_str(" content=");
                    rendered.push_str(&truncate(&result.output.text_content(), 600));
                    rendered.push('\n');
                    render_tool_artifact_markers(&result.output, &mut rendered);
                }
            }
            ConversationMessage::Chat(chat) => {
                rendered.push_str(chat.role.as_str());
                rendered.push_str(": ");
                let max_chars = if chat.role == ChatRole::User {
                    700
                } else {
                    500
                };
                rendered.push_str(&truncate(&chat.content, max_chars));
                rendered.push('\n');
                render_artifact_markers(chat, &mut rendered);
            }
            ConversationMessage::ArtifactAnalysis(analysis) => {
                rendered.push_str("artifact_analysis: ");
                rendered.push_str(&truncate(&analysis.model_context(), 700));
                rendered.push('\n');
            }
        }
    }
    rendered
}

fn render_artifact_markers(message: &nenjo_models::ChatMessage, rendered: &mut String) {
    for input in &message.artifacts {
        let artifact = input.artifact();
        rendered.push_str("artifact_ref: id=");
        rendered.push_str(&artifact.id().to_string());
        rendered.push_str(" digest=");
        rendered.push_str(artifact.digest().as_str());
        rendered.push_str(" media_type=");
        rendered.push_str(artifact.media_type().essence_str());
        rendered.push_str(" bytes=");
        rendered.push_str(&artifact.size().bytes().to_string());
        rendered.push_str(" source=");
        rendered.push_str(match input.source() {
            nenjo_models::ArtifactInputSource::UserAttachment => "user_attachment",
            nenjo_models::ArtifactInputSource::TaskInput => "task_input",
            nenjo_models::ArtifactInputSource::ToolResult => "tool_result",
            nenjo_models::ArtifactInputSource::SessionContext => "session_context",
        });
        if let Some(instruction) = input.instruction() {
            rendered.push_str(" instruction=");
            rendered.push_str(&truncate(instruction.as_str(), 240));
        }
        rendered.push('\n');
    }
}

fn render_tool_artifact_markers(output: &nenjo_models::ToolOutput, rendered: &mut String) {
    for part in output.parts() {
        let ToolOutputPart::Artifact(artifact) = part else {
            continue;
        };
        rendered.push_str("artifact_ref: id=");
        rendered.push_str(&artifact.id().to_string());
        rendered.push_str(" digest=");
        rendered.push_str(artifact.digest().as_str());
        rendered.push_str(" media_type=");
        rendered.push_str(artifact.media_type().essence_str());
        rendered.push_str(" bytes=");
        rendered.push_str(&artifact.size().bytes().to_string());
        rendered.push_str(" source=tool_result\n");
    }
}

pub(crate) fn truncate_old_tool_arguments(
    messages: &mut [ConversationMessage],
    max_tokens: usize,
    trigger_percent: u8,
) {
    let trigger_percent = trigger_percent.clamp(1, 100) as usize;
    let threshold = max_tokens * trigger_percent / 100;
    if estimate_tokens(messages) < threshold {
        return;
    }

    const PROTECT_TAIL: usize = 12;

    let len = messages.len();
    let protect_tail = PROTECT_TAIL.min(len.saturating_sub(1));
    let compactable_end = len - protect_tail;

    for message in messages[1..compactable_end].iter_mut() {
        let ConversationMessage::AssistantToolCalls { tool_calls, .. } = message else {
            continue;
        };
        for call in tool_calls {
            call.arguments = truncate_tool_arguments(&call.name, &call.arguments);
        }
    }
}

fn truncate_tool_arguments(tool_name: &str, arguments: &str) -> String {
    const MAX_ARG_LEN: usize = 500;

    if arguments.len() <= MAX_ARG_LEN {
        return arguments.to_string();
    }

    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(obj) = parsed.as_object_mut()
    {
        match tool_name {
            "write" | "file_write" => {
                if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                    let len = content.len();
                    obj.insert(
                        "content".to_string(),
                        serde_json::Value::String(format!("«previously written — {len} chars»")),
                    );
                }
            }
            "edit" | "file_edit" => {
                for key in &["old_string", "new_string"] {
                    if let Some(val) = obj.get(*key).and_then(|v| v.as_str())
                        && val.len() > 200
                    {
                        let preview = truncate(val, 100);
                        obj.insert(
                            key.to_string(),
                            serde_json::Value::String(format!("«{} chars» {preview}", val.len())),
                        );
                    }
                }
            }
            "shell" => {
                if let Some(cmd) = obj.get("command").and_then(|v| v.as_str())
                    && cmd.len() > 300
                {
                    obj.insert(
                        "command".to_string(),
                        serde_json::Value::String(truncate(cmd, 300)),
                    );
                }
            }
            _ => {
                let keys: Vec<String> = obj.keys().cloned().collect();
                for key in keys {
                    if let Some(val) = obj.get(&key).and_then(|v| v.as_str())
                        && val.len() > 300
                    {
                        obj.insert(
                            key,
                            serde_json::Value::String(format!("«{} chars omitted»", val.len())),
                        );
                    }
                }
            }
        }
        return serde_json::to_string(obj).unwrap_or_else(|_| truncate(arguments, MAX_ARG_LEN));
    }

    truncate(arguments, MAX_ARG_LEN)
}

pub(crate) fn truncate_str(s: &str, max_bytes: usize) -> &str {
    &s[..s.floor_char_boundary(max_bytes)]
}

pub(crate) fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    if max_len <= 3 {
        return truncate_str(s, max_len).to_string();
    }
    format!("{}...", truncate_str(s, max_len.saturating_sub(3)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo_models::{
        ArtifactId, ArtifactInput, ArtifactInputSource, ArtifactRef, ArtifactSize, MediaType,
        Sha256Digest, ToolCall, ToolResultMessage,
    };
    use nenjo_models::{ChatResponse, TokenUsage};

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let max_len = 10;
        let result = truncate("hello world this is a long string", max_len);
        assert!(result.ends_with("..."));
        assert_eq!(result.len(), max_len);
    }

    #[test]
    fn tool_call_assistant_message_is_semantic() {
        let msg = assistant_tool_call(
            "call_123",
            "spawn_sub_agents",
            r#"{"agents":[{"agent":"Dev"}]}"#,
        );

        let ConversationMessage::AssistantToolCalls { text, tool_calls } = msg else {
            panic!("expected assistant tool calls");
        };
        assert_eq!(text.as_deref(), Some("Let me use a tool."));
        assert_eq!(tool_calls[0].id, "call_123");
        assert_eq!(tool_calls[0].name, "spawn_sub_agents");
    }

    #[test]
    fn tool_result_message_has_tool_call_id() {
        let msg = tool_result("call_123", "Task completed successfully");
        let ConversationMessage::ToolResults(results) = msg else {
            panic!("expected tool results");
        };
        assert_eq!(results[0].tool_call_id, "call_123");
        assert_eq!(results[0].output, "Task completed successfully");
    }

    #[test]
    fn truncate_tool_arguments_small_passthrough() {
        let args = r#"{"path":"src/main.rs"}"#;
        assert_eq!(truncate_tool_arguments("read", args), args);
    }

    #[test]
    fn truncate_tool_arguments_write_replaces_content() {
        let big_content = "x".repeat(2000);
        let args = serde_json::json!({
            "path": "src/main.rs",
            "content": big_content,
        });
        let result = truncate_tool_arguments("write", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["path"], "src/main.rs");
        let content = parsed["content"].as_str().unwrap();
        assert!(content.contains("previously written") && content.contains("2000 chars"));
        assert!(result.len() < 200);
    }

    #[test]
    fn truncate_tool_arguments_edit_truncates_large_strings() {
        let big_old = "a".repeat(500);
        let big_new = "b".repeat(500);
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "old_string": big_old,
            "new_string": big_new,
        });
        let result = truncate_tool_arguments("edit", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["path"], "src/lib.rs");
        assert!(parsed["old_string"].as_str().unwrap().contains("500 chars"));
        assert!(parsed["new_string"].as_str().unwrap().contains("500 chars"));
    }

    #[test]
    fn truncate_tool_arguments_generic_caps_large_values() {
        let big_val = "z".repeat(1000);
        let args = serde_json::json!({ "query": big_val });
        let result = truncate_tool_arguments("search", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let query = parsed["query"].as_str().unwrap();
        assert!(query.contains("1000 chars") && query.contains("omitted"));
    }

    #[test]
    fn estimate_tokens_basic() {
        let msgs = vec![
            ConversationMessage::system("a]".repeat(200).as_str()),
            ConversationMessage::user("b".repeat(400).as_str()),
        ];
        let est = estimate_tokens(&msgs);
        assert_eq!(est, 200);
    }

    fn artifact_input() -> ArtifactInput {
        ArtifactInput::new(
            ArtifactRef::new(
                ArtifactId::parse(uuid::Uuid::new_v4()).unwrap(),
                Sha256Digest::parse(&format!("sha256:{}", "f".repeat(64))).unwrap(),
                MediaType::parse("image/png").unwrap(),
                ArtifactSize::new(42),
            ),
            ArtifactInputSource::ToolResult,
        )
    }

    fn artifact_ref() -> ArtifactRef {
        artifact_input().artifact().clone()
    }

    fn tool_result(id: &str, content: impl Into<String>) -> ConversationMessage {
        ConversationMessage::tool_result(ToolResultMessage::text(id, content))
    }

    fn assistant_tool_call(
        id: &str,
        name: &str,
        arguments: impl Into<String>,
    ) -> ConversationMessage {
        ConversationMessage::assistant_tool_calls(
            Some("Let me use a tool.".to_string()),
            vec![ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: arguments.into(),
            }],
        )
    }

    fn chat_content(message: &ConversationMessage) -> &str {
        &message.as_chat().expect("expected chat message").content
    }

    fn tool_text(message: &ConversationMessage) -> String {
        let ConversationMessage::ToolResults(results) = message else {
            panic!("expected tool results");
        };
        results
            .iter()
            .map(|result| result.output.text_content())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn payload_compaction_preserves_artifact_references() {
        let artifact = artifact_ref();
        let mut messages = vec![
            ConversationMessage::system("system"),
            ConversationMessage::tool_result(
                ToolResultMessage::text("call_1", "x".repeat(10_000))
                    .with_artifact(artifact.clone()),
            ),
        ];

        assert!(compact_messages_for_payload(&mut messages, 2_000));
        let ConversationMessage::ToolResults(results) = &messages[1] else {
            panic!("expected tool result");
        };
        assert!(results[0].output.contains("payload compacted"));
        assert!(
            results[0]
                .output
                .parts()
                .iter()
                .any(|part| matches!(part, ToolOutputPart::Artifact(found) if found == &artifact))
        );
    }

    #[test]
    fn summary_render_uses_artifact_metadata_not_bytes() {
        let rendered = render_messages_for_summary(&[ConversationMessage::tool_result(
            ToolResultMessage::text("call_1", "result").with_artifact(artifact_ref()),
        )]);

        assert!(rendered.contains("artifact_ref: id="));
        assert!(rendered.contains("media_type=image/png"));
        assert!(!rendered.contains("base64"));
    }

    #[test]
    fn compact_messages_noop_when_under_budget() {
        let mut msgs = vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user("hi"),
            ConversationMessage::assistant("hello"),
        ];
        let before = msgs.clone();
        compact_messages(&mut msgs, 100_000);
        assert_eq!(msgs.len(), before.len());
        assert_eq!(msgs, before);
    }

    fn build_large_conversation() -> Vec<ConversationMessage> {
        let big_result = "x".repeat(4000);
        vec![
            ConversationMessage::system("system prompt"),
            ConversationMessage::user("do task 1"),
            assistant_tool_call("c1", "read", r#"{"path":"src/main.rs"}"#),
            tool_result("c1", &big_result),
            assistant_tool_call("c2", "file_write", r#"{"path":"src/main.rs"}"#),
            tool_result("c2", &big_result),
            assistant_tool_call("c3", "shell", r#"{"path":"src/main.rs"}"#),
            tool_result("c3", &big_result),
            ConversationMessage::assistant("done with old work"),
            ConversationMessage::user("do task 2"),
            assistant_tool_call("c4", "read", r#"{"path":"src/main.rs"}"#),
            tool_result("c4", &big_result),
            ConversationMessage::assistant("here is the result"),
            ConversationMessage::user("thanks"),
            ConversationMessage::assistant("you're welcome"),
        ]
    }

    #[test]
    fn compact_messages_phase1_truncates_old_tool_results() {
        let mut msgs = build_large_conversation();
        let original_len = msgs.len();
        let tokens_before = estimate_tokens(&msgs);
        let budget = tokens_before * 3 / 5;
        compact_messages(&mut msgs, budget);

        assert_eq!(msgs.len(), original_len);
        assert!(tool_text(&msgs[3]).contains("compacted"));
        assert!(tool_text(&msgs[5]).contains("compacted"));
        assert!(!tool_text(&msgs[11]).contains("compacted"));
    }

    #[test]
    fn compact_messages_phase2_summarizes_assistant_tool_calls() {
        let small_result = |id: &str| tool_result(id, "ok");
        let big_assistant = |id: &str, name: &str| -> ConversationMessage {
            let big_args = "a".repeat(3000);
            assistant_tool_call(id, name, big_args)
        };

        let mut msgs = vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user("task"),
            big_assistant("c1", "file_write"),
            small_result("c1"),
            big_assistant("c2", "shell"),
            small_result("c2"),
            big_assistant("c3", "read"),
            small_result("c3"),
            ConversationMessage::assistant("old summary"),
            ConversationMessage::user("next task"),
            big_assistant("c4", "read"),
            small_result("c4"),
            ConversationMessage::assistant("recent result"),
            ConversationMessage::user("thanks"),
            ConversationMessage::assistant("welcome"),
        ];

        let tokens_before = estimate_tokens(&msgs);
        let budget = tokens_before * 2 / 5;
        compact_messages(&mut msgs, budget);

        let has_summarized = msgs.iter().any(|message| {
            matches!(
                message,
                ConversationMessage::AssistantToolCalls { tool_calls, .. }
                    if tool_calls.iter().any(|call| call.arguments == "{}")
            )
        });
        assert!(has_summarized);
        assert!(msgs[0].is_role(ChatRole::System));
    }

    #[test]
    fn compact_messages_phase3_drops_oldest() {
        let mut msgs = build_large_conversation();
        compact_messages(&mut msgs, 50);

        assert!(msgs[0].is_role(ChatRole::System));
        assert!(msgs.len() >= 5);
        assert_eq!(chat_content(msgs.last().unwrap()), "you're welcome");
    }

    #[test]
    fn compact_messages_preserves_system_and_recent() {
        let mut msgs = build_large_conversation();
        let last_content = msgs.last().cloned().unwrap();
        let system_content = msgs[0].clone();

        compact_messages(&mut msgs, 100);

        assert_eq!(msgs[0], system_content);
        assert_eq!(msgs.last(), Some(&last_content));
    }

    #[test]
    fn compact_messages_for_payload_truncates_recent_tool_results() {
        let huge_result = "x".repeat(600_000);
        let mut msgs = vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user("old request"),
            ConversationMessage::assistant("old answer"),
            ConversationMessage::user("older request"),
            ConversationMessage::assistant("older answer"),
            assistant_tool_call("c1", "shell", r#"{"cmd":"large output"}"#),
            tool_result("c1", &huge_result),
            assistant_tool_call("c2", "read", r#"{"cmd":"large output"}"#),
            tool_result("c2", &huge_result),
            ConversationMessage::user("continue"),
        ];

        let budget = estimate_serialized_messages_bytes(&msgs) / 3;
        assert!(compact_messages_for_payload(&mut msgs, budget));

        assert!(estimate_serialized_messages_bytes(&msgs) <= budget);
        assert!(msgs.iter().any(|message| matches!(message,
            ConversationMessage::ToolResults(_)
                if tool_text(message).contains("payload compacted")
        )));
    }

    #[test]
    fn compact_messages_does_not_payload_compact_for_token_budget() {
        let huge_result = "x".repeat(600_000);
        let mut msgs = vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user("old request"),
            ConversationMessage::assistant("old answer"),
            tool_result("c1", huge_result),
            ConversationMessage::user("continue"),
        ];

        compact_messages(&mut msgs, 20_000);

        assert!(!msgs.iter().any(|message| {
            match message {
                ConversationMessage::Chat(chat) => chat.content.contains("payload compacted"),
                ConversationMessage::AssistantToolCalls { text, .. } => text
                    .as_deref()
                    .is_some_and(|text| text.contains("payload compacted")),
                ConversationMessage::ToolResults(_) => {
                    tool_text(message).contains("payload compacted")
                }
                ConversationMessage::ArtifactAnalysis(analysis) => {
                    analysis.text.contains("payload compacted")
                }
            }
        }));
    }

    #[test]
    fn compact_messages_phase2_5_truncates_large_assistant_text() {
        let big_text = "This is a large artifact document. ".repeat(200);
        let mut msgs = vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user("create a PRD"),
            ConversationMessage::assistant(&big_text),
            ConversationMessage::user("update section 2"),
            ConversationMessage::assistant(&big_text),
            ConversationMessage::user("looks good"),
            ConversationMessage::assistant("Great, glad you like it."),
            ConversationMessage::user("any more changes?"),
            ConversationMessage::assistant("No, we're done."),
            ConversationMessage::user("thanks"),
            ConversationMessage::assistant("You're welcome!"),
        ];

        let tokens_before = estimate_tokens(&msgs);
        let budget = tokens_before * 2 / 5;
        compact_messages(&mut msgs, budget);

        assert!(chat_content(&msgs[2]).contains("compacted"));
        assert!(chat_content(&msgs[4]).contains("compacted"));
        assert!(!chat_content(msgs.last().unwrap()).contains("compacted"));
    }

    #[test]
    fn phase3_candidate_keeps_tool_call_groups_intact() {
        let msgs = build_large_conversation();
        let range = find_phase3_candidate(&msgs, estimate_tokens(&msgs) / 4).unwrap();
        assert!(range.contains(&2));
        assert!(range.contains(&3));
    }

    struct SummaryProvider;

    #[async_trait::async_trait]
    impl ModelProvider for SummaryProvider {
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some(format!(
                    "{HISTORY_SUMMARY_MARKER}\n- user asked for the old work to be completed\n- tools already produced the needed changes\n- continue from the latest turn"
                )),
                tool_calls: Vec::new(),
                provider_tool_calls: vec![],
                usage: TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::Stop,
            })
        }

        fn context_window(&self, _model: &str) -> Option<usize> {
            Some(8_000)
        }
    }

    #[tokio::test]
    async fn compact_messages_with_summary_inserts_summary_marker() {
        let big_user = "Need a full migration plan. ".repeat(220);
        let big_assistant =
            "I reviewed the repository and drafted the migration plan. ".repeat(180);
        let mut msgs = vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user(&big_user),
            ConversationMessage::assistant(&big_assistant),
            ConversationMessage::user(&big_user),
            ConversationMessage::assistant(&big_assistant),
            ConversationMessage::user("recent request"),
            ConversationMessage::assistant("recent acknowledgement"),
            ConversationMessage::user("recent follow-up"),
            ConversationMessage::assistant("recent answer"),
            ConversationMessage::user("thanks"),
            ConversationMessage::assistant("welcome"),
        ];
        let original_last = msgs.last().cloned().unwrap();
        let provider = SummaryProvider;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let budget = estimate_tokens(&msgs) / 3;

        compact_messages_with_summary(&provider, "test-model", 0.0, &mut msgs, budget, Some(&tx))
            .await
            .unwrap();

        assert!(msgs.iter().any(is_summary_message));
        assert_eq!(msgs.last(), Some(&original_last));
        assert!(
            estimate_tokens(&msgs)
                < estimate_tokens(&[
                    ConversationMessage::system("sys"),
                    ConversationMessage::user(&big_user),
                    ConversationMessage::assistant(&big_assistant),
                    ConversationMessage::user(&big_user),
                    ConversationMessage::assistant(&big_assistant),
                    ConversationMessage::user("recent request"),
                    ConversationMessage::assistant("recent acknowledgement"),
                    ConversationMessage::user("recent follow-up"),
                    ConversationMessage::assistant("recent answer"),
                    ConversationMessage::user("thanks"),
                    ConversationMessage::assistant("welcome"),
                ])
        );

        let event = rx.recv().await.expect("message compacted event");
        match event {
            TurnEvent::MessageCompacted {
                messages_before,
                messages_after,
            } => assert!(messages_after < messages_before),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    fn build_conversation_near_limit(_budget: usize) -> Vec<ConversationMessage> {
        let big_content = "x".repeat(2000);
        let assistant_write = |id: &str, content: &str| -> ConversationMessage {
            let args = serde_json::json!({
                "path": format!("src/{id}.rs"),
                "content": content,
            });
            assistant_tool_call(id, "file_write", args.to_string())
        };

        vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user("task"),
            assistant_write("c1", &big_content),
            tool_result("c1", "ok"),
            assistant_write("c2", &big_content),
            tool_result("c2", "ok"),
            assistant_write("c3", &big_content),
            tool_result("c3", "ok"),
            ConversationMessage::assistant("recent result"),
            ConversationMessage::user("thanks"),
            ConversationMessage::assistant("welcome"),
        ]
    }

    #[test]
    fn truncate_old_tool_arguments_noop_when_far_from_limit() {
        let mut msgs = build_conversation_near_limit(10_000);
        let before = msgs.clone();
        truncate_old_tool_arguments(&mut msgs, 1_000_000, 60);
        assert_eq!(msgs.len(), before.len());
        assert_eq!(msgs, before);
    }

    #[test]
    fn truncate_old_tool_arguments_preserves_recent_calls() {
        let mut msgs = build_conversation_near_limit(1000);
        let tokens = estimate_tokens(&msgs);
        let budget = tokens * 5 / 4;
        truncate_old_tool_arguments(&mut msgs, budget, 60);

        let ConversationMessage::AssistantToolCalls { tool_calls, .. } = &msgs[6] else {
            panic!("expected recent assistant tool call");
        };
        let args = &tool_calls[0].arguments;
        assert!(args.contains("\"content\""));
        assert!(args.contains("xxxxxxxx"));
    }

    #[test]
    fn truncate_tool_arguments_write_preserves_path() {
        let args = serde_json::json!({
            "path": "src/main.rs",
            "content": "x".repeat(2000),
        });
        let result = truncate_tool_arguments("write", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["path"], "src/main.rs");
        assert!(
            parsed["content"]
                .as_str()
                .unwrap()
                .contains("previously written")
        );
    }
}
