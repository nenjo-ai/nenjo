use std::sync::Arc;

use super::*;
use crate::bootstrap::{ManifestRefreshHandle, WorkerManifestCache, WorkerManifestStore};
use crate::crypto::WorkerAuthProvider;
use crate::tools::NativeRuntime;
use crate::tools::platform_payload::PlatformPayloadEncoder;
use crate::tools::platform_services::PlatformToolServices;
use nenjo::agents::prompts::PromptConfig;
use nenjo::manifest::AgentManifest;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::manifest::{
    AbilityManifest, CommandManifest, ContextBlockManifest, CouncilDelegationStrategy,
    CouncilManifest, DomainManifest, Manifest, McpServerManifest, ModelManifest, ProjectManifest,
    RoutineManifest, RoutineMetadata, SkillManifest,
};
use nenjo::{ManifestWriter, Slug, ToolFactory};
use nenjo_events::{EncryptedPayload, ModelAssignmentBinding};
use nenjo_models::MediaOperation;
use nenjo_platform::manifest_mcp::CommandsGetParams;
use nenjo_platform::{
    AbilitiesGetParams, AbilityManifestBackend, AgentManifestBackend, AgentsGetParams,
    CommandManifestBackend, ContextBlockManifestBackend, ContextBlocksGetParams,
    CouncilManifestBackend, CouncilsGetParams, DomainManifestBackend, DomainsGetParams,
    ModelManifestBackend, ModelsGetParams, PlatformManifestBackend, PlatformManifestClient,
    ProjectManifestBackend, ProjectsGetParams, RoutineManifestBackend, RoutinesGetParams,
    tools::PlatformNotificationEmitter,
};
use tempfile::tempdir;

fn cached_model(
    id: uuid::Uuid,
    slug: &str,
    model: &str,
    provider: &str,
    base_url: Option<&str>,
    capabilities: Vec<String>,
) -> crate::bootstrap::CachedModelManifest {
    crate::bootstrap::CachedModelManifest {
        id,
        manifest: ModelManifest {
            name: slug.into(),
            slug: Slug::derive(slug),
            description: None,
            model: model.into(),
            model_provider: provider.into(),
            temperature: None,
            context_window: None,
            base_url: base_url.map(str::to_owned),
            native_tools: Vec::new(),
        },
        capabilities,
    }
}

fn cached_agent(
    id: uuid::Uuid,
    name: &str,
    slug: Slug,
    model_assignments: Vec<ModelAssignmentBinding>,
) -> crate::bootstrap::CachedAgentManifest {
    crate::bootstrap::CachedAgentManifest {
        id,
        manifest: AgentManifest {
            name: name.into(),
            slug,
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model: None,
            domains: Vec::new(),
            platform_scopes: Vec::new(),
            mcp_servers: Vec::new(),
            script_tools: Vec::new(),
            media: Vec::new(),
            abilities: Vec::new(),
            prompt_locked: false,
            source_type: None,
            metadata: serde_json::json!({}),
        },
        model_assignments,
    }
}

struct TestNotificationSink;

impl PlatformNotificationEmitter for TestNotificationSink {
    fn send_push_notification(
        &self,
        _agent: &str,
        _current_session_id: Option<uuid::Uuid>,
        _encrypted_payload: EncryptedPayload,
        _recipient: Option<nenjo_platform::tools::PlatformNotificationRecipient>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn test_platform_services(
    config: &crate::config::Config,
    auth_provider: Arc<WorkerAuthProvider>,
) -> PlatformToolServices {
    let manifest_cache = Arc::new(WorkerManifestCache {
        manifests_dir: config.manifests_dir.clone(),
        workspace_dir: config.workspace_dir.clone(),
        state_dir: config.state_dir.clone(),
        config_dir: config.config_dir.clone(),
    });
    let manifest_store = Arc::new(WorkerManifestStore::new(
        manifest_cache,
        ManifestRefreshHandle::default(),
    ));
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
        None,
    )
}

#[tokio::test]
async fn worker_factory_hides_skill_tools_when_registry_is_empty() {
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
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    for skill_tool in ["use_skill", "list_installed_skills", "call_skill_mcp_tool"] {
        assert!(
            !names.iter().any(|name| name == skill_tool),
            "empty skill registry should hide {skill_tool}, got: {names:?}"
        );
    }
    for expected in [
        "shell",
        "read",
        "write",
        "edit",
        "remove",
        "search",
        "repo_status",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "worker tool belt should include {expected}, got: {names:?}"
        );
    }
    for removed in [
        "file_read",
        "file_write",
        "file_edit",
        "file_delete",
        "content_search",
        "glob_search",
        "git_operations",
    ] {
        assert!(
            !names.iter().any(|name| name == removed),
            "worker tool belt should not include legacy tool {removed}, got: {names:?}"
        );
    }
}

