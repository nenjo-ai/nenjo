//! Full-text knowledge search indexing.
//!
//! The tool contract stays metadata-first. This module owns the internal
//! Tantivy index lifecycle, cache keys, and timing stats used to make
//! `search_knowledge` fast without making search indexes part of pack storage.

use std::collections::{BTreeSet, HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, INDEXED, STORED, Schema, TEXT, TantivyDocument, Value};
use tantivy::{Index, IndexReader, doc};
use tokio::task;
use tracing::{debug, warn};

use crate::{
    KnowledgeDocFilter, KnowledgeDocManifest, KnowledgeDocSearchHit, KnowledgePack,
    KnowledgePackManifest, matches_filter, search_pack,
};

const INDEX_WRITER_MEMORY_BUDGET: usize = 32_000_000;
const EXACT_MATCH_SCORE: usize = 100_000;

/// Stable-enough cache key for an in-process knowledge search index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KnowledgeIndexKey {
    pub pack_selector: String,
    pub pack_id: String,
    pub root_uri: String,
    pub version: String,
    pub content_hash: String,
    pub document_count: usize,
    pub fallback_fingerprint: String,
}

impl KnowledgeIndexKey {
    pub fn new(pack_selector: impl Into<String>, manifest: &dyn KnowledgePackManifest) -> Self {
        let fallback_fingerprint = metadata_fingerprint(manifest);
        Self {
            pack_selector: pack_selector.into(),
            pack_id: manifest.pack_id().to_string(),
            root_uri: manifest.root_uri().to_string(),
            version: manifest.version().to_string(),
            content_hash: manifest.content_hash().to_string(),
            document_count: manifest.docs().len(),
            fallback_fingerprint,
        }
    }
}

/// Timing and size data for one index build.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeIndexBuildStats {
    pub document_count: usize,
    pub indexed_document_count: usize,
    pub missing_content_count: usize,
    pub total_content_bytes: usize,
    pub content_read_millis: u128,
    pub document_prepare_millis: u128,
    pub tantivy_add_docs_millis: u128,
    pub tantivy_commit_millis: u128,
    pub reader_open_millis: u128,
    pub total_build_millis: u128,
}

/// Timing and cache data for one search.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeSearchStats {
    pub cache_hit: bool,
    pub build_stats: Option<KnowledgeIndexBuildStats>,
    pub search_millis: u128,
    pub fallback_used: bool,
}

/// Cached full-text search service for knowledge packs.
#[derive(Clone, Default)]
pub struct KnowledgeSearchService {
    cache: Arc<RwLock<HashMap<KnowledgeIndexKey, Arc<KnowledgeSearchIndex>>>>,
}

