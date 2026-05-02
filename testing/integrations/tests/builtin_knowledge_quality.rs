//! Intent-driven builtin knowledge quality evals using a real LLM provider.
//!
//! Requires `OPENROUTER_API_KEY`. Tests are skipped automatically if the key is
//! not set. Set `NENJO_KG_EVAL_MODEL` to override the default OpenRouter model.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Once};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use uuid::Uuid;

use nenjo::builtin_knowledge::{BuiltinDocFilter, BuiltinDocSearchHit, builtin_knowledge_pack};
use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, PromptTemplates,
};
use nenjo::provider::{ModelProviderFactory, Provider, ToolFactory};
use nenjo_models::ModelProvider;
use nenjo_models::openrouter::OpenRouterProvider;
use nenjo_tools::{Tool, ToolCategory, ToolResult};

// ---------------------------------------------------------------------------
// Provider and manifest helpers
// ---------------------------------------------------------------------------

struct OpenRouterFactory {
    api_key: String,
}

impl ModelProviderFactory for OpenRouterFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(OpenRouterProvider::new(Some(&self.api_key))) as Arc<dyn ModelProvider>)
    }
}

fn get_api_key() -> Option<String> {
    match std::env::var("OPENROUTER_API_KEY") {
        Ok(key) if !key.is_empty() => Some(key),
        _ => None,
    }
}

fn init_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "nenjo=debug,nenjo_models=debug,nenjo_integration_tests=debug,info".to_string()
        });
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

fn eval_model_name() -> String {
    std::env::var("NENJO_KG_EVAL_MODEL").unwrap_or_else(|_| "~moonshotai/kimi-latest".to_string())
}

fn make_model() -> ModelManifest {
    ModelManifest {
        id: Uuid::new_v4(),
        name: "builtin-kg-eval-model".into(),
        description: None,
        model: eval_model_name(),
        model_provider: "openrouter".into(),
        temperature: Some(0.0),
        base_url: None,
    }
}

fn make_project() -> ProjectManifest {
    ProjectManifest {
        id: Uuid::new_v4(),
        name: "builtin-kg-eval".into(),
        slug: "builtin-kg-eval".into(),
        description: None,
        settings: Value::Null,
    }
}

fn make_agent(model_id: Uuid) -> AgentManifest {
    AgentManifest {
        id: Uuid::new_v4(),
        name: "builtin-kg-eval-agent".into(),
        description: Some("Intent-driven builtin knowledge eval agent".into()),
        prompt_config: PromptConfig {
            system_prompt: INTENT_GRAPH_EXPAND_PROMPT.into(),
            templates: PromptTemplates {
                chat_task: "{{ chat.message }}".into(),
                task_execution: String::new(),
                gate_eval: String::new(),
                cron_task: String::new(),
                ..Default::default()
            },
            ..Default::default()
        },
        color: None,
        model_id: Some(model_id),
        domain_ids: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        ability_ids: vec![],
        prompt_locked: false,
        heartbeat: None,
    }
}

const INTENT_GRAPH_EXPAND_PROMPT: &str = r#"
You answer questions about Nenjo by using builtin knowledge tools.

For every user intent:
1. Classify the intent into likely Nenjo concepts and resource families.
2. Use search_builtin_doc_paths to find compact seed documents.
3. Use read_builtin_doc_manifest on the best seed documents.
4. You MUST call list_builtin_doc_neighbors at least once on a best seed document
   before answering. If the neighbors are not useful, say that after inspecting them.
5. Read the final selected documents with read_builtin_doc before answering.
6. Answer as an implementation-oriented plan.

Prefer graph expansion when the user asks how concepts relate, what to use
together, or how to structure agents, routines, memory, scopes, councils,
domains, abilities, projects, or tasks. Name the builtin documents or concepts
you used in a short "Knowledge used:" sentence at the end.

{{builtin_documents}}
"#;

// ---------------------------------------------------------------------------
// Eval-local builtin tools with call logging
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum BuiltinToolKind {
    SearchPaths,
    ReadDoc,
    ReadManifest,
    Neighbors,
    ListTree,
}

#[derive(Debug, Clone)]
struct ToolCallLog {
    name: String,
    args: Value,
    doc_ids: Vec<String>,
    targets: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct EvalLog {
    calls: Arc<Mutex<Vec<ToolCallLog>>>,
}

impl EvalLog {
    fn record(&self, call: ToolCallLog) {
        self.calls.lock().expect("eval log poisoned").push(call);
    }