#[tokio::test]
async fn worker_factory_exposes_activation_tools_without_mcp_proxy_for_plain_skills() {
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
    let registry = Arc::new(crate::skills::SkillRegistry::default());
    let skill: SkillManifest = serde_json::from_value(serde_json::json!({
        "name": "review",
        "root_dir": root.join("skills/review")
    }))
    .unwrap();
    registry.reconcile(&[skill], &[]);

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
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
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory.create_tools(&agent).await;
    let names = tools.iter().map(|tool| tool.name()).collect::<Vec<_>>();
    assert!(names.contains(&"use_skill"), "got: {names:?}");
    assert!(names.contains(&"list_installed_skills"), "got: {names:?}");
    assert!(!names.contains(&"call_skill_mcp_tool"), "got: {names:?}");
}

#[tokio::test]
async fn worker_factory_scopes_shell_tool_to_requested_workspace() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let workspace = root.join("workspace");
    let worktree = root.join("worktrees").join("task-worktree");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    tokio::fs::create_dir_all(&worktree).await.unwrap();
    tokio::fs::write(worktree.join("marker.txt"), "factory scoped")
        .await
        .unwrap();

    let config = crate::config::Config {
        workspace_dir: workspace.clone(),
        state_dir: root.join("state"),
        manifests_dir: root.join("manifests"),
        backend_api_url: Some("http://localhost:3001".into()),
        api_key: "test-api-key".into(),
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(workspace);
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
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory
        .create_tools_with_security(
            &agent,
            Arc::new(nenjo::ToolSecurity::with_workspace_dir(worktree.clone())),
        )
        .await;
    let shell = tools
        .iter()
        .find(|tool| tool.name() == "shell")
        .expect("shell tool should be exposed");

    let pwd = shell
        .execute(serde_json::json!({"command": "pwd"}))
        .await
        .unwrap();
    assert!(pwd.success);
    assert_eq!(
        std::fs::canonicalize(pwd.output.trim()).unwrap(),
        std::fs::canonicalize(&worktree).unwrap()
    );

    let relative_read = shell
        .execute(serde_json::json!({"command": "cat marker.txt"}))
        .await
        .unwrap();
    assert!(relative_read.success);
    assert_eq!(relative_read.output.trim(), "factory scoped");
}

