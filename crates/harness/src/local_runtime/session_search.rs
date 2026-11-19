//! Tantivy-backed full-text search over local session records and transcripts.
//!
//! The corpus is assembled from the local session record store and transcript
//! files; the index is rebuilt lazily whenever the corpus version changes.
//! The source files stay canonical and the index is disposable: any build or
//! search failure falls back to a lexical substring scan.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Utc};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, INDEXED, STORED, Schema, TEXT, TantivyDocument, Value};
use tantivy::{Index, IndexReader, doc};
use tokio::task;
use tracing::{debug, warn};
use uuid::Uuid;

/// Maximum events indexed per session so one giant transcript cannot dominate.
pub const MAX_EVENTS_PER_SESSION: usize = 500;
/// Maximum transcript excerpts returned per session for one query.
const MAX_HITS_PER_SESSION: usize = 5;
/// Tantivy writer memory budget, matching the knowledge search index.
const INDEX_WRITER_MEMORY_BUDGET: usize = 32_000_000;

/// One indexed unit: either a session summary row or a transcript event.
#[derive(Debug, Clone)]
pub struct SessionSearchDoc {
    pub session_id: Uuid,
    /// Sequence number; `0` for the synthetic session-summary document.
    pub seq: u64,
    /// `chat`, `task`, or `domain`.
    pub kind: String,
    /// Session title or first user prompt.
    pub title: String,
    /// `summary`, `user`, `assistant`, `tool`, or `system`.
    pub role: String,
    /// `summary`, `ok`, `error`, or `interrupted`.
    pub status: String,
    /// Indexed text: summary fields for `summary`, event text otherwise.
    pub body: String,
}

/// Identifies when the corpus changed and the index needs rebuilding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCorpusVersion {
    pub session_count: usize,
    pub max_updated_at: Option<DateTime<Utc>>,
    /// Distinguishes summaries-only indexes from full-corpus indexes built
    /// from the same corpus state.
    pub summaries_only: bool,
}

/// A ranked search hit pointing back at the originating session event.
#[derive(Debug, Clone)]
pub struct SessionSearchHit {
    pub session_id: Uuid,
    pub seq: u64,
    /// `summary`, `user`, `assistant`, `tool`, or `system`.
    pub role: String,
    /// `summary`, `ok`, `error`, or `interrupted`.
    pub status: String,
    pub score: f64,
    pub matched_fields: Vec<String>,
    /// A short window of the matched text.
    pub excerpt: String,
}

/// Filter applied after retrieval.
#[derive(Debug, Clone, Default)]
pub struct SessionSearchFilter {
    pub kind: Option<String>,
    /// `error`, `ok`, or `interrupted`.
    pub status: Option<String>,
}

struct SessionSearchSchema {
    session_id: Field,
    seq: Field,
    kind: Field,
    title: Field,
    role: Field,
    status: Field,
    body: Field,
    doc_index: Field,
}

impl SessionSearchSchema {
    fn build() -> (Schema, Self) {
        let mut builder = Schema::builder();
        let session_id = builder.add_u64_field("session_id", INDEXED | STORED);
        let seq = builder.add_u64_field("seq", INDEXED | STORED);
        let doc_index = builder.add_u64_field("doc_index", INDEXED | STORED);
        let kind = builder.add_text_field("kind", TEXT);
        let title = builder.add_text_field("title", TEXT);
        let role = builder.add_text_field("role", TEXT);
        let status = builder.add_text_field("status", TEXT);
        let body = builder.add_text_field("body", TEXT);
        let schema = builder.build();
        (
            schema,
            Self {
                session_id,
                seq,
                kind,
                title,
                role,
                status,
                body,
                doc_index,
            },
        )
    }

    fn search_fields(&self) -> Vec<Field> {
        vec![self.kind, self.title, self.role, self.status, self.body]
    }
}

fn session_id_bits(session_id: Uuid) -> u64 {
    u64::from_be_bytes(session_id.as_bytes()[0..8].try_into().expect("16 bytes"))
}

