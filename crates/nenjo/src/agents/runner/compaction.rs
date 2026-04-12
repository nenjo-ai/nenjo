use std::ops::Range;

use anyhow::Result;
use tokio::sync::mpsc;

use nenjo_models::{ChatMessage, ChatRequest, ModelProvider};

use super::types::TurnEvent;

const HISTORY_SUMMARY_MARKER: &str = "[history summary]";
const PHASE4_MIN_MESSAGES: usize = 4;
const PHASE4_MIN_TOKENS: usize = 800;
const PHASE4_MAX_CHARS: usize = 1_200;

/// Estimate total token count across all messages using the chars/4 heuristic.
pub(crate) fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(|m| m.content.len() / 4).sum()
}

pub(crate) async fn compact_messages_with_summary(
    provider: &dyn ModelProvider,
    model: &str,
    temperature: f64,
    messages: &mut Vec<ChatMessage>,
    max_tokens: usize,
    events_tx: Option<&mpsc::UnboundedSender<TurnEvent>>,
) -> Result<()> {
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

    if summarized {
        let _ = events_tx.map(|tx| {
            tx.send(TurnEvent::MessageCompacted {
                messages_before,
                messages_after: messages.len(),
            })
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
fn compact_messages(messages: &mut Vec<ChatMessage>, max_tokens: usize) {
    compact_messages_without_drop(messages, max_tokens);
    if estimate_tokens(messages) > max_tokens {
        drop_oldest_messages(messages, max_tokens);
    }
}

fn compact_messages_without_drop(messages: &mut [ChatMessage], max_tokens: usize) {
    if estimate_tokens(messages) <= max_tokens {
        return;
    }

    let len = messages.len();
    let protect_tail = 6.min(len.saturating_sub(1));
    let compactable_end = len - protect_tail;

    for i in 1..compactable_end {
        if messages[i].role != "tool" {
            continue;
        }
        if messages[i].content.len() <= 500 {
            continue;
        }
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&messages[i].content) {
            if let Some(obj) = parsed.as_object_mut()
                && let Some(content) = obj.get("content").and_then(|v| v.as_str())
            {
                let preview = truncate(content, 200);
                obj.insert(
                    "content".to_string(),
                    serde_json::Value::String(format!(
                        "{preview}\n[compacted — {} chars total]",
                        content.len()
                    )),
                );
                messages[i].content = serde_json::to_string(obj).unwrap_or_default();
            }
        } else {
            let original_len = messages[i].content.len();
            messages[i].content = format!(
                "{}\n[compacted — {original_len} chars total]",
                truncate(&messages[i].content, 200)
            );
        }

        if estimate_tokens(messages) <= max_tokens {
            return;
        }
    }

    for i in 1..compactable_end {
        if messages[i].role != "assistant" {
            continue;
        }
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&messages[i].content)
            && let Some(calls) = parsed.get("tool_calls").and_then(|v| v.as_array()).cloned()
        {
            if calls.is_empty() {
                continue;
            }
            let summarized_calls: Vec<serde_json::Value> = calls
                .into_iter()
                .map(|mut c| {
                    if let Some(obj) = c.as_object_mut() {
                        obj.insert("arguments".to_string(), serde_json::json!("{}"));
                    }
                    c
                })
                .collect();
            parsed["tool_calls"] = serde_json::Value::Array(summarized_calls);
            messages[i].content = parsed.to_string();

            if estimate_tokens(messages) <= max_tokens {
                return;
            }
        }
    }

    for i in 1..compactable_end {
        if messages[i].role != "assistant" {
            continue;
        }
        if messages[i].content.starts_with('{') {
            continue;
        }
        if messages[i].content.len() <= 600 {
            continue;
        }
        let original_len = messages[i].content.len();
        messages[i].content = format!(
            "{}\n[compacted — {original_len} chars total]",
            truncate(&messages[i].content, 300)
        );
        if estimate_tokens(messages) <= max_tokens {
            return;
        }
    }
}

fn drop_oldest_messages(messages: &mut Vec<ChatMessage>, max_tokens: usize) {
    let min_keep = 5;
    while messages.len() > min_keep && estimate_tokens(messages) > max_tokens {
        let removed = messages.remove(1);
        if removed.role == "assistant" {
            while messages.len() > min_keep && messages.get(1).is_some_and(|m| m.role == "tool") {
                messages.remove(1);
            }
        }
    }
}

fn replace_range_with_summary(
    messages: &mut Vec<ChatMessage>,
    range: Range<usize>,
    summary: ChatMessage,
) {
    messages.splice(range, [summary]);
}

fn find_phase3_candidate(messages: &[ChatMessage], max_tokens: usize) -> Option<Range<usize>> {
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
    let has_dialogue = candidate
        .iter()
        .any(|msg| matches!(msg.role.as_str(), "user" | "assistant"));
    if !has_dialogue || candidate_tokens < max_tokens / 10 {
        return None;
    }

    Some(start..end)
}

fn message_group_range(
    messages: &[ChatMessage],
    start: usize,
    compactable_end: usize,
) -> Range<usize> {
    let Some(message) = messages.get(start) else {
        return start..start;
    };

    if message.role == "assistant" && parse_assistant_tool_calls(message).is_some() {
        let mut end = start + 1;
        while end < compactable_end && messages.get(end).is_some_and(|msg| msg.role == "tool") {
            end += 1;
        }
        return start..end;
    }

    start..(start + 1).min(compactable_end)
}

fn is_summary_message(message: &ChatMessage) -> bool {
    message.role == "assistant"
        && message
            .content
            .trim_start()
            .starts_with(HISTORY_SUMMARY_MARKER)
}

async fn summarize_message_span(
    provider: &dyn ModelProvider,
    model: &str,
    temperature: f64,
    candidate: &[ChatMessage],
) -> Result<Option<ChatMessage>> {
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
        ChatMessage::system(
            "You compress old conversation context into a concise continuation summary.",
        ),
        ChatMessage::user(prompt),
    ];
    let mut response = provider
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
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

    Ok(Some(ChatMessage::assistant(summary)))
}

fn render_messages_for_summary(messages: &[ChatMessage]) -> String {
    let mut rendered = String::new();
    for message in messages {
        match message.role.as_str() {
            "assistant" => {
                if let Some((content, calls)) = parse_assistant_tool_calls(message) {
                    if !content.is_empty() {
                        rendered.push_str("assistant: ");
                        rendered.push_str(&truncate(&content, 500));
                        rendered.push('\n');
                    }
                    for call in calls {
                        rendered.push_str("assistant_tool_call: ");
                        rendered.push_str(&call.name);
                        if !call.arguments.trim().is_empty() {
                            rendered.push_str(" args=");
                            rendered.push_str(&truncate(&call.arguments, 240));
                        }
                        rendered.push('\n');
                    }
                } else {
                    rendered.push_str("assistant: ");
                    rendered.push_str(&truncate(&message.content, 700));
                    rendered.push('\n');
                }
            }
            "tool" => {
                let tool_content = parse_tool_result_content(message)
                    .unwrap_or_else(|| truncate(&message.content, 600));
                rendered.push_str("tool: ");
                rendered.push_str(&truncate(&tool_content, 600));
                rendered.push('\n');
            }
            "user" => {
                rendered.push_str("user: ");
                rendered.push_str(&truncate(&message.content, 700));
                rendered.push('\n');
            }
            role => {
                rendered.push_str(role);
                rendered.push_str(": ");
                rendered.push_str(&truncate(&message.content, 500));
                rendered.push('\n');
            }
        }
    }
    rendered
}

fn parse_assistant_tool_calls(
    message: &ChatMessage,
) -> Option<(String, Vec<nenjo_models::ToolCall>)> {
    if message.role != "assistant" {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&message.content).ok()?;
    let calls = parsed.get("tool_calls")?.as_array()?.clone();
    let calls: Vec<nenjo_models::ToolCall> = calls
        .into_iter()
        .filter_map(|call| serde_json::from_value(call).ok())
        .collect();
    let content = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    Some((content, calls))
}

fn parse_tool_result_content(message: &ChatMessage) -> Option<String> {
    if message.role != "tool" {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&message.content).ok()?;
    parsed
        .get("content")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(crate) fn truncate_old_tool_arguments(
    messages: &mut [ChatMessage],
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

    for msg in messages[1..compactable_end].iter_mut() {
        if msg.role != "assistant" {
            continue;
        }
        let mut parsed = match serde_json::from_str::<serde_json::Value>(&msg.content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let calls = match parsed.get("tool_calls").and_then(|v| v.as_array()).cloned() {
            Some(c) => c,
            None => continue,
        };

        let mut changed = false;
        let mut new_calls = Vec::new();
        for call in &calls {
            let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args_str = call.get("arguments").and_then(|a| a.as_str()).unwrap_or("");

            let truncated = truncate_tool_arguments(name, args_str);
            if truncated != args_str {
                changed = true;
            }

            let mut new_call = call.clone();
            if let Some(obj) = new_call.as_object_mut() {
                obj.insert(
                    "arguments".to_string(),
                    serde_json::Value::String(truncated),
                );
            }
            new_calls.push(new_call);
        }

        if changed && let Some(obj) = parsed.as_object_mut() {
            obj.insert(
                "tool_calls".to_string(),
                serde_json::Value::Array(new_calls),
            );
            msg.content = serde_json::to_string(obj).unwrap_or_default();
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
            "file_write" => {
                if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                    let len = content.len();
                    obj.insert(
                        "content".to_string(),
                        serde_json::Value::String(format!("«previously written — {len} chars»")),
                    );
                }
            }
            "file_edit" => {
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
    fn tool_call_assistant_message_has_structured_json() {
        let tool_calls = vec![serde_json::json!({
            "id": "call_123",
            "name": "delegate_to",
            "arguments": r#"{"agent_name":"Dev","task":"fix bug"}"#,
        })];
        let assistant_content = serde_json::json!({
            "content": "I'll delegate this.",
            "tool_calls": tool_calls,
        });
        let msg = ChatMessage::assistant(assistant_content.to_string());

        let parsed: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(parsed["content"], "I'll delegate this.");
        assert!(parsed["tool_calls"].is_array());
        assert_eq!(parsed["tool_calls"][0]["id"], "call_123");
        assert_eq!(parsed["tool_calls"][0]["name"], "delegate_to");
    }

    #[test]
    fn tool_result_message_has_tool_call_id() {
        let tool_content = serde_json::json!({
            "tool_call_id": "call_123",
            "content": "Task completed successfully",
        });
        let msg = ChatMessage::tool(tool_content.to_string());

        let parsed: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(parsed["tool_call_id"], "call_123");
        assert_eq!(parsed["content"], "Task completed successfully");
    }

    #[test]
    fn truncate_tool_arguments_small_passthrough() {
        let args = r#"{"path":"src/main.rs"}"#;
        assert_eq!(truncate_tool_arguments("file_read", args), args);
    }

    #[test]
    fn truncate_tool_arguments_file_write_replaces_content() {
        let big_content = "x".repeat(2000);
        let args = serde_json::json!({
            "path": "src/main.rs",
            "content": big_content,
        });
        let result = truncate_tool_arguments("file_write", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["path"], "src/main.rs");
        let content = parsed["content"].as_str().unwrap();
        assert!(content.contains("previously written") && content.contains("2000 chars"));
        assert!(result.len() < 200);
    }

    #[test]
    fn truncate_tool_arguments_file_edit_truncates_large_strings() {
        let big_old = "a".repeat(500);
        let big_new = "b".repeat(500);
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "old_string": big_old,
            "new_string": big_new,
        });
        let result = truncate_tool_arguments("file_edit", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["path"], "src/lib.rs");
        assert!(parsed["old_string"].as_str().unwrap().contains("500 chars"));
        assert!(parsed["new_string"].as_str().unwrap().contains("500 chars"));
    }

    #[test]
    fn truncate_tool_arguments_generic_caps_large_values() {
        let big_val = "z".repeat(1000);
        let args = serde_json::json!({ "query": big_val });
        let result = truncate_tool_arguments("content_search", &args.to_string());
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let query = parsed["query"].as_str().unwrap();
        assert!(query.contains("1000 chars") && query.contains("omitted"));
    }

    #[test]
    fn estimate_tokens_basic() {
        let msgs = vec![
            ChatMessage::system("a]".repeat(200).as_str()),
            ChatMessage::user("b".repeat(400).as_str()),
        ];
        let est = estimate_tokens(&msgs);
        assert_eq!(est, 200);
    }

    #[test]
    fn compact_messages_noop_when_under_budget() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
        ];
        let before = msgs.clone();
        compact_messages(&mut msgs, 100_000);
        assert_eq!(msgs.len(), before.len());
        assert_eq!(msgs[0].content, before[0].content);
    }

    fn build_large_conversation() -> Vec<ChatMessage> {
        let big_result = "x".repeat(4000);
        let tool_result = |id: &str, content: &str| -> ChatMessage {
            let json = serde_json::json!({
                "tool_call_id": id,
                "content": content,
            });
            ChatMessage::tool(json.to_string())
        };
        let assistant_tool_call = |id: &str, name: &str| -> ChatMessage {
            let json = serde_json::json!({
                "content": "Let me use a tool.",
                "tool_calls": [{
                    "id": id,
                    "name": name,
                    "arguments": r#"{"path":"src/main.rs"}"#,
                }],
            });
            ChatMessage::assistant(json.to_string())
        };

        vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("do task 1"),
            assistant_tool_call("c1", "file_read"),
            tool_result("c1", &big_result),
            assistant_tool_call("c2", "file_write"),
            tool_result("c2", &big_result),
            assistant_tool_call("c3", "shell"),
            tool_result("c3", &big_result),
            ChatMessage::assistant("done with old work"),
            ChatMessage::user("do task 2"),
            assistant_tool_call("c4", "file_read"),
            tool_result("c4", &big_result),
            ChatMessage::assistant("here is the result"),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("you're welcome"),
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
        assert!(msgs[3].content.contains("compacted"));
        assert!(msgs[5].content.contains("compacted"));
        assert!(!msgs[11].content.contains("compacted"));
    }

    #[test]
    fn compact_messages_phase2_summarizes_assistant_tool_calls() {
        let small_result = |id: &str| -> ChatMessage {
            let json = serde_json::json!({
                "tool_call_id": id,
                "content": "ok",
            });
            ChatMessage::tool(json.to_string())
        };
        let big_assistant = |id: &str, name: &str| -> ChatMessage {
            let big_args = "a".repeat(3000);
            let json = serde_json::json!({
                "content": "Let me use a tool.",
                "tool_calls": [{
                    "id": id,
                    "name": name,
                    "arguments": big_args,
                }],
            });
            ChatMessage::assistant(json.to_string())
        };

        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("task"),
            big_assistant("c1", "file_write"),
            small_result("c1"),
            big_assistant("c2", "shell"),
            small_result("c2"),
            big_assistant("c3", "file_read"),
            small_result("c3"),
            ChatMessage::assistant("old summary"),
            ChatMessage::user("next task"),
            big_assistant("c4", "file_read"),
            small_result("c4"),
            ChatMessage::assistant("recent result"),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("welcome"),
        ];

        let tokens_before = estimate_tokens(&msgs);
        let budget = tokens_before * 2 / 5;
        compact_messages(&mut msgs, budget);

        let has_summarized = msgs.iter().any(|m| {
            if m.role != "assistant" {
                return false;
            }
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&m.content)
                && let Some(calls) = parsed.get("tool_calls").and_then(|v| v.as_array())
            {
                return calls
                    .iter()
                    .any(|c| c.get("arguments").and_then(|a| a.as_str()) == Some("{}"));
            }
            false
        });
        assert!(has_summarized);
        assert_eq!(msgs[0].role, "system");
    }

    #[test]
    fn compact_messages_phase3_drops_oldest() {
        let mut msgs = build_large_conversation();
        compact_messages(&mut msgs, 50);

        assert_eq!(msgs[0].role, "system");
        assert!(msgs.len() >= 5);
        assert_eq!(msgs.last().unwrap().content, "you're welcome");
    }

    #[test]
    fn compact_messages_preserves_system_and_recent() {
        let mut msgs = build_large_conversation();
        let last_content = msgs.last().unwrap().content.clone();
        let system_content = msgs[0].content.clone();

        compact_messages(&mut msgs, 100);

        assert_eq!(msgs[0].content, system_content);
        assert_eq!(msgs.last().unwrap().content, last_content);
    }

    #[test]
    fn compact_messages_phase2_5_truncates_large_assistant_text() {
        let big_text = "This is a large artifact document. ".repeat(200);
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("create a PRD"),
            ChatMessage::assistant(&big_text),
            ChatMessage::user("update section 2"),
            ChatMessage::assistant(&big_text),
            ChatMessage::user("looks good"),
            ChatMessage::assistant("Great, glad you like it."),
            ChatMessage::user("any more changes?"),
            ChatMessage::assistant("No, we're done."),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("You're welcome!"),
        ];

        let tokens_before = estimate_tokens(&msgs);
        let budget = tokens_before * 2 / 5;
        compact_messages(&mut msgs, budget);

        assert!(msgs[2].content.contains("compacted"));
        assert!(msgs[4].content.contains("compacted"));
        assert!(!msgs.last().unwrap().content.contains("compacted"));
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
                usage: TokenUsage::default(),
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
            ChatMessage::system("sys"),
            ChatMessage::user(&big_user),
            ChatMessage::assistant(&big_assistant),
            ChatMessage::user(&big_user),
            ChatMessage::assistant(&big_assistant),
            ChatMessage::user("recent request"),
            ChatMessage::assistant("recent acknowledgement"),
            ChatMessage::user("recent follow-up"),
            ChatMessage::assistant("recent answer"),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("welcome"),
        ];
        let original_last = msgs.last().unwrap().content.clone();
        let provider = SummaryProvider;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let budget = estimate_tokens(&msgs) / 3;

        compact_messages_with_summary(&provider, "test-model", 0.0, &mut msgs, budget, Some(&tx))
            .await
            .unwrap();

        assert!(msgs.iter().any(is_summary_message));
        assert_eq!(msgs.last().unwrap().content, original_last);
        assert!(
            estimate_tokens(&msgs)
                < estimate_tokens(&[
                    ChatMessage::system("sys"),
                    ChatMessage::user(&big_user),
                    ChatMessage::assistant(&big_assistant),
                    ChatMessage::user(&big_user),
                    ChatMessage::assistant(&big_assistant),
                    ChatMessage::user("recent request"),
                    ChatMessage::assistant("recent acknowledgement"),
                    ChatMessage::user("recent follow-up"),
                    ChatMessage::assistant("recent answer"),
                    ChatMessage::user("thanks"),
                    ChatMessage::assistant("welcome"),
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

    fn build_conversation_near_limit(_budget: usize) -> Vec<ChatMessage> {
        let big_content = "x".repeat(2000);
        let assistant_write = |id: &str, content: &str| -> ChatMessage {
            let args = serde_json::json!({
                "path": format!("src/{id}.rs"),
                "content": content,
            });
            let json = serde_json::json!({
                "content": "",
                "tool_calls": [{
                    "id": id,
                    "name": "file_write",
                    "arguments": args.to_string(),
                }],
            });
            ChatMessage::assistant(json.to_string())
        };
        let tool_result = |id: &str| -> ChatMessage {
            let json = serde_json::json!({
                "tool_call_id": id,
                "content": "ok",
            });
            ChatMessage::tool(json.to_string())
        };

        vec![
            ChatMessage::system("sys"),
            ChatMessage::user("task"),
            assistant_write("c1", &big_content),
            tool_result("c1"),
            assistant_write("c2", &big_content),
            tool_result("c2"),
            assistant_write("c3", &big_content),
            tool_result("c3"),
            ChatMessage::assistant("recent result"),
            ChatMessage::user("thanks"),
            ChatMessage::assistant("welcome"),
        ]
    }

    #[test]
    fn truncate_old_tool_arguments_noop_when_far_from_limit() {
        let mut msgs = build_conversation_near_limit(10_000);
        let before = msgs.clone();
        truncate_old_tool_arguments(&mut msgs, 1_000_000, 60);
        assert_eq!(msgs.len(), before.len());
        assert!(msgs
            .iter()
            .zip(before.iter())
            .all(|(after, before)| after.role == before.role && after.content == before.content));
    }

    #[test]
    fn truncate_old_tool_arguments_preserves_recent_calls() {
        let mut msgs = build_conversation_near_limit(1000);
        let tokens = estimate_tokens(&msgs);
        let budget = tokens * 5 / 4;
        truncate_old_tool_arguments(&mut msgs, budget, 60);

        let recent = &msgs[6];
        let parsed: serde_json::Value = serde_json::from_str(&recent.content).unwrap();
        let args = parsed["tool_calls"][0]["arguments"].as_str().unwrap();
        assert!(args.contains("\"content\""));
        assert!(args.contains("xxxxxxxx"));
    }

    #[test]
    fn truncate_tool_arguments_file_write_preserves_path() {
        let args = serde_json::json!({
            "path": "src/main.rs",
            "content": "x".repeat(2000),
        });
        let result = truncate_tool_arguments("file_write", &args.to_string());
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