    fn snapshot(&self) -> Vec<ToolCallLog> {
        self.calls.lock().expect("eval log poisoned").clone()
    }
}

struct BuiltinEvalTool {
    kind: BuiltinToolKind,
    log: EvalLog,
}

impl BuiltinEvalTool {
    fn new(kind: BuiltinToolKind, log: EvalLog) -> Self {
        Self { kind, log }
    }
}

#[async_trait::async_trait]
impl Tool for BuiltinEvalTool {
    fn name(&self) -> &str {
        match self.kind {
            BuiltinToolKind::SearchPaths => "search_builtin_doc_paths",
            BuiltinToolKind::ReadDoc => "read_builtin_doc",
            BuiltinToolKind::ReadManifest => "read_builtin_doc_manifest",
            BuiltinToolKind::Neighbors => "list_builtin_doc_neighbors",
            BuiltinToolKind::ListTree => "list_builtin_doc_tree",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            BuiltinToolKind::SearchPaths => {
                "Search builtin Nenjo docs and return compact metadata without body content."
            }
            BuiltinToolKind::ReadDoc => {
                "Read one full builtin Nenjo doc by id or builtin://nenjo/ path."
            }
            BuiltinToolKind::ReadManifest => {
                "Read one builtin Nenjo doc manifest by id or builtin://nenjo/ path."
            }
            BuiltinToolKind::Neighbors => {
                "List graph neighbors for one builtin Nenjo doc by id or path. Each neighbor is returned once with nested edges containing source, target, edge type, and note."
            }
            BuiltinToolKind::ListTree => "List the builtin Nenjo document tree.",
        }
    }

    fn parameters_schema(&self) -> Value {
        match self.kind {
            BuiltinToolKind::SearchPaths => json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "path_prefix": { "type": "string" },
                    "related_to": { "type": "string" },
                    "edge_type": {
                        "type": "string",
                        "enum": ["part_of", "defines", "governs", "classifies", "references", "depends_on", "extends", "related_to"]
                    }
                },
                "required": ["query"]
            }),
            BuiltinToolKind::ReadDoc | BuiltinToolKind::ReadManifest => json!({
                "type": "object",
                "properties": {
                    "id_or_path": { "type": "string" }
                },
                "required": ["id_or_path"]
            }),
            BuiltinToolKind::Neighbors => json!({
                "type": "object",
                "properties": {
                    "id_or_path": { "type": "string" },
                    "edge_type": {
                        "type": "string",
                        "enum": ["part_of", "defines", "governs", "classifies", "references", "depends_on", "extends", "related_to"]
                    }
                },
                "required": ["id_or_path"]
            }),
            BuiltinToolKind::ListTree => json!({
                "type": "object",
                "properties": {
                    "prefix": { "type": "string" }
                }
            }),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let pack = builtin_knowledge_pack();
        let output = match self.kind {
            BuiltinToolKind::SearchPaths => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("missing query"))?;
                let hits = pack.search_paths(query, filter_from_args(&args)?);
                let doc_ids = hits.iter().map(|hit| hit.id.clone()).collect();
                self.log.record(ToolCallLog {
                    name: self.name().to_string(),
                    args: args.clone(),
                    doc_ids,
                    targets: Vec::new(),
                });
                serde_json::to_value(compact_hits(hits))?
            }
            BuiltinToolKind::ReadDoc => {
                let id_or_path = id_or_path_arg(&args)?;
                let doc = pack
                    .read_doc(id_or_path)
                    .ok_or_else(|| anyhow!("unknown builtin doc {id_or_path}"))?;
                self.log.record(ToolCallLog {
                    name: self.name().to_string(),
                    args: args.clone(),
                    doc_ids: vec![doc.manifest.id.clone()],
                    targets: Vec::new(),
                });
                serde_json::to_value(doc)?
            }
            BuiltinToolKind::ReadManifest => {
                let id_or_path = id_or_path_arg(&args)?;
                let manifest = pack
                    .read_manifest(id_or_path)
                    .ok_or_else(|| anyhow!("unknown builtin doc {id_or_path}"))?;
                self.log.record(ToolCallLog {
                    name: self.name().to_string(),
                    args: args.clone(),
                    doc_ids: vec![manifest.id.clone()],
                    targets: Vec::new(),
                });
                serde_json::to_value(manifest)?
            }
            BuiltinToolKind::Neighbors => {
                let id_or_path = id_or_path_arg(&args)?;
                let edge_type = args
                    .get("edge_type")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .context("invalid edge_type")?;
                let neighbors = pack.neighbors(id_or_path, edge_type);
                let targets = neighbors
                    .iter()
                    .map(|neighbor| neighbor.target.clone())
                    .collect();
                self.log.record(ToolCallLog {
                    name: self.name().to_string(),
                    args: args.clone(),
                    doc_ids: Vec::new(),
                    targets,
                });
                serde_json::to_value(neighbors)?
            }
            BuiltinToolKind::ListTree => {
                let prefix = args.get("prefix").and_then(Value::as_str);
                let tree = pack.list_tree(prefix);
                self.log.record(ToolCallLog {
                    name: self.name().to_string(),
                    args: args.clone(),
                    doc_ids: Vec::new(),
                    targets: tree
                        .entries
                        .iter()
                        .map(|entry| entry.path.clone())
                        .collect(),
                });
                serde_json::to_value(tree)?
            }
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }
}

