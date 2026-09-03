//! Tests for memory integration with Provider and AgentRunner.

use std::sync::Arc;

use anyhow::Result;
use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, model_manifest_slug,
};
use nenjo::memory::{MarkdownMemory, MemoryScope};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo::types::{AbilityPromptConfig, DomainPromptConfig};
use nenjo::{Buffered, ChatInput, Slug};
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
            provider_tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            },
            finish_reason: nenjo_models::FinishReason::Stop,
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
        slug: model_manifest_slug("mock", "mock-llm-v1"),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        context_window: None,
        base_url: None,
        native_tools: vec![],
        capabilities: Vec::new(),
        input_modalities: Vec::new(),
        output_modalities: Vec::new(),
        execution_modes: Vec::new(),
    };

    let agent = AgentManifest {
        name: "memory-agent".into(),
        slug: Slug::derive("memory-agent"),
        description: Some("An agent with memory".into()),
        prompt_config: PromptConfig {
            system_prompt: "You are a helpful assistant.".into(),
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug(&model.model_provider, &model.model)),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        abilities: vec![],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let project = ProjectManifest {
        name: "test-project".into(),
        slug: Slug::derive("test-project"),
        description: None,
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
    let memory = MarkdownMemory::new(dir.path());

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
        .agent("memory-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"save_memory"), "should have save_memory");
    assert!(
        names.contains(&"recall_memory"),
        "should have recall_memory"
    );
    assert!(
        names.contains(&"forget_memory"),
        "should have forget_memory"
    );
    for legacy_name in ["save_artifact", "read_artifact", "delete_artifact"] {
        assert!(
            !names.contains(&legacy_name),
            "memory must not expose legacy artifact tool {legacy_name}"
        );
    }
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
        .agent("memory-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(!names.contains(&"save_memory"));
}

#[tokio::test]
async fn memory_store_and_recall() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
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
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
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
async fn memory_context_contains_all_tiers() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
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

    let context = nenjo::memory::build_memory_context(memory.as_ref(), &scope)
        .await
        .unwrap();

    assert!(
        context.contains("<memories>"),
        "should have memories root tag"
    );
    assert!(context.contains("<memories-core>"), "should have core tier");
    assert!(
        context.contains("<memories-project>"),
        "should have project tier"
    );
    assert!(
        context.contains("<memories-shared>"),
        "should have shared tier"
    );
    assert!(
        context.contains("User prefers Rust"),
        "should contain project fact"
    );
    assert!(
        context.contains("Distributed systems"),
        "should contain core fact"
    );
    assert!(context.contains("PostgreSQL"), "should contain shared fact");
}

#[tokio::test]
async fn memory_context_empty_when_no_facts() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));
    let scope = MemoryScope::new("empty-agent", Some("empty-project"));

    let context = nenjo::memory::build_memory_context(memory.as_ref(), &scope)
        .await
        .unwrap();

    assert!(context.is_empty(), "should be empty when no facts exist");
}

// ---------------------------------------------------------------------------
// Scope isolation tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scope_project_agent_three_tiers_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));

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

    let context = nenjo::memory::build_memory_context(memory.as_ref(), &scope)
        .await
        .unwrap();

    assert!(context.contains("project-only"));
    assert!(context.contains("core-only"));
    assert!(context.contains("shared-only"));

    let project = nenjo_xml::xml::parse::extract_tag_content(&context, "memories-project")
        .expect("project memory tier");
    assert!(project.contains("project-only"));
    assert!(!project.contains("core-only"));

    let core = nenjo_xml::xml::parse::extract_tag_content(&context, "memories-core")
        .expect("core memory tier");
    assert!(core.contains("core-only"));
    assert!(!core.contains("project-only"));

    let shared = nenjo_xml::xml::parse::extract_tag_content(&context, "memories-shared")
        .expect("shared memory tier");
    assert!(shared.contains("shared-only"));
    assert!(!shared.contains("project-only"));
}

#[tokio::test]
async fn scope_system_agent_collapses_to_core() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));

    use nenjo::memory::Memory;

    let scope = MemoryScope::new("nenji", None);

    // Project and core both write to agent_nenji_core
    memory
        .append(&scope.project, "prefs", "fact-a")
        .await
        .unwrap();
    memory.append(&scope.core, "prefs", "fact-b").await.unwrap();

    let context = nenjo::memory::build_memory_context(memory.as_ref(), &scope)
        .await
        .unwrap();

    // Project tier is skipped when it resolves to the same namespace as core
    // (system agents with no project), so only core should appear.
    assert!(!context.contains("<memories-project>"));

    let core_xml = nenjo_xml::xml::parse::extract_tag_content(&context, "memories-core")
        .expect("core memory tier");
    assert!(core_xml.contains("fact-a"));
    assert!(core_xml.contains("fact-b"));
}

