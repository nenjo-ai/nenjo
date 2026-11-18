//! Read-only agent tools for browsing prior sessions and transcripts.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use nenjo_sessions::{
    SessionRuntime, SessionRecord, SessionStatus, SessionSummary, SessionTranscriptEvent,
    SessionTranscriptEventPayload, TranscriptQuery,
};

use nenjo_harness::SessionSearchFilter;

use super::{Tool, ToolCategory, ToolResult};

const SESSION_LIST_TOOL_NAME: &str = "session_list";
const SESSION_READ_TOOL_NAME: &str = "session_read";
const SESSION_SEARCH_TOOL_NAME: &str = "session_search";

const LIST_DEFAULT_LIMIT: usize = 20;
const LIST_MAX_LIMIT: usize = 50;
const READ_MAX_LIMIT: usize = 50;
const EVENT_PREVIEW_MAX_CHARS: usize = 500;
const SEARCH_MAX_LIMIT: usize = 50;

/// Shared read access to the session runtime behind the history tools.
pub(crate) type SessionHistoryBackend = Arc<dyn SessionRuntime>;

/// `session_list`: paginated, filtered session summaries.
pub(crate) struct SessionListTool {
    backend: SessionHistoryBackend,
}

/// `session_read`: bounded transcript reads for one session.
pub(crate) struct SessionReadTool {
    backend: SessionHistoryBackend,
}

/// `session_search`: index-backed full-text search over session summaries and
/// transcripts, with a lexical substring fallback.
pub(crate) struct SessionSearchTool {
    backend: SessionHistoryBackend,
    search_service: Option<Arc<nenjo_harness::SessionSearchService>>,
}

pub(crate) fn session_history_tools(
    backend: SessionHistoryBackend,
    search_service: Option<Arc<nenjo_harness::SessionSearchService>>,
) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(SessionListTool {
            backend: backend.clone(),
        }),
        Arc::new(SessionReadTool {
            backend: backend.clone(),
        }),
        Arc::new(SessionSearchTool {
            backend,
            search_service,
        }),
    ]
}

fn session_label(record: &SessionRecord) -> String {
    record
        .summary
        .title
        .clone()
        .or_else(|| record.summary.first_user_prompt.clone())
        .unwrap_or_default()
}

fn bounded(text: &str, max_chars: usize) -> String {
    SessionSummary::bounded(text, max_chars)
}

fn session_summary_row(record: &SessionRecord) -> serde_json::Value {
    json!({
        "session_id": record.session_id,
        "kind": record.kind,
        "status": record.status,
        "title_or_first_prompt": session_label(record),
        "agent": record.agent,
        "project": record.project,
        "created_at": record.created_at.to_rfc3339(),
        "updated_at": record.updated_at.to_rfc3339(),
        "last_error_summary": record.summary.last_error_summary,
        "last_progress_message": record.summary.last_progress_message,
    })
}

fn parse_limit(args: &serde_json::Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value.min(max as u64) as usize)
        .unwrap_or(default)
        .max(1)
}

