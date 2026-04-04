//! Tests for memory integration with Provider and AgentRunner.

use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use nenjo::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use nenjo::memory::{MarkdownMemory, MemoryScope};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo_models::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};

// ---------------------------------------------------------------------------
// Mock Provider
// ---------------------------------------------------------------------------

struct MockProvider {
    response_text: String,
}

impl MockProvider {
    fn new(text: &str) -> Self {
        Self {
            response_text: text.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some(self.response_text.clone()),
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            },
        })
    }

    fn context_window(&self, _model: &str) -> Option<usize> {
        Some(128_000)
    }
}

struct MockModelProviderFactory {
    response_text: String,
}

impl ModelProviderFactory for MockModelProviderFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(MockProvider::new(&self.response_text)))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_manifest() -> Manifest {
    let model = ModelManifest {
        id: Uuid::new_v4(),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        tags: vec![],
    };

    let agent = AgentManifest {
        id: Uuid::new_v4(),
        name: "memory-agent".into(),
        description: Some("An agent with memory".into()),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": "You are a helpful assistant.\n{{ memories }}",
            "templates": {
                "chat_task": "{{ chat.message }}",
                "task_execution": "",
                "gate_eval": "",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model.id),
        model_name: Some("test-model".into()),
        skills: vec![],
        domains: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        abilities: vec![],
    };

    let project = ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    };

    Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![project],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn provider_with_memory_adds_tools() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path(), ws_dir.path());

    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory {
            response_text: "ok".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .build()
        .unwrap();

    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"memory_store"), "should have memory_store");
    assert!(
        names.contains(&"memory_recall"),
        "should have memory_recall"
    );
    assert!(
        names.contains(&"memory_forget"),
        "should have memory_forget"
    );
    assert!(
        names.contains(&"resource_save"),
        "should have resource_save"
    );
    assert!(
        names.contains(&"resource_read"),
        "should have resource_read"
    );
    assert!(
        names.contains(&"resource_delete"),
        "should have resource_delete"
    );
}

#[tokio::test]
async fn provider_without_memory_has_no_memory_tools() {
    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory {
            response_text: "ok".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .build()
        .unwrap();

    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(!names.contains(&"memory_store"));
}

#[tokio::test]
async fn memory_store_and_recall() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));
    let scope = MemoryScope::new("test-agent", Some("test-project"));

    use nenjo::memory::Memory;

    // Store facts
    memory
        .append(&scope.project, "preferences", "User prefers Rust")
        .await
        .unwrap();
    memory
        .append(&scope.project, "preferences", "Always use snake_case")
        .await
        .unwrap();
    memory
        .append(&scope.core, "expertise", "Distributed systems")
        .await
        .unwrap();

    // Recall by category
    let cat = memory
        .read_category(&scope.project, "preferences")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cat.facts.len(), 2);
    assert_eq!(cat.facts[0].text, "User prefers Rust");

    // List all categories
    let cats = memory.list_categories(&scope.project).await.unwrap();
    assert_eq!(cats.len(), 1);
    assert_eq!(cats[0].category, "preferences");

    let core_cats = memory.list_categories(&scope.core).await.unwrap();
    assert_eq!(core_cats.len(), 1);
    assert_eq!(core_cats[0].category, "expertise");
}

#[tokio::test]
async fn memory_forget() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));
    let scope = MemoryScope::new("test-agent", Some("test-project"));

    use nenjo::memory::Memory;

    memory
        .append(&scope.project, "prefs", "Likes Rust")
        .await
        .unwrap();
    memory
        .append(&scope.project, "prefs", "Likes Go")
        .await
        .unwrap();

    assert!(
        memory
            .delete_fact(&scope.project, "prefs", "Likes Rust")
            .await
            .unwrap()
    );

    let cat = memory
        .read_category(&scope.project, "prefs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cat.facts.len(), 1);
    assert_eq!(cat.facts[0].text, "Likes Go");
}