#[tokio::test]
async fn scope_shared_visible_across_agents() {
    let dir = tempfile::tempdir().unwrap();
    let memory = Arc::new(MarkdownMemory::new(dir.path()));

    use nenjo::memory::Memory;

    let scope_coder = MemoryScope::new("coder", Some("webapp"));
    let scope_reviewer = MemoryScope::new("reviewer", Some("webapp"));

    // Coder stores a shared fact
    memory
        .append(&scope_coder.shared, "conventions", "Always write tests")
        .await
        .unwrap();

    // Reviewer can see it via their shared scope (same project)
    let context = nenjo::memory::build_memory_context(memory.as_ref(), &scope_reviewer)
        .await
        .unwrap();
    let shared = nenjo_xml::xml::parse::extract_tag_content(&context, "memories-shared")
        .expect("shared memory tier");
    assert!(shared.contains("Always write tests"));

    // But reviewer can't see coder's project-scoped memories
    let reviewer_project = memory
        .list_categories(&scope_reviewer.project)
        .await
        .unwrap();
    assert!(reviewer_project.is_empty());
}

// ---------------------------------------------------------------------------
// Ability & domain memory flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ability_inherits_memory_context() {
    use nenjo::manifest::AbilityManifest;

    let dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path());

    use nenjo::memory::Memory;

    // Pre-populate memory so it shows up in session context.
    let scope = MemoryScope::new("ability-agent", Some("test-project"));
    memory
        .append(&scope.core, "expertise", "Knows Rust deeply")
        .await
        .unwrap();

    let model = ModelManifest {
        slug: model_manifest_slug("mock", "mock-llm-v1"),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        context_window: None,
        base_url: None,
        native_tools: vec![],
        capabilities: Vec::new(),
        input_modalities: Vec::new(),
        output_modalities: Vec::new(),
        execution_modes: Vec::new(),
    };

    let ability = AbilityManifest {
        slug: Slug::derive("code-review"),
        name: "code-review".into(),
        path: None,
        description: Some("Reviews code".into()),
        activation_condition: "when code review is needed".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "You review code.".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let agent = nenjo::manifest::AgentManifest {
        name: "ability-agent".into(),
        slug: Slug::derive("ability-agent"),
        description: Some("Agent with abilities".into()),
        prompt_config: PromptConfig {
            system_prompt: "You are helpful.".into(),
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug(&model.model_provider, &model.model)),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        abilities: vec![ability.slug.clone()],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let project = nenjo::manifest::ProjectManifest {
        name: "test-project".into(),
        slug: Slug::derive("test-project"),
        description: None,
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
        .agent("ability-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    // The agent should have the ability broker tools, not a per-ability tool.
    let specs = runner.instance().tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"list_assigned_abilities"),
        "should have list_assigned_abilities"
    );
    assert!(names.contains(&"use_ability"), "should have use_ability");
    assert!(
        !names.contains(&"code_review"),
        "should not expose per-ability tool"
    );
    assert!(names.contains(&"save_memory"), "should have save_memory");

    // Memory is loaded into session context at execution time while the backend
    // remains configured on the runner.
    assert!(
        runner.memory().is_some(),
        "runner should have memory backend"
    );
}

#[tokio::test]
async fn domain_expansion_preserves_memory() {
    use nenjo::manifest::DomainManifest;

    let dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path());

    use nenjo::memory::Memory;

    let scope = MemoryScope::new("domain-agent", Some("test-project"));
    memory
        .append(&scope.project, "decisions", "Using axum for HTTP")
        .await
        .unwrap();

    let model = ModelManifest {
        slug: model_manifest_slug("mock", "mock-llm-v1"),
        name: "test-model".into(),
        description: None,
        model: "mock-llm-v1".into(),
        model_provider: "mock".into(),
        temperature: Some(0.5),
        context_window: None,
        base_url: None,
        native_tools: vec![],
        capabilities: Vec::new(),
        input_modalities: Vec::new(),
        output_modalities: Vec::new(),
        execution_modes: Vec::new(),
    };

    let domain = DomainManifest {
        slug: Slug::derive("prd"),
        name: "prd".into(),
        path: String::new(),
        description: Some("Product requirements".into()),
        command: "/prd".into(),
        platform_scopes: vec![],
        abilities: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        prompt_config: DomainPromptConfig {
            developer_prompt_addon: Some("PRD mode".into()),
        },
    };

    let agent = nenjo::manifest::AgentManifest {
        name: "domain-agent".into(),
        slug: Slug::derive("domain-agent"),
        description: Some("Agent with domains".into()),
        prompt_config: PromptConfig {
            system_prompt: "You are helpful.".into(),
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug(&model.model_provider, &model.model)),
        domains: vec![domain.slug.clone()],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        abilities: vec![],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let project = nenjo::manifest::ProjectManifest {
        name: "test-project".into(),
        slug: Slug::derive("test-project"),
        description: None,
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
        .agent("domain-agent")
        .await
        .unwrap()
        .build()
        .await
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
        names.contains(&"save_memory"),
        "domain runner should have save_memory"
    );
}

// ---------------------------------------------------------------------------
// Runner execution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runner_with_memory_executes() {
    let dir = tempfile::tempdir().unwrap();
    let memory = MarkdownMemory::new(dir.path());

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
        .agent("memory-agent")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let output = runner
        .chat(
            ChatInput::new("What do you know about this project?"),
            Buffered,
        )
        .await
        .unwrap()
        .output()
        .await
        .unwrap();
    assert_eq!(output.text, "I see from memory this is a Rust project.");
}