/// Excerpt centered on the first query-term occurrence when findable.
fn excerpt_for(query_terms: &[&str], body: &str) -> String {
    const WINDOW: usize = 200;
    let lower_body = body.to_lowercase();
    let mut start = 0;
    for term in query_terms {
        let term_lower = term.to_lowercase();
        if let Some(position) = lower_body.find(&term_lower) {
            start = position.saturating_sub(WINDOW / 3);
            break;
        }
        // Fall back to a token-level match; terms may be stemmed in the index
        // but the raw body retains the original wording.
        let mut token_start: Option<usize> = None;
        let mut offset = 0usize;
        for token in lower_body.split_whitespace() {
            if token.starts_with(&term_lower) {
                token_start = Some(offset);
                break;
            }
            offset += token.len() + 1;
        }
        if let Some(position) = token_start {
            start = position.saturating_sub(WINDOW / 3);
            break;
        }
    }
    let window: String = body.chars().skip(start).take(WINDOW).collect();
    if start > 0 {
        format!("…{window}")
    } else {
        window
    }
}

struct SessionSearchIndex {
    index: Index,
    reader: IndexReader,
    schema: SessionSearchSchema,
    docs: Vec<SessionSearchDoc>,
}

impl SessionSearchIndex {
    fn build(docs: Vec<SessionSearchDoc>) -> Result<Self> {
        let (schema, fields) = SessionSearchSchema::build();
        let index = Index::create_in_ram(schema);
        let mut writer = index.writer(INDEX_WRITER_MEMORY_BUDGET)?;
        for (doc_index, document) in docs.iter().enumerate() {
            writer.add_document(doc!(
                fields.doc_index => doc_index as u64,
                fields.session_id => session_id_bits(document.session_id),
                fields.seq => document.seq,
                fields.kind => document.kind.as_str(),
                fields.title => document.title.as_str(),
                fields.role => document.role.as_str(),
                fields.status => document.status.as_str(),
                fields.body => document.body.as_str(),
            ))?;
        }
        writer.commit()?;
        let reader = index.reader()?;
        Ok(Self {
            index,
            reader,
            schema: fields,
            docs,
        })
    }

    fn search(&self, query: &str, filter: &SessionSearchFilter) -> Result<Vec<SessionSearchHit>> {
        let searcher = self.reader.searcher();
        let mut parser = QueryParser::for_index(&self.index, self.schema.search_fields());
        parser.set_conjunction_by_default();
        parser.set_field_boost(self.schema.title, 4.0);
        parser.set_field_boost(self.schema.role, 2.0);
        parser.set_field_boost(self.schema.body, 1.0);

        let (parsed_query, parse_errors) = parser.parse_query_lenient(query);
        if !parse_errors.is_empty() {
            debug!(
                errors = ?parse_errors,
                "Session search query parsed with recoverable errors"
            );
        }

        let total_docs = self.docs.len().max(1);
        let top_docs = searcher.search(
            &parsed_query,
            &TopDocs::with_limit(total_docs).order_by_score(),
        )?;

        let query_terms: Vec<&str> = query.split_whitespace().collect();
        let mut per_session: BTreeSet<Uuid> = BTreeSet::new();
        let mut hits_per_session: std::collections::HashMap<Uuid, usize> =
            std::collections::HashMap::new();
        let mut hits = Vec::new();
        for (score, address) in top_docs {
            let retrieved = searcher.doc::<TantivyDocument>(address)?;
            let Some(doc_index) = retrieved
                .get_first(self.schema.doc_index)
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
            else {
                continue;
            };
            let Some(document) = self.docs.get(doc_index) else {
                continue;
            };
            if let Some(kind) = filter.kind.as_deref()
                && document.kind != kind
            {
                continue;
            }
            if let Some(status) = filter.status.as_deref()
                && document.status != status
            {
                continue;
            }
            let session_hits = hits_per_session.entry(document.session_id).or_default();
            if *session_hits >= MAX_HITS_PER_SESSION {
                continue;
            }
            *session_hits += 1;
            per_session.insert(document.session_id);
            hits.push(SessionSearchHit {
                session_id: document.session_id,
                seq: document.seq,
                role: document.role.clone(),
                status: document.status.clone(),
                score: score as f64,
                matched_fields: matched_fields(document, &query_terms),
                excerpt: excerpt_for(&query_terms, &document.body),
            });
        }
        Ok(hits)
    }
}