#[tokio::test]
async fn memory_vars_injected_into_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));
    let scope = MemoryScope::new("memory-agent", Some("test-project"));

    use nenjo::memory::Memory;

    // Store facts in each tier
    memory
        .append(&scope.core, "expertise", "Distributed systems expert")
        .await
        .unwrap();
    memory
        .append(&scope.project, "preferences", "User prefers Rust")
        .await
        .unwrap();
    memory
        .append(&scope.shared, "decisions", "Using PostgreSQL for DB")
        .await
        .unwrap();

    // Build memory vars
    let vars = nenjo::memory::build_memory_vars(memory.as_ref(), &scope)
        .await
        .unwrap();

    let full = vars.get("memories").expect("should have memories key");
    assert!(full.contains("<memories>"), "should have memories root tag");
    assert!(full.contains("<memories-core>"), "should have core tier");
    assert!(
        full.contains("<memories-project>"),
        "should have project tier"
    );
    assert!(
        full.contains("<memories-shared>"),
        "should have shared tier"
    );
    assert!(
        full.contains("User prefers Rust"),
        "should contain project fact"
    );
    assert!(
        full.contains("Distributed systems"),
        "should contain core fact"
    );
    assert!(full.contains("PostgreSQL"), "should contain shared fact");

    // Individual tiers
    assert!(vars.contains_key("memories.core"), "should have core key");
    assert!(
        vars.contains_key("memories.project"),
        "should have project key"
    );
    assert!(
        vars.contains_key("memories.shared"),
        "should have shared key"
    );
}

#[tokio::test]
async fn memory_vars_empty_when_no_facts() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));
    let scope = MemoryScope::new("empty-agent", Some("empty-project"));

    let vars = nenjo::memory::build_memory_vars(memory.as_ref(), &scope)
        .await
        .unwrap();

    assert!(vars.is_empty(), "should be empty when no facts exist");
}

#[tokio::test]
async fn resource_vars_injected() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));
    let scope = MemoryScope::new("test-agent", Some("test-project"));

    use nenjo::memory::Memory;

    memory
        .save_resource(
            &scope.resources_project,
            "auth-prd.md",
            "Auth PRD",
            "architect",
            "# Auth PRD\nOAuth2 flow",
        )
        .await
        .unwrap();
    memory
        .save_resource(
            &scope.resources_global,
            "standards.md",
            "Coding standards",
            "system",
            "# Standards\nUse Rust",
        )
        .await
        .unwrap();

    let vars = nenjo::memory::build_resource_vars(memory.as_ref(), &scope)
        .await
        .unwrap();

    assert!(vars.contains_key("resources"), "should have resources key");
    assert!(
        vars.contains_key("resources.project"),
        "should have project key"
    );
    assert!(
        vars.contains_key("resources.workspace"),
        "should have workspace key"
    );

    let full = &vars["resources"];
    assert!(full.contains("auth-prd.md"));
    assert!(full.contains("standards.md"));
    assert!(full.contains("architect"));
}

// ---------------------------------------------------------------------------
// Scope isolation tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scope_project_agent_three_tiers_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));

    use nenjo::memory::Memory;

    let scope = MemoryScope::new("coder", Some("webapp"));

    // Store in each tier
    memory
        .append(&scope.project, "prefs", "project-only")
        .await
        .unwrap();
    memory
        .append(&scope.core, "prefs", "core-only")
        .await
        .unwrap();
    memory
        .append(&scope.shared, "prefs", "shared-only")
        .await
        .unwrap();

    // Build memory vars — all three tiers present
    let vars = nenjo::memory::build_memory_vars(memory.as_ref(), &scope)
        .await
        .unwrap();
    let full = &vars["memories"];

    assert!(full.contains("project-only"));
    assert!(full.contains("core-only"));
    assert!(full.contains("shared-only"));

    // Each tier is separate in vars
    assert!(vars["memories.project"].contains("project-only"));
    assert!(!vars["memories.project"].contains("core-only"));

    assert!(vars["memories.core"].contains("core-only"));
    assert!(!vars["memories.core"].contains("project-only"));

    assert!(vars["memories.shared"].contains("shared-only"));
    assert!(!vars["memories.shared"].contains("project-only"));
}

#[tokio::test]
async fn scope_system_agent_collapses_to_core() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));

    use nenjo::memory::Memory;

    let scope = MemoryScope::new("nenji", None);

    // Project and core both write to agent_nenji_core
    memory
        .append(&scope.project, "prefs", "fact-a")
        .await
        .unwrap();
    memory.append(&scope.core, "prefs", "fact-b").await.unwrap();

    let vars = nenjo::memory::build_memory_vars(memory.as_ref(), &scope)
        .await
        .unwrap();

    // Project and core should contain both facts (same underlying dir)
    let project_xml = &vars["memories.project"];
    assert!(project_xml.contains("fact-a"));
    assert!(project_xml.contains("fact-b"));

    let core_xml = &vars["memories.core"];
    assert!(core_xml.contains("fact-a"));
    assert!(core_xml.contains("fact-b"));
}