impl KnowledgeSearchService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn search(
        &self,
        pack_selector: impl Into<String>,
        pack: Arc<dyn KnowledgePack>,
        query: impl Into<String>,
        filter: KnowledgeDocFilter,
    ) -> Vec<KnowledgeDocSearchHit> {
        self.search_with_stats(pack_selector, pack, query, filter)
            .await
            .0
    }

    pub async fn search_with_stats(
        &self,
        pack_selector: impl Into<String>,
        pack: Arc<dyn KnowledgePack>,
        query: impl Into<String>,
        filter: KnowledgeDocFilter,
    ) -> (Vec<KnowledgeDocSearchHit>, KnowledgeSearchStats) {
        let pack_selector = pack_selector.into();
        let query = query.into();

        if query.trim().is_empty() {
            let started = Instant::now();
            let hits = search_pack(pack.as_ref(), &query, filter);
            return (
                hits,
                KnowledgeSearchStats {
                    search_millis: elapsed_ms(started),
                    fallback_used: true,
                    ..KnowledgeSearchStats::default()
                },
            );
        }

        let key = KnowledgeIndexKey::new(pack_selector.clone(), pack.manifest());
        let (index, cache_hit, build_stats) = match self.get_or_build(key, pack.clone()).await {
            Ok(result) => result,
            Err(error) => {
                warn!(
                    pack = %pack_selector,
                    error = %error,
                    "Falling back to lexical knowledge search after index build failure"
                );
                let started = Instant::now();
                let hits = search_pack(pack.as_ref(), &query, filter);
                return (
                    hits,
                    KnowledgeSearchStats {
                        search_millis: elapsed_ms(started),
                        fallback_used: true,
                        ..KnowledgeSearchStats::default()
                    },
                );
            }
        };

        let started = Instant::now();
        let hits = match index.search(pack.as_ref(), &query, filter.clone()) {
            Ok(hits) => hits,
            Err(error) => {
                warn!(
                    pack = %pack_selector,
                    error = %error,
                    "Falling back to lexical knowledge search after index search failure"
                );
                let hits = search_pack(pack.as_ref(), &query, filter);
                return (
                    hits,
                    KnowledgeSearchStats {
                        cache_hit,
                        build_stats,
                        search_millis: elapsed_ms(started),
                        fallback_used: true,
                    },
                );
            }
        };

        (
            hits,
            KnowledgeSearchStats {
                cache_hit,
                build_stats,
                search_millis: elapsed_ms(started),
                fallback_used: false,
            },
        )
    }

    async fn get_or_build(
        &self,
        key: KnowledgeIndexKey,
        pack: Arc<dyn KnowledgePack>,
    ) -> Result<(
        Arc<KnowledgeSearchIndex>,
        bool,
        Option<KnowledgeIndexBuildStats>,
    )> {
        if let Some(index) = self
            .cache
            .read()
            .expect("knowledge search cache read lock poisoned")
            .get(&key)
            .cloned()
        {
            return Ok((index, true, None));
        }

        let build_key = key.clone();
        let index = task::spawn_blocking(move || KnowledgeSearchIndex::build(pack))
            .await
            .context("knowledge search index build task failed")??;
        let stats = index.build_stats.clone();
        let index = Arc::new(index);

        let mut cache = self
            .cache
            .write()
            .expect("knowledge search cache write lock poisoned");
        if let Some(existing) = cache.get(&key).cloned() {
            return Ok((existing, true, None));
        }
        cache.insert(key, index.clone());

        debug!(
            pack = %build_key.pack_selector,
            docs = stats.document_count,
            indexed_docs = stats.indexed_document_count,
            content_bytes = stats.total_content_bytes,
            missing_content = stats.missing_content_count,
            total_build_ms = stats.total_build_millis,
            content_read_ms = stats.content_read_millis,
            document_prepare_ms = stats.document_prepare_millis,
            tantivy_add_docs_ms = stats.tantivy_add_docs_millis,
            tantivy_commit_ms = stats.tantivy_commit_millis,
            reader_open_ms = stats.reader_open_millis,
            "Built knowledge search index"
        );

        Ok((index, false, Some(stats)))
    }
}

struct KnowledgeSearchIndex {
    index: Index,
    reader: IndexReader,
    schema: KnowledgeSearchSchema,
    docs: Vec<KnowledgeIndexedDocument>,
    build_stats: KnowledgeIndexBuildStats,
}

impl KnowledgeSearchIndex {
    fn build(pack: Arc<dyn KnowledgePack>) -> Result<Self> {
        let total_started = Instant::now();
        let (schema, fields) = KnowledgeSearchSchema::build();
        let index = Index::create_in_ram(schema);
        let mut writer = index.writer(INDEX_WRITER_MEMORY_BUDGET)?;

        let prepare_started = Instant::now();
        let mut content_read = Duration::ZERO;
        let mut docs = Vec::new();
        let mut missing_content_count = 0;
        let mut total_content_bytes = 0;

        for manifest in pack.manifest().docs() {
            let read_started = Instant::now();
            let content = pack
                .doc_content(manifest)
                .map(|content| content.into_owned());
            content_read += read_started.elapsed();

            let (content, content_missing) = match content {
                Some(content) => (content, false),
                None => (String::new(), true),
            };
            if content_missing {
                missing_content_count += 1;
            }

            total_content_bytes += content.len();
            docs.push(KnowledgeIndexedDocument::new(manifest.clone(), content));
        }
        let document_prepare_millis = elapsed_ms(prepare_started);

        let add_started = Instant::now();
        for (doc_index, indexed) in docs.iter().enumerate() {
            writer.add_document(indexed.tantivy_document(doc_index as u64, fields))?;
        }
        let tantivy_add_docs_millis = elapsed_ms(add_started);

        let commit_started = Instant::now();
        writer.commit()?;
        let tantivy_commit_millis = elapsed_ms(commit_started);

        let reader_started = Instant::now();
        let reader = index.reader()?;
        let reader_open_millis = elapsed_ms(reader_started);

        let build_stats = KnowledgeIndexBuildStats {
            document_count: pack.manifest().docs().len(),
            indexed_document_count: docs.len(),
            missing_content_count,
            total_content_bytes,
            content_read_millis: content_read.as_millis(),
            document_prepare_millis,
            tantivy_add_docs_millis,
            tantivy_commit_millis,
            reader_open_millis,
            total_build_millis: elapsed_ms(total_started),
        };

        Ok(Self {
            index,
            reader,
            schema: fields,
            docs,
            build_stats,
        })
    }

