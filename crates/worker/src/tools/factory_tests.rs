use std::sync::Arc;

use super::*;
use crate::crypto::WorkerAuthProvider;
use crate::tools::NativeRuntime;
use crate::tools::platform_payload::PlatformPayloadEncoder;
use crate::tools::platform_services::PlatformToolServices;
use nenjo::agents::prompts::PromptConfig;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::manifest::{
    AbilityManifest, DomainManifest, Manifest, McpServerManifest, SkillManifest,
};
use nenjo::manifest::{AgentManifest, MediaRequirement};
use nenjo::{ManifestWriter, Slug, ToolFactory};
use nenjo_events::EncryptedPayload;
use nenjo_models::NativeOperation;
use nenjo_platform::{
    AbilitiesGetParams, AbilityManifestBackend, AgentManifestBackend, AgentsGetParams,
    DomainManifestBackend, DomainsGetParams, ManifestAccessPolicy, PlatformManifestBackend,
    PlatformManifestClient, tools::PlatformNotificationEmitter,
};
use tempfile::tempdir;

struct TestNotificationSink;

impl PlatformNotificationEmitter for TestNotificationSink {
    fn send_push_notification(
        &self,
        _agent: &str,
        _encrypted_payload: EncryptedPayload,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn test_platform_services(
    config: &crate::config::Config,
    auth_provider: Arc<WorkerAuthProvider>,
) -> PlatformToolServices {
    let manifest_store = Arc::new(LocalManifestStore::new(config.manifests_dir.clone()));
    let platform_client = PlatformManifestClient::new(config.backend_api_url(), &config.api_key)
        .map(Arc::new)
        .ok();
    PlatformToolServices::new(
        manifest_store,
        platform_client,
        PlatformPayloadEncoder::new(auth_provider),
        None,
        config.workspace_dir.clone(),
        config.config_dir.join("library"),
    )
}

#[tokio::test]
async fn worker_factory_always_exposes_use_skill_tool() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    let config = crate::config::Config {
        workspace_dir: root.join("workspace"),
        state_dir: root.join("state"),
        manifests_dir: root.join("manifests"),
        backend_api_url: Some("http://localhost:3001".into()),
        api_key: "test-api-key".into(),
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::new(security, NativeRuntime, config, platform, external_mcp);

    let agent = AgentManifest {
        name: "tester".into(),
        slug: Slug::derive("test-agent"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(
        names.iter().any(|name| name == "use_skill"),
        "worker tool belt should always include use_skill, got: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "list_installed_skills"),
        "worker tool belt should always include list_installed_skills, got: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "call_skill_mcp_tool"),
        "worker tool belt should always include call_skill_mcp_tool, got: {names:?}"
    );
}

#[tokio::test]
async fn worker_factory_exposes_agent_native_media_tools() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    let config = crate::config::Config {
        workspace_dir: root.join("workspace"),
        state_dir: root.join("state"),
        manifests_dir: root.join("manifests"),
        media_providers: vec![crate::config::MediaProviderConfig {
            slug: Slug::derive("openai_image"),
            provider: "openai".to_string(),
            model: "gpt-image-1".to_string(),
            capabilities: vec![NativeOperation::GenerateImage],
        }],
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::new(security, NativeRuntime, config, platform, external_mcp);

    let agent = AgentManifest {
        name: "image tester".into(),
        slug: Slug::derive("image-tester"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: vec![MediaRequirement::Capability(NativeOperation::GenerateImage)],
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let tools = factory.create_tools(&agent).await;

    assert!(
        tools.iter().any(|tool| tool.name() == "generate_image"),
        "agent media requirements should add native media tools"
    );
}

#[tokio::test]
async fn worker_factory_skill_mcp_proxy_requires_skill_activation() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let workspace_dir = root.join("workspace");
    let state_dir = root.join("state");
    let manifests_dir = root.join("manifests");
    let plugin_dir = workspace_dir
        .join(".nenjo")
        .join("plugins")
        .join("mcp-skill");
    let skill_dir = plugin_dir.join("skills").join("mcp-skill");
    tokio::fs::create_dir_all(&skill_dir).await.unwrap();
    tokio::fs::write(skill_dir.join("SKILL.md"), "# MCP Skill")
        .await
        .unwrap();
    tokio::fs::write(plugin_dir.join("server.sh"), skill_mcp_fixture_script())
        .await
        .unwrap();

    let config = crate::config::Config {
        workspace_dir: workspace_dir.clone(),
        state_dir,
        manifests_dir,
        backend_api_url: Some("http://localhost:3001".into()),
        api_key: "test-api-key".into(),
        ..Default::default()
    };
    let server = McpServerManifest {
        name: "mcp_skill__review_server".to_string(),
        display_name: "mcp-skill:review-server".to_string(),
        description: None,
        transport: "stdio".to_string(),
        command: Some("bash".to_string()),
        args: Some(vec!["server.sh".to_string()]),
        url: None,
        env_schema: serde_json::json!([]),
        source_type: "package".to_string(),
        read_only: true,
        metadata: serde_json::json!({
            "runtime": {
                "cwd": plugin_dir.to_string_lossy().to_string(),
                "env": {
                    "MODE": "skill"
                }
            }
        }),
    };
    let skill = SkillManifest {
        name: "mcp-skill".to_string(),
        display_name: None,
        aliases: Vec::new(),
        description: Some("Skill with MCP".to_string()),
        entry_path: "SKILL.md".to_string(),
        root_path: "skills/mcp-skill".to_string(),
        root_dir: skill_dir,
        plugin_root_path: Some(".".to_string()),
        plugin_root_dir: Some(plugin_dir),
        scripts: Vec::new(),
        references: Vec::new(),
        assets: Vec::new(),
        mcp_servers: vec![Slug::derive(&server.name)],
        hooks: Vec::new(),
        source_type: "package".to_string(),
        read_only: true,
        metadata: serde_json::Value::Null,
    };
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    external_mcp.reconcile(std::slice::from_ref(&server)).await;
    let registry = Arc::new(crate::skills::SkillRegistry::default());
    registry.reconcile(&[skill], &[]);

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::with_skill_registry(
        security,
        NativeRuntime,
        config,
        platform,
        external_mcp,
        registry,
    );
    let agent = AgentManifest {
        name: "tester".into(),
        slug: Slug::derive("test-agent"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let tools = factory.create_tools(&agent).await;
    let use_skill = tools
        .iter()
        .find(|tool| tool.name() == "use_skill")
        .expect("use_skill tool should exist");
    let skill_mcp = tools
        .iter()
        .find(|tool| tool.name() == "call_skill_mcp_tool")
        .expect("skill MCP proxy should exist");

    let before = skill_mcp
        .execute(serde_json::json!({ "tool": "review", "arguments": {} }))
        .await
        .unwrap();
    assert!(!before.success);
    assert!(
        before
            .error
            .as_deref()
            .is_some_and(|error| error.contains("No skill MCP servers are active"))
    );

    let activation = use_skill
        .execute(serde_json::json!({ "name": "mcp-skill" }))
        .await
        .unwrap();
    assert!(activation.success);

    let after = skill_mcp
        .execute(serde_json::json!({ "tool": "review", "arguments": {} }))
        .await
        .unwrap();
    assert!(after.success);
    assert_eq!(after.output, "review-ok:skill");
}

fn skill_mcp_fixture_script() -> String {
    r#"#!/usr/bin/env bash
set -euo pipefail
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"fixture","version":"0.1.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"review","description":"Review","inputSchema":{"type":"object","properties":{}}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"review-ok:%s"}]}}\n' "${MODE:-missing}"
      ;;
    *)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"unknown method"}}'
      ;;
  esac
done
"#
    .to_string()
}

async fn scoped_backend(
    caller_scopes: Vec<String>,
) -> (
    PlatformManifestBackend<LocalManifestStore, PlatformPayloadEncoder>,
    AgentManifest,
    AgentManifest,
    AbilityManifest,
    AbilityManifest,
    DomainManifest,
    DomainManifest,
) {
    let temp = tempdir().unwrap();
    let root = temp.keep();
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let store = Arc::new(LocalManifestStore::new(root.join("manifests")));

    let visible_agent = AgentManifest {
        name: "visible-agent".into(),
        slug: Slug::derive("test-agent"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec!["projects:read".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    let hidden_agent = AgentManifest {
        name: "hidden-agent".into(),
        slug: Slug::derive("test-agent"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec!["projects:write".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let visible_ability = AbilityManifest {
        name: "visible-ability".into(),
        path: None,
        description: None,
        activation_condition: "visible".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "visible prompt".into(),
        },
        platform_scopes: vec!["projects:read".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let hidden_ability = AbilityManifest {
        name: "hidden-ability".into(),
        path: None,
        description: None,
        activation_condition: "hidden".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "hidden prompt".into(),
        },
        platform_scopes: vec!["projects:write".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let visible_domain = DomainManifest {
        name: "visible-domain".into(),
        path: String::new(),
        description: None,
        command: "#visible".into(),
        platform_scopes: vec!["projects:read".into()],
        abilities: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        prompt_config: nenjo::types::DomainPromptConfig::default(),
    };
    let hidden_domain = DomainManifest {
        name: "hidden-domain".into(),
        path: String::new(),
        description: None,
        command: "#hidden".into(),
        platform_scopes: vec!["projects:write".into()],
        abilities: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        prompt_config: nenjo::types::DomainPromptConfig::default(),
    };

    store
        .replace_manifest(&Manifest {
            agents: vec![visible_agent.clone(), hidden_agent.clone()],
            abilities: vec![visible_ability.clone(), hidden_ability.clone()],
            domains: vec![visible_domain.clone(), hidden_domain.clone()],
            ..Default::default()
        })
        .await
        .unwrap();

    let client = PlatformManifestClient::new("http://localhost:3001", "test-api-key").unwrap();
    let inner = Arc::new(PlatformManifestBackend::new(
        store.clone(),
        client,
        PlatformPayloadEncoder::new(auth_provider),
    ));

    (
        inner
            .as_ref()
            .clone()
            .with_access_policy(ManifestAccessPolicy::new(caller_scopes)),
        visible_agent,
        hidden_agent,
        visible_ability,
        hidden_ability,
        visible_domain,
        hidden_domain,
    )
}

#[tokio::test]
async fn worker_factory_exposes_manifest_tools_without_duplicate_platform_tools() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    let config = crate::config::Config {
        workspace_dir: root.join("workspace"),
        state_dir: root.join("state"),
        manifests_dir: root.join("manifests"),
        backend_api_url: Some("http://localhost:3001".into()),
        api_key: "test-api-key".into(),
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::new(security, NativeRuntime, config, platform, external_mcp);

    let agent = AgentManifest {
        name: "tester".into(),
        slug: Slug::derive("test-agent"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec![
            "agents:read".into(),
            "agents:write".into(),
            "projects:read".into(),
        ],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(names.iter().any(|name| name == "list_agents"));
    assert!(names.iter().any(|name| name == "get_agent"));
    assert!(names.iter().any(|name| name == "configure_agent"));
    assert!(!names.iter().any(|name| name == "get_agent_prompt"));
    assert!(!names.iter().any(|name| name == "create_agent"));
    assert!(!names.iter().any(|name| name == "update_agent"));
    assert!(!names.iter().any(|name| name == "update_agent_prompt"));
    assert!(!names.iter().any(|name| name == "delete_agent"));
    assert!(names.iter().any(|name| name == "list_projects"));
    assert!(names.iter().any(|name| name == "get_project"));
    assert!(names.iter().any(|name| name == "list_project_tasks"));
    assert!(names.iter().any(|name| name == "get_project_task"));
    assert!(
        names
            .iter()
            .any(|name| name == "list_project_execution_runs")
    );
    assert!(names.iter().any(|name| name == "get_project_execution_run"));
    assert!(!names.iter().any(|name| name == "list_builtin_docs"));
    assert!(!names.iter().any(|name| name == "list_knowledge_packs"));
    assert!(!names.iter().any(|name| name == "read_knowledge_doc"));
    assert!(!names.iter().any(|name| name == "search_knowledge"));
    assert!(!names.iter().any(|name| name == "list_knowledge_neighbors"));
    assert!(!names.iter().any(|name| name == "read_builtin_doc"));
    assert!(!names.iter().any(|name| name == "search_builtin_docs"));
    assert!(!names.iter().any(|name| name == "search_builtin_doc_paths"));
    assert!(!names.iter().any(|name| name == "list_builtin_doc_tree"));
    assert!(!names.iter().any(|name| name == "read_builtin_doc_manifest"));
    assert!(
        !names
            .iter()
            .any(|name| name == "list_builtin_doc_neighbors")
    );
    assert!(!names.iter().any(|name| name == "create_project_task"));
    assert!(!names.iter().any(|name| name == "start_project_execution"));

    assert!(!names.iter().any(|name| name == "platform_read"));
    assert!(!names.iter().any(|name| name == "platform_write"));
    assert!(!names.iter().any(|name| name == "platform_graph"));

    let agent_without_project_scope = AgentManifest {
        platform_scopes: vec!["agents:read".into()],
        ..agent.clone()
    };
    let tools = factory.create_tools(&agent_without_project_scope).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(!names.iter().any(|name| name == "list_knowledge_packs"));
    assert!(!names.iter().any(|name| name == "read_knowledge_doc"));
    assert!(!names.iter().any(|name| name == "search_knowledge"));
    assert!(!names.iter().any(|name| name == "list_knowledge_neighbors"));
    assert!(!names.iter().any(|name| name == "list_projects"));

    let library_agent = AgentManifest {
        platform_scopes: vec!["library:write".into()],
        ..agent
    };
    let tools = factory.create_tools(&library_agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(!names.iter().any(|name| name == "list_knowledge_packs"));
    assert!(!names.iter().any(|name| name == "read_knowledge_doc"));
    assert!(!names.iter().any(|name| name == "search_knowledge"));
    assert!(!names.iter().any(|name| name == "list_knowledge_neighbors"));
    assert!(names.iter().any(|name| name == "create_knowledge_pack"));
    assert!(names.iter().any(|name| name == "update_knowledge_pack"));
    assert!(names.iter().any(|name| name == "create_knowledge_doc"));
    assert!(names.iter().any(|name| name == "update_knowledge_doc"));
    assert!(names.iter().any(|name| name == "delete_knowledge_doc"));
    assert!(!names.iter().any(|name| name == "list_projects"));
    assert!(!names.iter().any(|name| name == "delete_knowledge_pack"));
}

#[tokio::test]
async fn worker_factory_exposes_project_write_rest_tools_under_project_write_scope() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    let config = crate::config::Config {
        workspace_dir: root.join("workspace"),
        state_dir: root.join("state"),
        manifests_dir: root.join("manifests"),
        backend_api_url: Some("http://localhost:3001".into()),
        api_key: "test-api-key".into(),
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::new(security, NativeRuntime, config, platform, external_mcp);

    let agent = AgentManifest {
        name: "tester".into(),
        slug: Slug::derive("test-agent"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec!["projects:write".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(names.iter().any(|name| name == "create_project_tasks"));
    assert!(names.iter().any(|name| name == "update_project_task"));
    assert!(names.iter().any(|name| name == "delete_project_task"));
    assert!(names.iter().any(|name| name == "start_project_execution"));
    assert!(names.iter().any(|name| name == "pause_project_execution"));
    assert!(names.iter().any(|name| name == "resume_project_execution"));
}

#[tokio::test]
async fn worker_factory_exposes_notification_tools_under_notify_scopes() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    let config = crate::config::Config {
        workspace_dir: root.join("workspace"),
        state_dir: root.join("state"),
        manifests_dir: root.join("manifests"),
        backend_api_url: Some("http://localhost:3001".into()),
        api_key: "test-api-key".into(),
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::new(security, NativeRuntime, config, platform, external_mcp);

    let agent = AgentManifest {
        name: "tester".into(),
        slug: Slug::parse("tester").unwrap(),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec!["notify:read".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert!(
        names
            .iter()
            .any(|name| name == "list_notification_sessions")
    );
    assert!(names.iter().any(|name| name == "list_notifications"));
    assert!(!names.iter().any(|name| name == "send_notification"));

    let writer = AgentManifest {
        platform_scopes: vec!["notify:write".into()],
        ..agent
    };
    let tools = super::with_platform_notification_emitter(Arc::new(TestNotificationSink), async {
        factory.create_tools(&writer).await
    })
    .await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert!(
        names
            .iter()
            .any(|name| name == "list_notification_sessions")
    );
    assert!(names.iter().any(|name| name == "list_notifications"));
    assert!(names.iter().any(|name| name == "send_notification"));
}

#[tokio::test]
async fn platform_manifest_backend_filters_agents_abilities_and_domains_by_scopes() {
    let (
        backend,
        visible_agent,
        hidden_agent,
        visible_ability,
        hidden_ability,
        visible_domain,
        hidden_domain,
    ) = scoped_backend(vec!["projects:read".into()]).await;

    let agents = backend.list_agents().await.unwrap();
    assert_eq!(agents.agents.len(), 1);
    assert_eq!(agents.agents[0].name, visible_agent.name);
    assert!(
        backend
            .get_agent(AgentsGetParams {
                agent: Slug::derive(&hidden_agent.name)
            })
            .await
            .is_err()
    );

    let abilities = backend.list_abilities().await.unwrap();
    assert_eq!(abilities.abilities.len(), 1);
    assert_eq!(abilities.abilities[0].name, visible_ability.name);
    assert!(
        backend
            .get_ability(AbilitiesGetParams {
                ability: Slug::derive(&hidden_ability.name)
            })
            .await
            .is_err()
    );

    let domains = backend.list_domains().await.unwrap();
    assert_eq!(domains.domains.len(), 1);
    assert!(
        domains
            .domains
            .iter()
            .any(|domain| domain.slug == visible_domain.slug())
    );
    assert!(
        backend
            .get_domain(DomainsGetParams {
                domain: hidden_domain.slug()
            })
            .await
            .is_err()
    );
}