#[tokio::test]
async fn scope_shared_visible_across_agents() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));

    use nenjo::memory::Memory;

    let scope_coder = MemoryScope::new("coder", Some("webapp"));
    let scope_reviewer = MemoryScope::new("reviewer", Some("webapp"));

    // Coder stores a shared fact
    memory
        .append(&scope_coder.shared, "conventions", "Always write tests")
        .await
        .unwrap();

    // Reviewer can see it via their shared scope (same project)
    let vars = nenjo::memory::build_memory_vars(memory.as_ref(), &scope_reviewer)
        .await
        .unwrap();
    assert!(vars["memories.shared"].contains("Always write tests"));

    // But reviewer can't see coder's project-scoped memories
    let reviewer_project = memory
        .list_categories(&scope_reviewer.project)
        .await
        .unwrap();
    assert!(reviewer_project.is_empty());
}

#[tokio::test]
async fn scope_resources_shared_across_agents() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));

    use nenjo::memory::Memory;

    let scope_architect = MemoryScope::new("architect", Some("webapp"));
    let scope_coder = MemoryScope::new("coder", Some("webapp"));

    // Architect saves a project resource
    memory
        .save_resource(
            &scope_architect.resources_project,
            "design.md",
            "System design",
            "architect",
            "# Design doc",
        )
        .await
        .unwrap();

    // Coder can see it (same project resources path)
    let vars = nenjo::memory::build_resource_vars(memory.as_ref(), &scope_coder)
        .await
        .unwrap();
    assert!(vars["resources.project"].contains("design.md"));
    assert!(vars["resources.project"].contains("architect"));

    // Coder can read the full content
    let content = memory
        .read_resource(&scope_coder.resources_project, "design.md")
        .await
        .unwrap()
        .unwrap();
    assert!(content.contains("# Design doc"));
}

#[tokio::test]
async fn scope_resources_global_visible_to_all() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path(), ws_dir.path()));

    use nenjo::memory::Memory;

    let scope_a = MemoryScope::new("agent-a", Some("project-x"));
    let scope_b = MemoryScope::new("agent-b", Some("project-y"));
    let scope_sys = MemoryScope::new("system-agent", None);

    // Agent A saves a global resource
    memory
        .save_resource(
            &scope_a.resources_global,
            "guide.md",
            "Onboarding guide",
            "agent-a",
            "# Guide",
        )
        .await
        .unwrap();

    // All agents see it regardless of project
    for scope in [&scope_a, &scope_b, &scope_sys] {
        let entries = memory
            .list_resources(&scope.resources_global)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1, "global resource should be visible");
        assert_eq!(entries[0].filename, "guide.md");
    }
}

// ---------------------------------------------------------------------------
// Ability & domain memory flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ability_inherits_memory_vars() {
    use nenjo::manifest::AbilityManifest;

    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path(), ws_dir.path());

    use nenjo::memory::Memory;

    // Pre-populate memory so it shows up in vars
    let scope = MemoryScope::new("ability-agent", Some("test-project"));
    memory
        .append(&scope.core, "expertise", "Knows Rust deeply")
        .await
        .unwrap();

    let model = ModelManifest {
        id: Uuid::new_v4(),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        tags: vec![],
    };

    let ability = AbilityManifest {
        id: Uuid::new_v4(),
        name: "code-review".into(),
        display_name: None,
        description: Some("Reviews code".into()),
        activation_condition: "when code review is needed".into(),
        prompt: "You review code.".into(),
        platform_scopes: vec![],
        skill_ids: vec![],
        mcp_server_ids: vec![],
        tool_filter: serde_json::json!({}),
    };

    let agent = nenjo::manifest::AgentManifest {
        id: Uuid::new_v4(),
        name: "ability-agent".into(),
        description: Some("Agent with abilities".into()),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": "You are helpful.\n{{ memories }}",
            "templates": {
                "chat_task": "{{ chat.message }}",
                "task_execution": "",
                "gate_eval": "",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model.id),
        model_name: Some("test-model".into()),
        skills: vec![],
        domains: vec![],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        abilities: vec![ability.id],
    };

    let project = nenjo::manifest::ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    };

    let manifest = nenjo::manifest::Manifest {
        agents: vec![agent],
        models: vec![model],
        abilities: vec![ability],
        projects: vec![project],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory {
            response_text: "ok".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("ability-agent")
        .await
        .unwrap()
        .build()
        .unwrap();

    // The agent should have use_ability tool
    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"use_ability"), "should have use_ability");
    assert!(names.contains(&"memory_store"), "should have memory_store");

    // Memory vars should be empty on the instance (loaded at execution time)
    // but the memory backend is configured on the runner
    assert!(
        runner.memory().is_some(),
        "runner should have memory backend"
    );
}