fn matched_fields(document: &SessionSearchDoc, query_terms: &[&str]) -> Vec<String> {
    let mut matched = BTreeSet::new();
    let lower_terms: Vec<String> = query_terms.iter().map(|term| term.to_lowercase()).collect();
    let fields: [(&str, &String); 3] = [
        ("title", &document.title),
        ("role", &document.role),
        ("body", &document.body),
    ];
    for (label, value) in fields {
        let lower_value = value.to_lowercase();
        if lower_terms
            .iter()
            .any(|term| lower_value.contains(term.as_str()))
        {
            matched.insert(label.to_string());
        }
    }
    if matched.is_empty() {
        matched.insert("body".to_string());
    }
    matched.into_iter().collect()
}

/// Storage-agnostic provider of session search corpus data.
///
/// Implemented by runtimes that can enumerate their sessions and transcripts
/// so tools never touch filesystem paths directly.
#[async_trait::async_trait]
pub trait SessionSearchBackend: Send + Sync {
    /// Assemble the current corpus and its version. The corpus should include
    /// one summary document per session plus transcript event documents.
    async fn search_corpus(&self) -> Result<(Vec<SessionSearchDoc>, SessionCorpusVersion)>;
}

/// Cache plus backend: performs index-backed search with lexical fallback.
pub struct SessionSearchService {
    backend: Arc<dyn SessionSearchBackend>,
    cache: SessionSearchCache,
}

impl SessionSearchService {
    pub fn new(backend: Arc<dyn SessionSearchBackend>) -> Self {
        Self {
            backend,
            cache: SessionSearchCache::new(),
        }
    }

    /// Index-backed search over the current corpus; falls back to a lexical
    /// conjunction scan on any index failure. Returns `None` for empty or
    /// whitespace queries.
    pub async fn search(
        &self,
        query: &str,
        filter: &SessionSearchFilter,
        limit: usize,
        include_transcripts: bool,
    ) -> Result<Option<Vec<SessionSearchHit>>> {
        if query.trim().is_empty() {
            return Ok(None);
        }
        let (mut docs, mut version) = self.backend.search_corpus().await?;
        if !include_transcripts {
            docs.retain(|doc| doc.role == "summary");
            version.summaries_only = true;
        }
        match self
            .cache
            .search(docs.clone(), &version, query, filter)
            .await
        {
            Some(hits) => Ok(Some(hits)),
            None => Ok(Some(lexical_search(&docs, query, filter, limit))),
        }
    }
}

/// Cache of the most recent index plus the corpus version it was built from.
pub struct SessionSearchCache {
    inner: tokio::sync::Mutex<Option<(SessionCorpusVersion, Arc<SessionSearchIndex>)>>,
}

impl Default for SessionSearchCache {
    fn default() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(None),
        }
    }
}

impl SessionSearchCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Search the corpus, rebuilding the index when `version` differs from the
    /// cached build. On any failure, returns `None` so callers can fall back
    /// to lexical search.
    pub async fn search(
        &self,
        docs: Vec<SessionSearchDoc>,
        version: &SessionCorpusVersion,
        query: &str,
        filter: &SessionSearchFilter,
    ) -> Option<Vec<SessionSearchHit>> {
        if query.trim().is_empty() {
            return None;
        }
        let started = Instant::now();
        let mut cached = self.inner.lock().await;
        let rebuild = match cached.as_ref() {
            Some((cached_version, _)) => cached_version != version,
            None => true,
        };
        let index = if rebuild {
            let built = match task::spawn_blocking(move || SessionSearchIndex::build(docs)).await {
                Ok(Ok(built)) => Arc::new(built),
                Ok(Err(error)) => {
                    warn!(error = %error, "Falling back to lexical session search after index build failure");
                    return None;
                }
                Err(error) => {
                    warn!(error = %error, "Session search index build task failed");
                    return None;
                }
            };
            debug!(
                total_build_ms = started.elapsed().as_millis() as u64,
                "Rebuilt session search index"
            );
            *cached = Some((version.clone(), built.clone()));
            built
        } else {
            cached.as_ref().expect("checked above").1.clone()
        };
        match index.search(query, filter) {
            Ok(hits) => Some(hits),
            Err(error) => {
                warn!(
                    error = %error,
                    "Falling back to lexical session search after index search failure"
                );
                None
            }
        }
    }
}