fn parse_timestamp(args: &serde_json::Value, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

fn parse_kind(args: &serde_json::Value) -> Option<nenjo_sessions::SessionKind> {
    match args.get("kind").and_then(serde_json::Value::as_str) {
        Some("chat") => Some(nenjo_sessions::SessionKind::Chat),
        Some("task") => Some(nenjo_sessions::SessionKind::Task),
        Some("domain") => Some(nenjo_sessions::SessionKind::Domain),
        _ => None,
    }
}

fn parse_status(args: &serde_json::Value) -> Option<SessionStatus> {
    match args.get("status").and_then(serde_json::Value::as_str) {
        Some("pending") => Some(SessionStatus::Pending),
        Some("active") => Some(SessionStatus::Active),
        Some("paused") => Some(SessionStatus::Paused),
        Some("waiting") => Some(SessionStatus::Waiting),
        Some("completed") => Some(SessionStatus::Completed),
        Some("cancelled") => Some(SessionStatus::Cancelled),
        Some("failed") => Some(SessionStatus::Failed),
        _ => None,
    }
}

/// Cursor format: `updated_at_rfc3339|session_id`; sorts after every record
/// with an older (updated_at, session_id) pair.
fn encode_cursor(record: &SessionRecord) -> String {
    format!("{}|{}", record.updated_at.to_rfc3339(), record.session_id)
}

fn decode_cursor(cursor: &str) -> Option<(chrono::DateTime<chrono::Utc>, uuid::Uuid)> {
    let (timestamp, session_id) = cursor.split_once('|')?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .with_timezone(&chrono::Utc);
    let session_id = uuid::Uuid::parse_str(session_id).ok()?;
    Some((timestamp, session_id))
}

fn cursor_boundary(record: &SessionRecord, cursor: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let Some((timestamp, session_id)) = decode_cursor(cursor) {
        let record_updated: chrono::DateTime<chrono::Utc> = record.updated_at;
        return record_updated
            .cmp(&timestamp)
            .then(record.session_id.cmp(&session_id));
    }
    Ordering::Greater
}

fn matches_filters(
    record: &SessionRecord,
    kind: Option<nenjo_sessions::SessionKind>,
    status: Option<SessionStatus>,
    agent: Option<&str>,
    project: Option<&str>,
    after: Option<chrono::DateTime<chrono::Utc>>,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if let Some(kind) = kind
        && record.kind != kind
    {
        return false;
    }
    if let Some(status) = status
        && record.status != status
    {
        return false;
    }
    if let Some(agent) = agent
        && record.agent.as_deref() != Some(agent)
    {
        return false;
    }
    if let Some(project) = project
        && record.project.as_deref() != Some(project)
    {
        return false;
    }
    if let Some(after) = after
        && record.updated_at <= after
    {
        return false;
    }
    if let Some(before) = before
        && record.updated_at >= before
    {
        return false;
    }
    true
}

/// Session list filters shared by `session_list` and pagination helpers.
struct SessionListFilters {
    kind: Option<nenjo_sessions::SessionKind>,
    status: Option<SessionStatus>,
    agent: Option<String>,
    project: Option<String>,
    after: Option<chrono::DateTime<chrono::Utc>>,
    before: Option<chrono::DateTime<chrono::Utc>>,
    cursor: Option<String>,
    limit: usize,
}

impl SessionListFilters {
    fn from_args(args: &serde_json::Value, default_limit: usize, max_limit: usize) -> Self {
        Self {
            kind: parse_kind(args),
            status: parse_status(args),
            agent: args
                .get("agent")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            project: args
                .get("project")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            after: parse_timestamp(args, "after"),
            before: parse_timestamp(args, "before"),
            cursor: args
                .get("cursor")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            limit: parse_limit(args, default_limit, max_limit),
        }
    }
}

fn listed_sessions<'a>(
    records: &'a [SessionRecord],
    filters: &SessionListFilters,
) -> (Vec<&'a SessionRecord>, Option<String>) {
    let SessionListFilters {
        kind,
        status,
        agent,
        project,
        after,
        before,
        cursor,
        limit,
    } = filters;
    let mut matching: Vec<&SessionRecord> = records
        .iter()
        .filter(|record| {
            matches_filters(
                record,
                *kind,
                *status,
                agent.as_deref(),
                project.as_deref(),
                *after,
                *before,
            ) && cursor.as_deref().is_none_or(|cursor| {
                cursor_boundary(record, cursor) == std::cmp::Ordering::Greater
            })
        })
        .collect();
    matching.sort_by(|left, right| {
        left.updated_at
            .cmp(&right.updated_at)
            .then(left.session_id.cmp(&right.session_id))
    });
    let page: Vec<&SessionRecord> = matching.iter().take(*limit).copied().collect();
    let next_cursor = if matching.len() > *limit {
        page.last().map(|record| encode_cursor(record))
    } else {
        None
    };
    (page, next_cursor)
}