#[tokio::test]
async fn domain_expansion_preserves_memory() {
    use nenjo::manifest::DomainManifest;

    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path(), ws_dir.path());

    use nenjo::memory::Memory;

    let scope = MemoryScope::new("domain-agent", Some("test-project"));
    memory
        .append(&scope.project, "decisions", "Using axum for HTTP")
        .await
        .unwrap();

    let model = ModelManifest {
        id: Uuid::new_v4(),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        tags: vec![],
    };

    let domain = DomainManifest {
        id: Uuid::new_v4(),
        name: "prd".into(),
        display_name: "PRD Mode".into(),
        description: Some("Product requirements".into()),
        command: "/prd".into(),
        manifest: serde_json::json!({
            "tools": {
                "additional_scopes": [],
                "activate_abilities": [],
            },
            "prompt": {}
        }),
        category: None,
        tags: vec![],
        is_system: false,
        source_domain_id: None,
    };

    let agent = nenjo::manifest::AgentManifest {
        id: Uuid::new_v4(),
        name: "domain-agent".into(),
        description: Some("Agent with domains".into()),
        is_system: false,
        prompt_config: serde_json::json!({
            "system_prompt": "You are helpful.\n{{ memories }}",
            "templates": {
                "chat_task": "{{ chat.message }}",
                "task_execution": "",
                "gate_eval": "",
                "cron_task": ""
            }
        }),
        color: None,
        model_id: Some(model.id),
        model_name: Some("test-model".into()),
        skills: vec![],
        domains: vec![domain.id],
        platform_scopes: vec![],
        mcp_server_ids: vec![],
        abilities: vec![],
    };

    let project = nenjo::manifest::ProjectManifest {
        id: Uuid::new_v4(),
        name: "test-project".into(),
        slug: "test-project".into(),
        description: None,
        is_system: false,
        settings: serde_json::Value::Null,
    };

    let manifest = nenjo::manifest::Manifest {
        agents: vec![agent],
        models: vec![model],
        domains: vec![domain],
        projects: vec![project],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(MockModelProviderFactory {
            response_text: "ok".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("domain-agent")
        .await
        .unwrap()
        .build()
        .unwrap();

    // Expand into domain
    let domain_runner = runner.domain_expansion("prd").await.unwrap();

    // Domain runner should preserve memory backend
    assert!(
        domain_runner.memory().is_some(),
        "domain runner should have memory backend"
    );
    assert!(
        domain_runner.memory_scope().is_some(),
        "domain runner should have memory scope"
    );

    // Memory tools should still be present
    let specs = domain_runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"memory_store"),
        "domain runner should have memory_store"
    );
    assert!(
        names.contains(&"resource_save"),
        "domain runner should have resource_save"
    );
}

// ---------------------------------------------------------------------------
// Runner execution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runner_with_memory_executes() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path(), ws_dir.path());

    let provider = Provider::builder()
        .with_manifest(test_manifest())
        .with_model_factory(MockModelProviderFactory {
            response_text: "I see from memory this is a Rust project.".into(),
        })
        .with_tool_factory(NoopToolFactory)
        .with_memory(memory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent_by_name("memory-agent")
        .await
        .unwrap()
        .build()
        .unwrap();

    let output = runner
        .chat("What do you know about this project?")
        .await
        .unwrap();
    assert_eq!(output.text, "I see from memory this is a Rust project.");
}