/// Lexical fallback: conjunction substring scan over the corpus documents.
pub fn lexical_search(
    docs: &[SessionSearchDoc],
    query: &str,
    filter: &SessionSearchFilter,
    limit: usize,
) -> Vec<SessionSearchHit> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut per_session: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();
    let mut hits = Vec::new();
    for document in docs {
        if let Some(kind) = filter.kind.as_deref()
            && document.kind != kind
        {
            continue;
        }
        if let Some(status) = filter.status.as_deref()
            && document.status != status
        {
            continue;
        }
        let session_hits = per_session.entry(document.session_id).or_default();
        if *session_hits >= MAX_HITS_PER_SESSION {
            continue;
        }
        let lower = document.body.to_lowercase();
        let lower_title = document.title.to_lowercase();
        let all_match = terms
            .iter()
            .all(|term| lower.contains(term.as_str()) || lower_title.contains(term.as_str()));
        if !all_match {
            continue;
        }
        *session_hits += 1;
        hits.push(SessionSearchHit {
            session_id: document.session_id,
            seq: document.seq,
            role: document.role.clone(),
            status: document.status.clone(),
            score: 0.0,
            matched_fields: matched_fields(
                document,
                &terms.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
            excerpt: excerpt_for(
                &terms.iter().map(String::as_str).collect::<Vec<_>>(),
                &document.body,
            ),
        });
        if hits.len() >= limit {
            break;
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_doc(session_id: Uuid, kind: &str, title: &str, body: &str) -> SessionSearchDoc {
        SessionSearchDoc {
            session_id,
            seq: 0,
            kind: kind.to_string(),
            title: title.to_string(),
            role: "summary".to_string(),
            status: "summary".to_string(),
            body: body.to_string(),
        }
    }

    fn event_doc(session_id: Uuid, seq: u64, role: &str, body: &str) -> SessionSearchDoc {
        SessionSearchDoc {
            session_id,
            seq,
            kind: "chat".to_string(),
            title: String::new(),
            role: role.to_string(),
            status: "ok".to_string(),
            body: body.to_string(),
        }
    }

    fn tool_doc(session_id: Uuid, seq: u64, success: bool, body: &str) -> SessionSearchDoc {
        SessionSearchDoc {
            session_id,
            seq,
            kind: "chat".to_string(),
            title: String::new(),
            role: "tool".to_string(),
            status: if success { "ok" } else { "error" }.to_string(),
            body: body.to_string(),
        }
    }

    #[test]
    fn lexical_search_requires_all_terms() {
        let session = Uuid::new_v4();
        let docs = vec![
            summary_doc(session, "chat", "Compaction bug", "compaction"),
            event_doc(
                session,
                1,
                "user",
                "the strategy used for compaction failed",
            ),
            event_doc(session, 2, "assistant", "unrelated content"),
        ];
        let hits = lexical_search(
            &docs,
            "compaction strategy",
            &SessionSearchFilter::default(),
            10,
        );
        // Only the event doc contains both terms (AND semantics); the summary
        // title has "compaction" but not "strategy".
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 1);
        assert!(hits.iter().all(|hit| hit.session_id == session));
        // Term order should not matter.
        let hits = lexical_search(
            &docs,
            "strategy compaction",
            &SessionSearchFilter::default(),
            10,
        );
        assert_eq!(hits.len(), 1);
        // Unrelated term yields nothing.
        let hits = lexical_search(
            &docs,
            "compaction strategy zebra",
            &SessionSearchFilter::default(),
            10,
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn lexical_search_respects_kind_filter_and_limit() {
        let chat = Uuid::new_v4();
        let task = Uuid::new_v4();
        let docs = vec![
            summary_doc(chat, "chat", "chat about deployment", "deployment"),
            summary_doc(task, "task", "task about deployment", "deployment"),
        ];
        let filter = SessionSearchFilter {
            kind: Some("task".to_string()),
            ..SessionSearchFilter::default()
        };
        let hits = lexical_search(&docs, "deployment", &filter, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, task);

        let hits = lexical_search(&docs, "deployment", &SessionSearchFilter::default(), 1);
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn tantivy_index_finds_transcript_body_terms() {
        let session = Uuid::new_v4();
        let docs = vec![
            summary_doc(session, "chat", "Session title", "greeting content"),
            event_doc(session, 1, "user", "hello there"),
            event_doc(
                session,
                5,
                "assistant",
                "the NATS auth failure was traced to a stale token",
            ),
        ];
        let cache = SessionSearchCache::new();
        let version = SessionCorpusVersion {
            session_count: 1,
            max_updated_at: Some(Utc::now()),
            summaries_only: false,
        };
        let hits = cache
            .search(
                docs.clone(),
                &version,
                "NATS auth failure",
                &SessionSearchFilter::default(),
            )
            .await
            .expect("index search should succeed");
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.session_id == session));
        let body_hit = hits
            .iter()
            .find(|hit| hit.seq == 5)
            .expect("transcript event hit");
        assert!(body_hit.excerpt.to_lowercase().contains("nats"));

        // Same version reuses the cache and still finds results.
        let hits = cache
            .search(
                docs,
                &version,
                "NATS auth failure",
                &SessionSearchFilter::default(),
            )
            .await
            .expect("cached search should succeed");
        assert!(!hits.is_empty());
    }

    #[tokio::test]
    async fn cache_rebuilds_when_version_changes() {
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        let cache = SessionSearchCache::new();
        let version_a = SessionCorpusVersion {
            session_count: 1,
            max_updated_at: Some(Utc::now()),
            summaries_only: false,
        };
        let docs_a = vec![summary_doc(session_a, "chat", "alpha document", "alpha")];
        let hits = cache
            .search(docs_a, &version_a, "alpha", &SessionSearchFilter::default())
            .await
            .expect("first search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, session_a);

        // Same count, different max_updated_at -> rebuild.
        let version_b = SessionCorpusVersion {
            session_count: 1,
            max_updated_at: Some(Utc::now()),
            summaries_only: false,
        };
        let docs_b = vec![summary_doc(session_b, "chat", "beta document", "beta")];
        let hits = cache
            .search(
                docs_b.clone(),
                &version_b,
                "alpha",
                &SessionSearchFilter::default(),
            )
            .await
            .expect("rebuilt search");
        assert!(hits.is_empty(), "old session should be gone after rebuild");
        let hits = cache
            .search(docs_b, &version_b, "beta", &SessionSearchFilter::default())
            .await
            .expect("rebuilt search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, session_b);
    }

    #[tokio::test]
    async fn index_respects_kind_filter() {
        let chat = Uuid::new_v4();
        let task = Uuid::new_v4();
        let docs = vec![
            summary_doc(chat, "chat", "deployment runbook chat", "deployment"),
            summary_doc(task, "task", "deployment task", "deployment"),
        ];
        let cache = SessionSearchCache::new();
        let version = SessionCorpusVersion {
            session_count: 2,
            max_updated_at: Some(Utc::now()),
            summaries_only: false,
        };
        let filter = SessionSearchFilter {
            kind: Some("task".to_string()),
            ..SessionSearchFilter::default()
        };
        let hits = cache
            .search(docs, &version, "deployment", &filter)
            .await
            .expect("filtered search");
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.session_id == task));
    }

    #[test]
    fn lexical_search_filters_by_status() {
        let session = Uuid::new_v4();
        let docs = vec![
            tool_doc(session, 1, false, "git push rejected"),
            tool_doc(session, 2, true, "git push accepted"),
            event_doc(session, 3, "user", "retry the push"),
        ];
        let filter = SessionSearchFilter {
            status: Some("error".to_string()),
            ..SessionSearchFilter::default()
        };
        let hits = lexical_search(&docs, "push", &filter, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 1);
        assert_eq!(hits[0].status, "error");

        let filter = SessionSearchFilter {
            status: Some("ok".to_string()),
            ..SessionSearchFilter::default()
        };
        let hits = lexical_search(&docs, "push", &filter, 10);
        // Both the successful tool result and the user event are status ok.
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.status == "ok"));
    }

    #[tokio::test]
    async fn index_filters_by_status() {
        let session = Uuid::new_v4();
        let docs = vec![
            tool_doc(session, 1, false, "the deploy failed with exit code 1"),
            tool_doc(session, 2, true, "the deploy succeeded cleanly"),
        ];
        let cache = SessionSearchCache::new();
        let version = SessionCorpusVersion {
            session_count: 1,
            max_updated_at: Some(Utc::now()),
            summaries_only: false,
        };
        let filter = SessionSearchFilter {
            status: Some("error".to_string()),
            ..SessionSearchFilter::default()
        };
        let hits = cache
            .search(docs, &version, "deploy", &filter)
            .await
            .expect("filtered search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 1);
        assert_eq!(hits[0].status, "error");
        assert!(hits[0].excerpt.contains("failed"));
    }

    #[tokio::test]
    async fn index_matches_error_status_as_query_term() {
        // status is a search field: querying "error" finds failed calls even
        // when the error text does not literally contain the word.
        let session = Uuid::new_v4();
        let docs = vec![
            tool_doc(session, 1, false, "exit code 1"),
            tool_doc(session, 2, true, "exit code 0"),
        ];
        let cache = SessionSearchCache::new();
        let version = SessionCorpusVersion {
            session_count: 1,
            max_updated_at: Some(Utc::now()),
            summaries_only: false,
        };
        let hits = cache
            .search(docs, &version, "error", &SessionSearchFilter::default())
            .await
            .expect("status term search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].seq, 1);
    }

    #[tokio::test]
    async fn index_caps_hits_per_session() {
        let session = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut docs = vec![summary_doc(session, "chat", "caps", "repeated term")];
        for seq in 1..=8 {
            docs.push(event_doc(session, seq, "user", "repeated term everywhere"));
        }
        // A second session also matching, to confirm caps are per-session.
        docs.push(summary_doc(other, "chat", "caps", "repeated term"));
        let cache = SessionSearchCache::new();
        let version = SessionCorpusVersion {
            session_count: 2,
            max_updated_at: Some(Utc::now()),
            summaries_only: false,
        };
        let hits = cache
            .search(
                docs,
                &version,
                "repeated term",
                &SessionSearchFilter::default(),
            )
            .await
            .expect("capped search");
        let session_hits = hits.iter().filter(|hit| hit.session_id == session).count();
        let other_hits = hits.iter().filter(|hit| hit.session_id == other).count();
        assert_eq!(session_hits, MAX_HITS_PER_SESSION);
        assert_eq!(other_hits, 1);
    }

    #[tokio::test]
    async fn service_error_propagates_for_tool_fallback() {
        struct FailingBackend;

        #[async_trait::async_trait]
        impl SessionSearchBackend for FailingBackend {
            async fn search_corpus(&self) -> Result<(Vec<SessionSearchDoc>, SessionCorpusVersion)> {
                anyhow::bail!("corpus unavailable");
            }
        }

        let service = SessionSearchService::new(Arc::new(FailingBackend));
        let result = service
            .search("anything", &SessionSearchFilter::default(), 10, true)
            .await;
        assert!(
            result.is_err(),
            "backend errors must reach the caller so tools can fall back"
        );
    }
}