fn transcript_event_row(event: &SessionTranscriptEvent) -> serde_json::Value {
    let payload = match &event.payload {
        SessionTranscriptEventPayload::ConversationMessage { message } => {
            let role = match message {
                nenjo_models::ConversationMessage::Chat(chat) => Some(chat.role),
                _ => None,
            };
            let text = match message {
                nenjo_models::ConversationMessage::Chat(chat) => Some(chat.content.as_str()),
                nenjo_models::ConversationMessage::AssistantToolCalls { text, .. } => {
                    text.as_deref()
                }
                nenjo_models::ConversationMessage::ToolResults(_) => None,
                nenjo_models::ConversationMessage::ArtifactAnalysis(analysis) => {
                    Some(analysis.text.as_str())
                }
                nenjo_models::ConversationMessage::RuntimeContext(_) => None,
            };
            json!({
                "type": "conversation_message",
                "role": role,
                "text": text.map(|text| bounded(text, EVENT_PREVIEW_MAX_CHARS)),
            })
        }
        SessionTranscriptEventPayload::DomainActivated {
            domain_name,
            user_message_preview,
            ..
        } => json!({
            "type": "domain_activated",
            "domain_name": domain_name,
            "user_message_preview": user_message_preview,
        }),
        SessionTranscriptEventPayload::DomainDeactivated { domain_name, .. } => {
            json!({ "type": "domain_deactivated", "domain_name": domain_name })
        }
        SessionTranscriptEventPayload::ToolCalls {
            tool_names,
            text_preview,
            ..
        } => json!({
            "type": "tool_calls",
            "tool_names": tool_names,
            "text_preview": text_preview,
        }),
        SessionTranscriptEventPayload::ToolResult {
            tool_name,
            success,
            output_preview,
            error_preview,
            ..
        } => json!({
            "type": "tool_result",
            "tool_name": tool_name,
            "success": success,
            "output_preview": output_preview,
            "error_preview": error_preview,
        }),
        SessionTranscriptEventPayload::AbilityStarted { ability_name, .. } => {
            json!({ "type": "ability_started", "ability_name": ability_name })
        }
        SessionTranscriptEventPayload::AbilityCompleted {
            ability_name,
            success,
            final_output,
            ..
        } => json!({
            "type": "ability_completed",
            "ability_name": ability_name,
            "success": success,
            "final_output": bounded(final_output, EVENT_PREVIEW_MAX_CHARS),
        }),
        SessionTranscriptEventPayload::DelegationStarted {
            target_agent_name,
            task_input,
            ..
        } => json!({
            "type": "delegation_started",
            "target_agent_name": target_agent_name,
            "task_input": bounded(task_input, EVENT_PREVIEW_MAX_CHARS),
        }),
        SessionTranscriptEventPayload::DelegationCompleted {
            target_agent_name,
            success,
            final_output,
            ..
        } => json!({
            "type": "delegation_completed",
            "target_agent_name": target_agent_name,
            "success": success,
            "final_output": bounded(final_output, EVENT_PREVIEW_MAX_CHARS),
        }),
        SessionTranscriptEventPayload::TurnCompleted { final_output } => json!({
            "type": "turn_completed",
            "final_output": bounded(final_output, EVENT_PREVIEW_MAX_CHARS),
        }),
        SessionTranscriptEventPayload::TurnInterrupted { reason } => {
            json!({ "type": "turn_interrupted", "reason": reason })
        }
    };
    json!({
        "seq": event.seq,
        "recorded_at": event.recorded_at.to_rfc3339(),
        "payload": payload,
    })
}

fn search_matches(record: &SessionRecord, query_lower: &str) -> bool {
    let haystacks = [
        session_label(record),
        record.summary.last_error_summary.clone().unwrap_or_default(),
        record
            .summary
            .last_progress_message
            .clone()
            .unwrap_or_default(),
        record.agent.clone().unwrap_or_default(),
        record.project.clone().unwrap_or_default(),
    ];
    haystacks
        .iter()
        .any(|haystack| haystack.to_lowercase().contains(query_lower))
}