    fn search(
        &self,
        pack: &dyn KnowledgePack,
        query: &str,
        filter: KnowledgeDocFilter,
    ) -> Result<Vec<KnowledgeDocSearchHit>> {
        let mut hits = Vec::new();
        let mut seen = BTreeSet::new();

        if let Some(exact) = pack.read_manifest(query)
            && matches_filter(pack, exact, &filter)
        {
            seen.insert(exact.selector.clone());
            hits.push(KnowledgeDocSearchHit {
                document: exact.clone(),
                score: EXACT_MATCH_SCORE,
                matched: vec![exact_match_label(exact, query)],
            });
        }

        let searcher = self.reader.searcher();
        let mut parser = QueryParser::for_index(&self.index, self.schema.search_fields());
        parser.set_conjunction_by_default();
        parser.set_field_boost(self.schema.title, 6.0);
        parser.set_field_boost(self.schema.selector, 5.0);
        parser.set_field_boost(self.schema.id, 5.0);
        parser.set_field_boost(self.schema.source_path, 4.0);
        parser.set_field_boost(self.schema.headings, 4.0);
        parser.set_field_boost(self.schema.summary, 3.0);
        parser.set_field_boost(self.schema.tags, 3.0);
        parser.set_field_boost(self.schema.kind, 2.0);
        parser.set_field_boost(self.schema.related, 2.0);
        parser.set_field_boost(self.schema.body, 1.0);

        let (parsed_query, parse_errors) = parser.parse_query_lenient(query);
        if !parse_errors.is_empty() {
            debug!(
                errors = ?parse_errors,
                "Knowledge search query parsed with recoverable errors"
            );
        }

        let limit = self.docs.len().max(1);
        let top_docs =
            searcher.search(&parsed_query, &TopDocs::with_limit(limit).order_by_score())?;

        for (score, address) in top_docs {
            let retrieved = searcher.doc::<TantivyDocument>(address)?;
            let Some(doc_index) = retrieved
                .get_first(self.schema.doc_index)
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
            else {
                continue;
            };
            let Some(indexed) = self.docs.get(doc_index) else {
                continue;
            };
            if seen.contains(&indexed.manifest.selector) {
                continue;
            }
            if !matches_filter(pack, &indexed.manifest, &filter) {
                continue;
            }

            seen.insert(indexed.manifest.selector.clone());
            hits.push(KnowledgeDocSearchHit {
                document: indexed.manifest.clone(),
                score: tantivy_score(score),
                matched: indexed.matched_fields(query),
            });
        }

        Ok(hits)
    }
}

#[derive(Debug, Clone, Copy)]
struct KnowledgeSearchSchema {
    doc_index: Field,
    id: Field,
    selector: Field,
    source_path: Field,
    title: Field,
    summary: Field,
    kind: Field,
    tags: Field,
    related: Field,
    headings: Field,
    body: Field,
}

impl KnowledgeSearchSchema {
    fn build() -> (Schema, Self) {
        let mut builder = Schema::builder();
        let doc_index = builder.add_u64_field("doc_index", INDEXED | STORED);
        let id = builder.add_text_field("id", TEXT);
        let selector = builder.add_text_field("selector", TEXT);
        let source_path = builder.add_text_field("source_path", TEXT);
        let title = builder.add_text_field("title", TEXT);
        let summary = builder.add_text_field("summary", TEXT);
        let kind = builder.add_text_field("kind", TEXT);
        let tags = builder.add_text_field("tags", TEXT);
        let related = builder.add_text_field("related", TEXT);
        let headings = builder.add_text_field("headings", TEXT);
        let body = builder.add_text_field("body", TEXT);
        let schema = builder.build();
        (
            schema,
            Self {
                doc_index,
                id,
                selector,
                source_path,
                title,
                summary,
                kind,
                tags,
                related,
                headings,
                body,
            },
        )
    }