#[tokio::test]
async fn worker_factory_exposes_agent_native_media_tools() {
    use nenjo_platform::{
        PlatformResourceIdSnapshot, PlatformResourceIdStore, PlatformResourceKind,
    };
    use uuid::Uuid;

    let temp = tempdir().unwrap();
    let root = temp.path();
    let manifests_dir = root.join("manifests");
    std::fs::create_dir_all(&manifests_dir).unwrap();

    let agent_id = Uuid::new_v4();
    let model_id = Uuid::new_v4();
    let agent_slug = Slug::derive("image-tester");

    let mut resource_ids = PlatformResourceIdSnapshot::default();
    resource_ids.insert(PlatformResourceKind::Agent, &agent_slug, agent_id);
    PlatformResourceIdStore::new(&manifests_dir)
        .replace(&resource_ids)
        .unwrap();

    std::fs::write(
        manifests_dir.join("models.json"),
        serde_json::to_string(&[cached_model(
            model_id,
            "openai-image",
            "gpt-image-1",
            "openai",
            None,
            vec![MediaOperation::GenerateImage.as_str().to_string()],
        )])
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        manifests_dir.join("agents.json"),
        serde_json::to_string(&[cached_agent(
            agent_id,
            "image tester",
            agent_slug.clone(),
            vec![ModelAssignmentBinding {
                capability: MediaOperation::GenerateImage.as_str().to_string(),
                model_id,
                assignment_source: "local".to_string(),
            }],
        )])
        .unwrap(),
    )
    .unwrap();

    let config = crate::config::Config {
        workspace_dir: root.join("workspace"),
        state_dir: root.join("state"),
        manifests_dir,
        media_providers: Vec::new(),
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::new(security, NativeRuntime, config, platform, external_mcp);

    let agent = AgentManifest {
        name: "image tester".into(),
        slug: agent_slug,
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
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory.create_tools(&agent).await;

    assert!(
        tools.iter().any(|tool| tool.name() == "generate_image"),
        "agent model_assignments should add native media tools"
    );
}

#[tokio::test]
async fn worker_factory_exposes_assignment_only_transcribe_tool_with_custom_base_url() {
    use nenjo_platform::{
        PlatformResourceIdSnapshot, PlatformResourceIdStore, PlatformResourceKind,
    };
    use uuid::Uuid;

    let temp = tempdir().unwrap();
    let root = temp.path();
    let manifests_dir = root.join("manifests");
    std::fs::create_dir_all(&manifests_dir).unwrap();

    let agent_id = Uuid::new_v4();
    let model_id = Uuid::new_v4();
    let agent_slug = Slug::derive("voice-agent");

    let mut resource_ids = PlatformResourceIdSnapshot::default();
    resource_ids.insert(PlatformResourceKind::Agent, &agent_slug, agent_id);
    PlatformResourceIdStore::new(&manifests_dir)
        .replace(&resource_ids)
        .unwrap();

    std::fs::write(
        manifests_dir.join("models.json"),
        serde_json::to_string(&[cached_model(
            model_id,
            "custom-stt",
            "whisper-1",
            "openai",
            Some("https://stt.example.internal/v1"),
            vec![MediaOperation::TranscribeAudio.as_str().to_string()],
        )])
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        manifests_dir.join("agents.json"),
        serde_json::to_string(&[cached_agent(
            agent_id,
            "voice agent",
            agent_slug.clone(),
            vec![ModelAssignmentBinding {
                capability: MediaOperation::TranscribeAudio.as_str().to_string(),
                model_id,
                assignment_source: "local".to_string(),
            }],
        )])
        .unwrap(),
    )
    .unwrap();
    // No media_providers.json — assignment-only path.

    let config = crate::config::Config {
        workspace_dir: root.join("workspace"),
        state_dir: root.join("state"),
        manifests_dir,
        media_providers: Vec::new(),
        ..Default::default()
    };

    let security = SecurityPolicy::with_workspace_dir(config.workspace_dir.clone());
    let external_mcp = Arc::new(crate::external_mcp::ExternalMcpPool::new());
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let platform = test_platform_services(&config, auth_provider);
    let factory = WorkerToolFactory::new(security, NativeRuntime, config, platform, external_mcp);

    let agent = AgentManifest {
        name: "voice agent".into(),
        slug: agent_slug,
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(), // no legacy media row
        abilities: vec![],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory.create_tools(&agent).await;

    assert!(
        tools.iter().any(|tool| tool.name() == "transcribe_audio"),
        "assignment-only STT path should expose transcribe_audio without media_providers"
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
        source_type: None,
        metadata: serde_json::json!({}),
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
    *'"method":"notifications/initialized"'*)
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

async fn scoped_backend() -> (
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
        slug: Slug::derive("visible-agent"),
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
        source_type: None,
        metadata: serde_json::json!({}),
    };
    let hidden_agent = AgentManifest {
        name: "hidden-agent".into(),
        slug: Slug::derive("hidden-agent"),
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
        source_type: None,
        metadata: serde_json::json!({}),
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
        inner.as_ref().clone(),
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
            "tasks:read".into(),
        ],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
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
    assert!(names.iter().any(|name| name == "list_tasks"));
    assert!(names.iter().any(|name| name == "get_task"));
    assert!(names.iter().any(|name| name == "list_task_execution_runs"));
    assert!(
        !names
            .iter()
            .any(|name| name == "list_active_execution_runs")
    );
    assert!(names.iter().any(|name| name == "watch_execution_run"));
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
    assert!(!names.iter().any(|name| name == "configure_task"));
    assert!(!names.iter().any(|name| name == "dispatch_task"));

    assert!(!names.iter().any(|name| name == "platform_read"));
    assert!(!names.iter().any(|name| name == "platform_write"));
    assert!(!names.iter().any(|name| name == "platform_graph"));

    let task_writer = AgentManifest {
        platform_scopes: vec!["tasks:write".into()],
        ..agent.clone()
    };
    let tools = factory.create_tools(&task_writer).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    for read_tool in [
        "list_agents",
        "get_agent",
        "list_projects",
        "get_project",
        "list_routines",
        "get_routine",
    ] {
        assert!(names.iter().any(|name| name == read_tool));
    }
    for write_tool in [
        "configure_agent",
        "create_project",
        "update_project",
        "delete_project",
        "configure_routine",
    ] {
        assert!(!names.iter().any(|name| name == write_tool));
    }
    assert!(names.iter().any(|name| name == "configure_task"));
    assert!(names.iter().any(|name| name == "dispatch_task"));

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
async fn worker_factory_exposes_task_tools_under_task_write_scope() {
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
        platform_scopes: vec!["tasks:write".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(names.iter().any(|name| name == "configure_task"));
    assert!(!names.iter().any(|name| name == "create_task"));
    assert!(names.iter().any(|name| name == "delete_task"));
    assert!(names.iter().any(|name| name == "dispatch_task"));
    assert!(names.iter().any(|name| name == "cancel_execution_run"));
    assert!(names.iter().any(|name| name == "retry_execution_run"));
    assert!(names.iter().any(|name| name == "watch_execution_run"));
    assert!(!names.iter().any(|name| name == "start_project_execution"));
}

#[tokio::test]
async fn worker_factory_exposes_local_execution_watch_without_platform_backend() {
    let temp = tempdir().unwrap();
    let config = crate::config::Config {
        workspace_dir: temp.path().join("workspace"),
        state_dir: temp.path().join("state"),
        manifests_dir: temp.path().join("manifests"),
        ..Default::default()
    };
    let factory = WorkerToolFactory::new(
        SecurityPolicy::with_workspace_dir(config.workspace_dir.clone()),
        NativeRuntime,
        config,
        PlatformToolServices::default(),
        Arc::new(crate::external_mcp::ExternalMcpPool::new()),
    );
    let agent = AgentManifest {
        name: "tester".into(),
        slug: Slug::derive("test-agent"),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model: None,
        domains: Vec::new(),
        platform_scopes: vec!["tasks:read".into()],
        mcp_servers: Vec::new(),
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: Vec::new(),
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory.create_tools(&agent).await;
    let names = tools.iter().map(|tool| tool.name()).collect::<Vec<_>>();

    assert!(names.contains(&"watch_execution_run"));
    assert!(!names.contains(&"list_tasks"));
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
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert!(
        names
            .iter()
            .any(|name| name == "search_notification_recipients")
    );
    assert!(names.iter().any(|name| name == "list_notifications"));
    assert!(!names.iter().any(|name| name == "search_notifications"));
    assert!(
        !names
            .iter()
            .any(|name| name == "list_notification_sessions")
    );
    assert!(!names.iter().any(|name| name == "send_notification"));

    let writer = AgentManifest {
        platform_scopes: vec!["notify:write".into()],
        ..agent
    };
    let tools = factory.create_tools(&writer).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert!(
        names
            .iter()
            .any(|name| name == "search_notification_recipients")
    );
    assert!(names.iter().any(|name| name == "list_notifications"));
    assert!(!names.iter().any(|name| name == "search_notifications"));
    assert!(
        !names
            .iter()
            .any(|name| name == "list_notification_sessions")
    );
    assert!(names.iter().any(|name| name == "send_notification"));

    let tools = super::with_platform_notification_emitter(Arc::new(TestNotificationSink), async {
        factory.create_tools(&writer).await
    })
    .await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert!(names.iter().any(|name| name == "send_notification"));
}

#[tokio::test]
async fn worker_factory_resolves_registered_notification_emitter_from_context() {
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
        platform_scopes: vec!["notify:write".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        abilities: vec![],
        prompt_locked: false,
        source_type: None,
        metadata: serde_json::json!({}),
    };

    let step_session_id = uuid::Uuid::new_v4();
    let _registration =
        super::register_platform_notification_emitter(Arc::new(TestNotificationSink));
    let tools = factory
        .create_tools_with_context(
            &agent,
            Arc::new(nenjo::ToolSecurity::default()),
            nenjo::ToolContext {
                project_slug: None,
                current_session_id: Some(step_session_id),
            },
        )
        .await;

    let tool = tools
        .iter()
        .find(|tool| tool.name() == "send_notification")
        .expect("send_notification tool should be exposed");
    assert!(tool.description().contains("notification"));
}

#[tokio::test]
async fn platform_manifest_backend_does_not_filter_read_results_by_resource_scopes() {
    let (
        backend,
        visible_agent,
        hidden_agent,
        visible_ability,
        hidden_ability,
        visible_domain,
        hidden_domain,
    ) = scoped_backend().await;

    let agents = backend.list_agents().await.unwrap();
    assert_eq!(agents.agents.len(), 2);
    assert!(
        agents
            .agents
            .iter()
            .any(|agent| agent.name == visible_agent.name)
    );
    assert!(
        agents
            .agents
            .iter()
            .any(|agent| agent.name == hidden_agent.name)
    );
    let agent = backend
        .get_agent(AgentsGetParams {
            agent: Slug::derive(&hidden_agent.name),
        })
        .await
        .unwrap()
        .agent;
    assert_eq!(agent.summary.name, hidden_agent.name);

    let abilities = backend.list_abilities().await.unwrap();
    assert_eq!(abilities.abilities.len(), 2);
    assert!(
        abilities
            .abilities
            .iter()
            .any(|ability| ability.name == visible_ability.name)
    );
    assert!(
        abilities
            .abilities
            .iter()
            .any(|ability| ability.name == hidden_ability.name)
    );
    let ability = backend
        .get_ability(AbilitiesGetParams {
            ability: Slug::derive(&hidden_ability.name),
        })
        .await
        .unwrap()
        .ability;
    assert_eq!(ability.summary.name, hidden_ability.name);

    let domains = backend.list_domains().await.unwrap();
    assert_eq!(domains.domains.len(), 2);
    assert!(
        domains
            .domains
            .iter()
            .any(|domain| domain.slug == visible_domain.slug())
    );
    assert!(
        domains
            .domains
            .iter()
            .any(|domain| domain.slug == hidden_domain.slug())
    );
    let domain = backend
        .get_domain(DomainsGetParams {
            domain: hidden_domain.slug(),
        })
        .await
        .unwrap()
        .domain;
    assert_eq!(domain.summary.slug, hidden_domain.slug());
}

#[tokio::test]
async fn platform_manifest_backend_reads_package_overlay_abilities() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let store = Arc::new(LocalManifestStore::new(root.join("manifests")));
    let package_ability = AbilityManifest {
        name: "build_routine".into(),
        path: Some("build".into()),
        description: Some("Build routines".into()),
        activation_condition: "Use for routine writes.".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "Build a routine.".into(),
        },
        platform_scopes: vec!["routines:read".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "package".into(),
        read_only: true,
        metadata: serde_json::Value::Null,
    };
    let client = PlatformManifestClient::new("http://localhost:3001", "test-api-key").unwrap();
    let backend =
        PlatformManifestBackend::new(store, client, PlatformPayloadEncoder::new(auth_provider))
            .with_read_only_manifest(Arc::new(Manifest {
                abilities: vec![package_ability.clone()],
                ..Default::default()
            }));

    let abilities = backend.list_abilities().await.unwrap();
    assert_eq!(abilities.abilities.len(), 1);
    assert_eq!(abilities.abilities[0].name, "build_routine");

    let ability = backend
        .get_ability(AbilitiesGetParams {
            ability: Slug::derive("build_routine"),
        })
        .await
        .unwrap()
        .ability;
    assert_eq!(ability.summary.name, package_ability.name);
    assert_eq!(ability.prompt_config.developer_prompt, "Build a routine.");
}

#[tokio::test]
async fn platform_manifest_backend_reads_package_overlay_for_manifest_resources() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let store = Arc::new(LocalManifestStore::new(root.join("manifests")));
    let package_agent = AgentManifest {
        name: "package agent".into(),
        slug: Slug::derive("package-agent"),
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
        source_type: None,
        metadata: serde_json::json!({}),
    };
    let package_ability = AbilityManifest {
        name: "package_ability".into(),
        path: None,
        description: None,
        activation_condition: "package".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "package ability prompt".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "package".into(),
        read_only: true,
        metadata: serde_json::Value::Null,
    };
    let package_command = CommandManifest {
        name: "package_command".into(),
        path: String::new(),
        command: "/package-command".into(),
        display_name: None,
        description: Some("Package command".into()),
        entry_path: "command.md".into(),
        content: "package command content".into(),
        root_path: String::new(),
        root_dir: std::path::PathBuf::new(),
        plugin_root_path: None,
        plugin_root_dir: None,
        hooks: vec![],
        source_type: "package".into(),
        read_only: true,
        metadata: serde_json::Value::Null,
    };
    let package_domain = DomainManifest {
        name: "package domain".into(),
        path: String::new(),
        description: None,
        command: "#package".into(),
        platform_scopes: vec![],
        abilities: vec![],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        prompt_config: nenjo::types::DomainPromptConfig::default(),
    };
    let package_project = ProjectManifest {
        name: "Package Project".into(),
        slug: Slug::derive("package-project"),
        description: None,
        settings: serde_json::json!({}),
    };
    let package_routine = RoutineManifest {
        name: "Package Routine".into(),
        slug: Slug::derive("package-routine"),
        description: None,
        metadata: RoutineMetadata::default(),
        steps: vec![],
        edges: vec![],
    };
    let package_model = ModelManifest {
        name: "Package Model".into(),
        slug: Slug::derive("package-model"),
        description: None,
        model: "gpt-4.1".into(),
        model_provider: "openai".into(),
        temperature: None,
        context_window: None,
        base_url: None,
        native_tools: vec![],
    };
    let package_council = CouncilManifest {
        name: "Package Council".into(),
        delegation_strategy: CouncilDelegationStrategy::Decompose,
        leader_agent: package_agent.slug.clone(),
        members: vec![],
    };
    let package_context_block = ContextBlockManifest {
        name: "package block".into(),
        path: String::new(),
        description: None,
        template: "package context".into(),
    };

    let client = PlatformManifestClient::new("http://localhost:3001", "test-api-key").unwrap();
    let backend =
        PlatformManifestBackend::new(store, client, PlatformPayloadEncoder::new(auth_provider))
            .with_read_only_manifest(Arc::new(Manifest {
                agents: vec![package_agent.clone()],
                abilities: vec![package_ability.clone()],
                commands: vec![package_command.clone()],
                domains: vec![package_domain.clone()],
                projects: vec![package_project.clone()],
                routines: vec![package_routine.clone()],
                models: vec![package_model.clone()],
                councils: vec![package_council.clone()],
                context_blocks: vec![package_context_block.clone()],
                ..Default::default()
            }));

    assert_eq!(backend.list_agents().await.unwrap().agents.len(), 1);
    assert_eq!(
        backend
            .get_agent(AgentsGetParams {
                agent: package_agent.slug.clone(),
            })
            .await
            .unwrap()
            .agent
            .summary
            .name,
        package_agent.name
    );
    assert_eq!(backend.list_abilities().await.unwrap().abilities.len(), 1);
    assert_eq!(
        backend
            .get_ability(AbilitiesGetParams {
                ability: Slug::derive(&package_ability.name),
            })
            .await
            .unwrap()
            .ability
            .summary
            .name,
        package_ability.name
    );
    assert_eq!(backend.list_commands().await.unwrap().commands.len(), 1);
    assert_eq!(
        backend
            .get_command(CommandsGetParams {
                command: package_command.command.clone(),
            })
            .await
            .unwrap()
            .command
            .name,
        package_command.name
    );
    assert_eq!(backend.list_domains().await.unwrap().domains.len(), 1);
    assert_eq!(
        backend
            .get_domain(DomainsGetParams {
                domain: package_domain.slug(),
            })
            .await
            .unwrap()
            .domain
            .summary
            .name,
        package_domain.name
    );
    assert_eq!(backend.list_projects().await.unwrap().projects.len(), 1);
    assert_eq!(
        backend
            .get_project(ProjectsGetParams {
                project: package_project.slug.clone(),
            })
            .await
            .unwrap()
            .project
            .summary
            .name,
        package_project.name
    );
    assert_eq!(backend.list_routines().await.unwrap().routines.len(), 1);
    assert_eq!(
        backend
            .get_routine(RoutinesGetParams {
                slug: package_routine.slug.clone(),
            })
            .await
            .unwrap()
            .routine
            .summary
            .name,
        package_routine.name
    );
    assert_eq!(backend.list_models().await.unwrap().models.len(), 1);
    assert_eq!(
        backend
            .get_model(ModelsGetParams {
                model: package_model.slug.clone(),
            })
            .await
            .unwrap()
            .model
            .summary
            .name,
        package_model.name
    );
    assert_eq!(backend.list_councils().await.unwrap().councils.len(), 1);
    assert_eq!(
        backend
            .get_council(CouncilsGetParams {
                council: Slug::derive(&package_council.name),
            })
            .await
            .unwrap()
            .council
            .summary
            .name,
        package_council.name
    );
    assert_eq!(
        backend
            .list_context_blocks()
            .await
            .unwrap()
            .context_blocks
            .len(),
        1
    );
    assert_eq!(
        backend
            .get_context_block(ContextBlocksGetParams {
                context_block: package_context_block.slug(),
            })
            .await
            .unwrap()
            .context_block
            .summary
            .name,
        package_context_block.name
    );
}

#[tokio::test]
async fn platform_manifest_backend_returns_package_overlay_abilities_regardless_of_resource_scope()
{
    let temp = tempdir().unwrap();
    let root = temp.path();
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let store = Arc::new(LocalManifestStore::new(root.join("manifests")));
    let package_ability = AbilityManifest {
        name: "build_routine".into(),
        path: Some("build".into()),
        description: Some("Build routines".into()),
        activation_condition: "Use for routine writes.".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "Build a routine.".into(),
        },
        platform_scopes: vec!["routines:write".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "package".into(),
        read_only: true,
        metadata: serde_json::Value::Null,
    };
    let client = PlatformManifestClient::new("http://localhost:3001", "test-api-key").unwrap();
    let backend =
        PlatformManifestBackend::new(store, client, PlatformPayloadEncoder::new(auth_provider))
            .with_read_only_manifest(Arc::new(Manifest {
                abilities: vec![package_ability],
                ..Default::default()
            }));

    let abilities = backend.list_abilities().await.unwrap();
    assert_eq!(abilities.abilities.len(), 1);
    assert_eq!(abilities.abilities[0].name, "build_routine");
    let ability = backend
        .get_ability(AbilitiesGetParams {
            ability: Slug::derive("build_routine"),
        })
        .await
        .unwrap()
        .ability;
    assert_eq!(ability.summary.name, "build_routine");
}

#[tokio::test]
async fn platform_manifest_backend_prefers_local_ability_over_package_overlay() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let auth_provider = Arc::new(WorkerAuthProvider::load_or_create(root.join("crypto")).unwrap());
    let store = Arc::new(LocalManifestStore::new(root.join("manifests")));
    let mut local_ability = AbilityManifest {
        name: "build_routine".into(),
        path: Some("build".into()),
        description: Some("Local build routine".into()),
        activation_condition: "Use the local ability.".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "Local routine builder.".into(),
        },
        platform_scopes: vec!["routines:read".into()],
        mcp_servers: vec![],
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    store
        .replace_manifest(&Manifest {
            abilities: vec![local_ability.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    local_ability.prompt_config.developer_prompt = "Package routine builder.".into();
    local_ability.source_type = "package".into();
    local_ability.read_only = true;

    let client = PlatformManifestClient::new("http://localhost:3001", "test-api-key").unwrap();
    let backend =
        PlatformManifestBackend::new(store, client, PlatformPayloadEncoder::new(auth_provider))
            .with_read_only_manifest(Arc::new(Manifest {
                abilities: vec![local_ability],
                ..Default::default()
            }));

    let abilities = backend.list_abilities().await.unwrap();
    assert_eq!(abilities.abilities.len(), 1);
    let ability = backend
        .get_ability(AbilitiesGetParams {
            ability: Slug::derive("build_routine"),
        })
        .await
        .unwrap()
        .ability;
    assert_eq!(
        ability.prompt_config.developer_prompt,
        "Local routine builder."
    );
}