fn invalid_arguments(error: &str) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new().into(),
        error: Some(error.to_string()),
    }
}

#[async_trait]
impl Tool for SessionListTool {
    fn name(&self) -> &str {
        SESSION_LIST_TOOL_NAME
    }

    fn description(&self) -> &str {
        "List prior sessions on this worker, newest first, with pagination. Each row includes the session id, kind (chat/task/domain), status, title or first user prompt, agent, project, timestamps, and last error. Use session_read to inspect a session's transcript."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["chat", "task", "domain"],
                    "description": "Filter by session kind."
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "active", "paused", "waiting", "completed", "cancelled", "failed"],
                    "description": "Filter by session status."
                },
                "agent": {
                    "type": "string",
                    "description": "Filter by exact agent slug."
                },
                "project": {
                    "type": "string",
                    "description": "Filter by exact project slug."
                },
                "after": {
                    "type": "string",
                    "description": "Only sessions updated after this RFC 3339 timestamp."
                },
                "before": {
                    "type": "string",
                    "description": "Only sessions updated before this RFC 3339 timestamp."
                },
                "limit": {
                    "type": "integer",
                    "description": format!("Maximum rows to return (default {LIST_DEFAULT_LIMIT}, max {LIST_MAX_LIMIT}).")
                },
                "cursor": {
                    "type": "string",
                    "description": "Opaque next_cursor from a previous page."
                }
            },
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Some(other) = args.get("kind").and_then(serde_json::Value::as_str)
            && !matches!(other, "chat" | "task" | "domain")
        {
            return Ok(invalid_arguments(&format!("unknown session kind: {other}")));
        }
        if let Some(other) = args.get("status").and_then(serde_json::Value::as_str)
            && !matches!(
                other,
                "pending" | "active" | "paused" | "waiting" | "completed" | "cancelled" | "failed"
            )
        {
            return Ok(invalid_arguments(&format!("unknown session status: {other}")));
        }
        let filters = SessionListFilters::from_args(&args, LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT);

        let records = self.backend.list_sessions().await?;
        let (page, next_cursor) = listed_sessions(&records, &filters);
        let rows: Vec<serde_json::Value> = page.iter().map(|record| session_summary_row(record)).collect();
        let output = json!({
            "sessions": rows,
            "next_cursor": next_cursor,
        });
        Ok(ToolResult::success(output.to_string()))
    }
}

#[async_trait]
impl Tool for SessionReadTool {
    fn name(&self) -> &str {
        SESSION_READ_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Read a session's transcript in bounded pages, ordered by sequence number. Returns events with role/text or tool previews plus next_seq for continuation. Combine with session_list to find sessions."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session UUID to read."
                },
                "after_seq": {
                    "type": "integer",
                    "description": "Return events with seq strictly greater than this value."
                },
                "limit": {
                    "type": "integer",
                    "description": format!("Maximum events to return (default {READ_MAX_LIMIT}, max {READ_MAX_LIMIT}).")
                }
            },
            "required": ["session_id"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(session_id) = args
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
        else {
            return Ok(invalid_arguments("session_id must be a UUID"));
        };
        let after_seq = args.get("after_seq").and_then(serde_json::Value::as_u64);
        let limit = parse_limit(&args, READ_MAX_LIMIT, READ_MAX_LIMIT);

        let record = self.backend.get_session(session_id).await?;
        let Some(record) = record else {
            return Ok(invalid_arguments(&format!(
                "session {session_id} not found"
            )));
        };
        let events = self
            .backend
            .read_transcript(
                session_id,
                TranscriptQuery {
                    after_seq,
                    limit: Some(limit),
                },
            )
            .await?;
        let rows: Vec<serde_json::Value> = events.iter().map(transcript_event_row).collect();
        let next_seq = events.last().map(|event| event.seq);
        let output = json!({
            "session_id": session_id,
            "kind": record.kind,
            "status": record.status,
            "title_or_first_prompt": session_label(&record),
            "events": rows,
            "next_seq": next_seq,
        });
        Ok(ToolResult::success(output.to_string()))
    }
}