struct BuiltinEvalToolFactory {
    log: EvalLog,
}

#[async_trait::async_trait]
impl ToolFactory for BuiltinEvalToolFactory {
    async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(BuiltinEvalTool::new(
                BuiltinToolKind::SearchPaths,
                self.log.clone(),
            )),
            Arc::new(BuiltinEvalTool::new(
                BuiltinToolKind::ReadDoc,
                self.log.clone(),
            )),
            Arc::new(BuiltinEvalTool::new(
                BuiltinToolKind::ReadManifest,
                self.log.clone(),
            )),
            Arc::new(BuiltinEvalTool::new(
                BuiltinToolKind::Neighbors,
                self.log.clone(),
            )),
            Arc::new(BuiltinEvalTool::new(
                BuiltinToolKind::ListTree,
                self.log.clone(),
            )),
        ]
    }
}

fn id_or_path_arg(args: &Value) -> Result<&str> {
    args.get("id_or_path")
        .or_else(|| args.get("id"))
        .or_else(|| args.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing id_or_path"))
}

fn filter_from_args(args: &Value) -> Result<BuiltinDocFilter> {
    let tags = args
        .get("tags")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    let edge_type = args
        .get("edge_type")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("invalid edge_type")?;

    Ok(BuiltinDocFilter {
        tags,
        path_prefix: args
            .get("path_prefix")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        related_to: args
            .get("related_to")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        edge_type,
        ..Default::default()
    })
}

fn compact_hits(hits: Vec<BuiltinDocSearchHit>) -> Vec<Value> {
    hits.into_iter()
        .take(8)
        .map(|hit| {
            json!({
                "id": hit.id,
                "virtual_path": hit.virtual_path,
                "title": hit.title,
                "summary": hit.summary,
                "kind": hit.kind,
                "authority": hit.authority,
                "tags": hit.tags,
                "score": hit.score,
                "matched": hit.matched
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Intent cases and scoring
// ---------------------------------------------------------------------------

struct IntentCase {
    id: &'static str,
    intent: &'static str,
    expected_docs: &'static [&'static str],
    required_answer_terms: &'static [&'static str],
    requires_graph: bool,
}

const CASES: &[IntentCase] = &[
    // IntentCase {
    //     id: "build_review_gate_routine",
    //     intent: "I want to build a routine that drafts release notes, runs a review gate, and publishes only if the gate passes. What Nenjo concepts should I use and how should I structure it?",
    //     expected_docs: &["nenjo.guide.routines", "nenjo.taxonomy.workflow_patterns"],
    //     required_answer_terms: &["routine", "gate", "pass", "fail", "terminal"],
    //     requires_graph: true,
    // },
    IntentCase {
        id: "agent_for_code_review",
        intent: "I need an agent that can review pull requests, use reusable review behavior, and only access the tools it needs. What should I configure?",
        expected_docs: &[
            "nenjo.guide.agents",
            "nenjo.guide.abilities",
            "nenjo.guide.scopes",
        ],
        required_answer_terms: &["agent", "ability", "scope", "tool"],
        requires_graph: true,
    },
    IntentCase {
        id: "project_knowledge_for_agent",
        intent: "I need project knowledge available to an agent without confusing it with long-term memory. Which Nenjo concepts should I use?",
        expected_docs: &[
            "nenjo.guide.context_blocks",
            "nenjo.guide.memory",
            "nenjo.guide.projects",
        ],
        required_answer_terms: &["project", "context", "memory", "knowledge"],
        requires_graph: true,
    },
];

#[tokio::test]
async fn builtin_knowledge_intent_graph_expand_quality() {
    init_tracing();

    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();
    let agent = make_agent(model.id);
    let project = make_project();
    let log = EvalLog::default();
    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![project],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(BuiltinEvalToolFactory { log: log.clone() })
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("builtin-kg-eval-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let mut failures = Vec::new();
    for case in CASES {
        let before = log.snapshot().len();
        let output = runner
            .chat(case.intent)
            .await
            .unwrap_or_else(|error| panic!("{} chat failed: {error}", case.id));
        let calls = log.snapshot().into_iter().skip(before).collect::<Vec<_>>();
        let score = score_case(case, &calls, &output.text);

        println!(
            "{}",
            serde_json::to_string(&json!({
                "case_id": case.id,
                "pattern": "intent_graph_expand",
                "tool_calls": output.tool_calls,
                "retrieved_docs": score.retrieved_docs,
                "neighbor_targets": score.neighbor_targets,
                "doc_recall": score.doc_recall,
                "answer_term_recall": score.answer_term_recall,
                "used_search": score.used_search,
                "used_neighbors": score.used_neighbors,
                "used_read_doc": score.used_read_doc,
                "calls": calls.iter().map(|call| {
                    json!({
                        "name": call.name,
                        "args": call.args,
                        "doc_ids": call.doc_ids,
                        "targets": call.targets,
                    })
                }).collect::<Vec<_>>(),
                "answer": output.text,
            }))
            .unwrap()
        );

        if output.tool_calls < 2 {
            failures.push(format!(
                "{}: expected at least 2 tool calls, got {}",
                case.id, output.tool_calls
            ));
        }
        if !score.used_search {
            failures.push(format!(
                "{}: did not use search_builtin_doc_paths; calls={:?}",
                case.id,
                call_names(&calls)
            ));
        }
        if case.requires_graph && !score.used_neighbors {
            failures.push(format!(
                "{}: did not use list_builtin_doc_neighbors; calls={:?}",
                case.id,
                call_names(&calls)
            ));
        }
        if !score.used_read_doc {
            failures.push(format!(
                "{}: did not read final document content; calls={:?}",
                case.id,
                call_names(&calls)
            ));
        }
        if score.doc_recall < 0.5 {
            failures.push(format!(
                "{}: doc recall too low: {:.2}; retrieved {:?}",
                case.id, score.doc_recall, score.retrieved_docs
            ));
        }
        if score.answer_term_recall < 0.75 {
            failures.push(format!(
                "{}: answer term recall too low: {:.2}; answer {}",
                case.id, score.answer_term_recall, output.text
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[derive(Debug)]
struct CaseScore {
    retrieved_docs: Vec<String>,
    neighbor_targets: Vec<String>,
    doc_recall: f64,
    answer_term_recall: f64,
    used_search: bool,
    used_neighbors: bool,
    used_read_doc: bool,
}

fn call_names(calls: &[ToolCallLog]) -> Vec<String> {
    calls.iter().map(|call| call.name.clone()).collect()
}

fn score_case(case: &IntentCase, calls: &[ToolCallLog], answer: &str) -> CaseScore {
    let mut retrieved_docs = BTreeSet::new();
    let mut neighbor_targets = BTreeSet::new();
    let mut used_search = false;
    let mut used_neighbors = false;
    let mut used_read_doc = false;

    for call in calls {
        match call.name.as_str() {
            "search_builtin_doc_paths" => used_search = true,
            "list_builtin_doc_neighbors" => used_neighbors = true,
            "read_builtin_doc" => used_read_doc = true,
            _ => {}
        }
        for doc_id in &call.doc_ids {
            retrieved_docs.insert(doc_id.clone());
        }
        for target in &call.targets {
            neighbor_targets.insert(target.clone());
            if let Some(doc) = builtin_knowledge_pack().read_manifest(target) {
                retrieved_docs.insert(doc.id.clone());
            }
        }
    }

    let retrieved_docs = retrieved_docs.into_iter().collect::<Vec<_>>();
    let neighbor_targets = neighbor_targets.into_iter().collect::<Vec<_>>();
    let expected_hits = case
        .expected_docs
        .iter()
        .filter(|expected| retrieved_docs.iter().any(|doc| doc == **expected))
        .count();
    let answer = answer.to_lowercase();
    let answer_hits = case
        .required_answer_terms
        .iter()
        .filter(|term| answer.contains(&term.to_lowercase()))
        .count();

    CaseScore {
        retrieved_docs,
        neighbor_targets,
        doc_recall: expected_hits as f64 / case.expected_docs.len() as f64,
        answer_term_recall: answer_hits as f64 / case.required_answer_terms.len() as f64,
        used_search,
        used_neighbors,
        used_read_doc,
    }
}