    fn search_fields(self) -> Vec<Field> {
        vec![
            self.id,
            self.selector,
            self.source_path,
            self.title,
            self.summary,
            self.kind,
            self.tags,
            self.related,
            self.headings,
            self.body,
        ]
    }
}

#[derive(Debug, Clone)]
struct KnowledgeIndexedDocument {
    manifest: KnowledgeDocManifest,
    headings: String,
    body: String,
    related: String,
}

impl KnowledgeIndexedDocument {
    fn new(manifest: KnowledgeDocManifest, body: String) -> Self {
        let headings = extract_markdown_headings(&body);
        let related = manifest
            .related
            .iter()
            .map(|edge| format!("{} {}", edge.edge_type.as_str(), edge.target))
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            manifest,
            headings,
            body,
            related,
        }
    }

    fn tantivy_document(&self, doc_index: u64, fields: KnowledgeSearchSchema) -> TantivyDocument {
        doc!(
            fields.doc_index => doc_index,
            fields.id => self.manifest.id.as_str(),
            fields.selector => self.manifest.selector.as_str(),
            fields.source_path => self.manifest.source_path.as_str(),
            fields.title => self.manifest.title.as_str(),
            fields.summary => self.manifest.summary.as_str(),
            fields.kind => self.manifest.kind.as_str(),
            fields.tags => self.manifest.tags.join(" "),
            fields.related => self.related.as_str(),
            fields.headings => self.headings.as_str(),
            fields.body => self.body.as_str(),
        )
    }

    fn matched_fields(&self, query: &str) -> Vec<String> {
        let mut matched = BTreeSet::new();
        record_match("id", &self.manifest.id, query, &mut matched);
        record_match("selector", &self.manifest.selector, query, &mut matched);
        record_match(
            "source_path",
            &self.manifest.source_path,
            query,
            &mut matched,
        );
        record_match("title", &self.manifest.title, query, &mut matched);
        record_match("summary", &self.manifest.summary, query, &mut matched);
        record_match("kind", self.manifest.kind.as_str(), query, &mut matched);
        record_match("tag", &self.manifest.tags.join(" "), query, &mut matched);
        record_match("related", &self.related, query, &mut matched);
        record_match("heading", &self.headings, query, &mut matched);
        record_match("content", &self.body, query, &mut matched);
        if matched.is_empty() {
            matched.insert("content".to_string());
        }
        matched.into_iter().collect()
    }
}