#[async_trait]
impl Tool for SessionSearchTool {
    fn name(&self) -> &str {
        SESSION_SEARCH_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Full-text search over prior sessions. Index-backed BM25 search covers titles, first user prompts, error summaries, and full transcript bodies (user, assistant, and tool text); falls back to a substring scan when the index is unavailable. Results are ranked by relevance with matched fields and excerpts. Use session_read to inspect a match from its seq."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search terms; all terms must match (AND semantics)."
                },
                "kind": {
                    "type": "string",
                    "enum": ["chat", "task", "domain"],
                    "description": "Filter by session kind."
                },
                "search_transcripts": {
                    "type": "boolean",
                    "description": "Include transcript bodies in the search, not just session summaries. Default true."
                },
                "limit": {
                    "type": "integer",
                    "description": format!("Maximum hits to return (default {LIST_DEFAULT_LIMIT}, max {SEARCH_MAX_LIMIT}).")
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(query) = args.get("query").and_then(serde_json::Value::as_str) else {
            return Ok(invalid_arguments("query is required"));
        };
        if query.trim().is_empty() {
            return Ok(invalid_arguments("query must not be empty"));
        }
        if let Some(other) = args.get("kind").and_then(serde_json::Value::as_str)
            && !matches!(other, "chat" | "task" | "domain")
        {
            return Ok(invalid_arguments(&format!("unknown session kind: {other}")));
        }
        let include_transcripts = args
            .get("search_transcripts")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let kind = parse_kind(&args);
        let filter = SessionSearchFilter {
            kind: kind.map(|kind| match kind {
                nenjo_sessions::SessionKind::Chat => "chat".to_string(),
                nenjo_sessions::SessionKind::Task => "task".to_string(),
                nenjo_sessions::SessionKind::Domain => "domain".to_string(),
            }),
        };
        let limit = parse_limit(&args, LIST_DEFAULT_LIMIT, SEARCH_MAX_LIMIT);

        // Index-backed path when a search service is wired and transcripts are
        // in scope; otherwise (or on index failure) fall back to the summary
        // substring scan over session records.
        if let Some(service) = self.search_service.as_ref()
            && let Some(hits) = service
                .search(query, &filter, limit, include_transcripts)
                .await
                .ok()
                .flatten()
        {
            let session_ids: std::collections::HashSet<uuid::Uuid> =
                hits.iter().map(|hit| hit.session_id).collect();
            let records = self.backend.list_sessions().await?;
            let rows: Vec<serde_json::Value> = hits
                .iter()
                .map(|hit| {
                    let record = records
                        .iter()
                        .find(|record| record.session_id == hit.session_id);
                    let mut row = match record {
                        Some(record) => session_summary_row(record),
                        None => json!({ "session_id": hit.session_id }),
                    };
                    row["hit_seq"] = json!(hit.seq);
                    row["hit_role"] = json!(hit.role);
                    row["score"] = json!(hit.score);
                    row["matched_fields"] = json!(hit.matched_fields);
                    row["excerpt"] = json!(hit.excerpt);
                    row
                })
                .collect();
            let output = json!({
                "query": query,
                "engine": "index",
                "sessions_scanned": session_ids.len(),
                "matches": rows,
            });
            return Ok(ToolResult::success(output.to_string()));
        }

        // Lexical fallback: summary fields only.
        let query_lower = query.trim().to_lowercase();
        let records = self.backend.list_sessions().await?;
        let mut matches: Vec<&SessionRecord> = records
            .iter()
            .filter(|record| {
                kind.is_none_or(|kind| record.kind == kind) && search_matches(record, &query_lower)
            })
            .collect();
        matches.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then(right.session_id.cmp(&left.session_id))
        });
        matches.truncate(limit);
        let rows: Vec<serde_json::Value> = matches
            .iter()
            .map(|record| session_summary_row(record))
            .collect();
        let output = json!({
            "query": query,
            "engine": "lexical",
            "matches": rows,
        });
        Ok(ToolResult::success(output.to_string()))
    }
}