fn metadata_fingerprint(manifest: &dyn KnowledgePackManifest) -> String {
    let mut hasher = DefaultHasher::new();
    manifest.pack_id().hash(&mut hasher);
    manifest.version().hash(&mut hasher);
    manifest.root_uri().hash(&mut hasher);
    manifest.content_hash().hash(&mut hasher);
    for doc in manifest.docs() {
        doc.id.hash(&mut hasher);
        doc.selector.hash(&mut hasher);
        doc.source_path.hash(&mut hasher);
        doc.title.hash(&mut hasher);
        doc.summary.hash(&mut hasher);
        doc.kind.hash(&mut hasher);
        doc.tags.hash(&mut hasher);
        doc.updated_at.hash(&mut hasher);
        for edge in &doc.related {
            edge.edge_type.as_str().hash(&mut hasher);
            edge.target.hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn extract_markdown_headings(content: &str) -> String {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let heading = line.strip_prefix('#')?;
            Some(heading.trim_start_matches('#').trim())
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn exact_match_label(doc: &KnowledgeDocManifest, query: &str) -> String {
    if doc.id == query {
        "id"
    } else if doc.selector == query {
        "selector"
    } else if doc.source_path == query {
        "source_path"
    } else {
        "selector"
    }
    .to_string()
}

fn record_match(label: &str, haystack: &str, query: &str, matched: &mut BTreeSet<String>) {
    if field_matches_query(haystack, query) {
        matched.insert(label.to_string());
    }
}

fn field_matches_query(haystack: &str, query: &str) -> bool {
    let haystack = search_normalize(haystack);
    let query = search_normalize(query);
    if query.is_empty() || haystack.is_empty() {
        return false;
    }
    if haystack.contains(&query) {
        return true;
    }
    let tokens = query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    !tokens.is_empty() && tokens.iter().all(|token| haystack.contains(token))
}

fn search_normalize(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut last_was_space = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            last_was_space = false;
        } else if !last_was_space {
            normalized.push(' ');
            last_was_space = true;
        }
    }
    normalized.trim().to_string()
}

fn tantivy_score(score: f32) -> usize {
    let score = (score * 100.0).round();
    if score.is_sign_negative() {
        0
    } else {
        score as usize
    }
}

fn elapsed_ms(started: Instant) -> u128 {
    started.elapsed().as_millis()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::KnowledgeSearchService;
    use crate::{
        KnowledgeDocEdge, KnowledgeDocFilter, KnowledgeDocKind, KnowledgeDocManifest,
        KnowledgePack, KnowledgePackManifest, KnowledgePackManifestData, PackageKnowledgePack,
    };

    struct TestPack {
        manifest: KnowledgePackManifestData,
        content: HashMap<String, String>,
    }

    impl KnowledgePack for TestPack {
        fn manifest(&self) -> &dyn KnowledgePackManifest {
            &self.manifest
        }

        fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>> {
            self.content
                .get(&manifest.id)
                .map(|content| Cow::Borrowed(content.as_str()))
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime")
    }

    fn doc(id: &str, selector: &str, title: &str, summary: &str) -> KnowledgeDocManifest {
        KnowledgeDocManifest {
            id: id.to_string(),
            selector: selector.to_string(),
            source_path: format!("docs/{}.md", selector.replace('.', "/")),
            title: title.to_string(),
            summary: summary.to_string(),
            kind: KnowledgeDocKind::new("guide"),
            tags: Vec::new(),
            related: Vec::<KnowledgeDocEdge>::new(),
            updated_at: String::new(),
        }
    }

    fn pack() -> Arc<dyn KnowledgePack> {
        let docs = vec![
            doc(
                "routine-flow",
                "building.routine_flow_authoring",
                "Routine Flow",
                "Author routines with graph structure.",
            ),
            doc(
                "agents",
                "resources.agents",
                "Agents",
                "Resource-level model for agents.",
            ),
        ];
        Arc::new(TestPack {
            manifest: KnowledgePackManifestData {
                pack_id: "test".into(),
                version: "1".into(),
                schema_version: 1,
                root_uri: "test://knowledge/".into(),
                content_hash: "body-search-fixture".into(),
                docs,
            },
            content: HashMap::from([
                (
                    "routine-flow".into(),
                    "# Routine Flow Authoring\n\nUse metadata.handoff_schema to define the enforced handoff schema for every edge."
                        .into(),
                ),
                (
                    "agents".into(),
                    "# Agents\n\nAgents own behavior and tool access.".into(),
                ),
            ]),
        })
    }

    fn package_pack() -> (PathBuf, Arc<dyn KnowledgePack>) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "nenjo-knowledge-search-package-{pid}-{unique}",
            pid = std::process::id()
        ));
        let docs_dir = dir.join("docs/package");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(
            dir.join("manifest.yaml"),
            r#"
schema: nenjo.knowledge.v1
manifest:
  pack_id: nenjo.package
  selector: pkg:nenjo-ai.packages.knowledge.search_test
  version: 0.1.0
  docs:
    - selector: package.body_search
      source_path: docs/package/body-search.md
      title: Package Body Search
      summary: Package-backed knowledge document.
      kind: reference
      tags: [package:test]
      related: []
"#,
        )
        .unwrap();
        std::fs::write(
            docs_dir.join("body-search.md"),
            "# Package Body Search\n\nThis body contains the package-only retrieval phrase.",
        )
        .unwrap();

        let pack = PackageKnowledgePack::load(&dir.join("manifest.yaml"), "0.1.0").unwrap();
        (dir, Arc::new(pack))
    }

    #[test]
    fn tantivy_search_finds_body_only_terms() {
        runtime().block_on(async {
            let service = KnowledgeSearchService::new();
            let hits = service
                .search(
                    "local:test",
                    pack(),
                    "handoff schema",
                    KnowledgeDocFilter::default(),
                )
                .await;

            assert!(!hits.is_empty());
            assert_eq!(hits[0].document.selector, "building.routine_flow_authoring");
            assert!(hits[0].matched.iter().any(|field| field == "content"));
        });
    }

    #[test]
    fn tantivy_search_reuses_cached_index_and_reports_build_stats() {
        runtime().block_on(async {
            let service = KnowledgeSearchService::new();
            let (_, cold_stats) = service
                .search_with_stats(
                    "local:test",
                    pack(),
                    "handoff schema",
                    KnowledgeDocFilter::default(),
                )
                .await;
            let (_, warm_stats) = service
                .search_with_stats(
                    "local:test",
                    pack(),
                    "tool access",
                    KnowledgeDocFilter::default(),
                )
                .await;

            assert!(!cold_stats.cache_hit);
            assert!(cold_stats.build_stats.is_some());
            assert!(warm_stats.cache_hit);
            assert!(warm_stats.build_stats.is_none());
            assert!(!warm_stats.fallback_used);
        });
    }

    #[test]
    fn tantivy_search_indexes_package_pack_bodies() {
        runtime().block_on(async {
            let (dir, pack) = package_pack();
            let service = KnowledgeSearchService::new();
            let hits = service
                .search(
                    "pkg:nenjo-ai.packages.knowledge.search_test",
                    pack,
                    "package-only retrieval phrase",
                    KnowledgeDocFilter::default(),
                )
                .await;

            std::fs::remove_dir_all(dir).unwrap();

            assert!(!hits.is_empty());
            assert_eq!(hits[0].document.selector, "package.body_search");
            assert!(hits[0].matched.iter().any(|field| field == "content"));
        });
    }

    #[test]
    #[ignore = "prints rebuild/search timing data for local performance inspection"]
    fn search_index_rebuild_cost() {
        runtime().block_on(async {
            for (name, docs, body_repeats) in [
                ("synthetic:small", 100, 8),
                ("synthetic:medium", 1_000, 12),
            ] {
                let pack = synthetic_pack(docs, body_repeats);
                let service = KnowledgeSearchService::new();
                let (_, cold_stats) = service
                    .search_with_stats(
                        name,
                        pack.clone(),
                        "handoff schema retry",
                        KnowledgeDocFilter::default(),
                    )
                    .await;
                let (_, warm_stats) = service
                    .search_with_stats(
                        name,
                        pack,
                        "permission scope",
                        KnowledgeDocFilter::default(),
                    )
                    .await;
                let build = cold_stats.build_stats.expect("cold build stats");
                println!(
                    "{name}\tdocs={}\tbytes={}\tbuild_ms={}\tadd_docs_ms={}\tcommit_ms={}\tcold_search_ms={}\twarm_search_ms={}",
                    build.indexed_document_count,
                    build.total_content_bytes,
                    build.total_build_millis,
                    build.tantivy_add_docs_millis,
                    build.tantivy_commit_millis,
                    cold_stats.search_millis,
                    warm_stats.search_millis,
                );
            }
        });
    }

    fn synthetic_pack(doc_count: usize, body_repeats: usize) -> Arc<dyn KnowledgePack> {
        let mut docs = Vec::new();
        let mut content = HashMap::new();
        for index in 0..doc_count {
            let id = format!("doc-{index}");
            let selector = format!("synthetic.doc_{index}");
            docs.push(doc(
                &id,
                &selector,
                &format!("Synthetic Doc {index}"),
                "Synthetic search measurement document.",
            ));
            let body = "handoff schema retry permission scope routine flow template vars\n"
                .repeat(body_repeats);
            content.insert(id, body);
        }
        Arc::new(TestPack {
            manifest: KnowledgePackManifestData {
                pack_id: "synthetic".into(),
                version: "1".into(),
                schema_version: 1,
                root_uri: "synthetic://knowledge/".into(),
                content_hash: format!("synthetic-{doc_count}-{body_repeats}"),
                docs,
            },
            content,
        })
    }
}
