use std::sync::Arc;

use nenjo::agents::prompts::PromptConfig;
use nenjo::manifest::AgentManifest;
use nenjo::manifest::local::LocalManifestStore;
use nenjo::manifest::{AbilityManifest, DomainManifest, Manifest};
use nenjo::{ManifestWriter, ToolFactory};
use nenjo_platform::{
    AbilitiesGetParams, AbilityManifestBackend, AgentManifestBackend, AgentsGetParams,
    DomainManifestBackend, DomainsGetParams, ManifestAccessPolicy, PlatformManifestBackend,
    PlatformManifestClient,
};
use tempfile::tempdir;
use uuid::Uuid;

use super::*;
use crate::crypto::WorkerAuthProvider;
use crate::tools::NativeRuntime;
use crate::tools::platform_payload::PlatformPayloadEncoder;
use crate::tools::platform_services::PlatformToolServices;

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
    )
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
        id: Uuid::new_v4(),
        name: "visible-agent".into(),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model_id: None,
        domain_ids: vec![],
        platform_scopes: vec!["projects:read".into()],
        mcp_server_ids: vec![],
        ability_ids: vec![],
        prompt_locked: false,
        heartbeat: None,
    };
    let hidden_agent = AgentManifest {
        id: Uuid::new_v4(),
        name: "hidden-agent".into(),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model_id: None,
        domain_ids: vec![],
        platform_scopes: vec!["projects:write".into()],
        mcp_server_ids: vec![],
        ability_ids: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let visible_ability = AbilityManifest {
        id: Uuid::new_v4(),
        name: "visible-ability".into(),
        path: String::new(),
        display_name: None,
        description: None,
        activation_condition: "visible".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "visible prompt".into(),
        },
        platform_scopes: vec!["projects:read".into()],
        mcp_server_ids: vec![],
        tool_name: "visible_ability".into(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let hidden_ability = AbilityManifest {
        id: Uuid::new_v4(),
        name: "hidden-ability".into(),
        path: String::new(),
        display_name: None,
        description: None,
        activation_condition: "hidden".into(),
        prompt_config: nenjo::types::AbilityPromptConfig {
            developer_prompt: "hidden prompt".into(),
        },
        platform_scopes: vec!["projects:write".into()],
        mcp_server_ids: vec![],
        tool_name: "hidden_ability".into(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let visible_domain = DomainManifest {
        id: Uuid::new_v4(),
        name: "visible-domain".into(),
        path: String::new(),
        display_name: "Visible Domain".into(),
        description: None,
        command: "#visible".into(),
        platform_scopes: vec!["projects:read".into()],
        ability_ids: vec![],
        mcp_server_ids: vec![],
        prompt_config: nenjo::types::DomainPromptConfig::default(),
    };
    let hidden_domain = DomainManifest {
        id: Uuid::new_v4(),
        name: "hidden-domain".into(),
        path: String::new(),
        display_name: "Hidden Domain".into(),
        description: None,
        command: "#hidden".into(),
        platform_scopes: vec!["projects:write".into()],
        ability_ids: vec![],
        mcp_server_ids: vec![],
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
async fn worker_factory_exposes_manifest_tools_without_legacy_platform_tools() {
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
        id: Uuid::new_v4(),
        name: "tester".into(),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model_id: None,
        domain_ids: vec![],
        platform_scopes: vec![
            "agents:read".into(),
            "agents:write".into(),
            "projects:read".into(),
        ],
        mcp_server_ids: vec![],
        ability_ids: vec![],
        prompt_locked: false,
        heartbeat: None,
    };

    let tools = factory.create_tools(&agent).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(names.iter().any(|name| name == "list_agents"));
    assert!(names.iter().any(|name| name == "get_agent"));
    assert!(names.iter().any(|name| name == "get_agent_prompt"));
    assert!(names.iter().any(|name| name == "create_agent"));
    assert!(names.iter().any(|name| name == "update_agent"));
    assert!(names.iter().any(|name| name == "list_projects"));
    assert!(names.iter().any(|name| name == "get_project"));
    assert!(names.iter().any(|name| name == "list_knowledge_packs"));
    assert!(names.iter().any(|name| name == "read_knowledge_doc"));
    assert!(names.iter().any(|name| name == "search_knowledge"));
    assert!(names.iter().any(|name| name == "search_knowledge_paths"));
    assert!(names.iter().any(|name| name == "list_project_tasks"));
    assert!(names.iter().any(|name| name == "get_project_task"));
    assert!(
        names
            .iter()
            .any(|name| name == "list_project_execution_runs")
    );
    assert!(names.iter().any(|name| name == "get_project_execution_run"));
    assert!(!names.iter().any(|name| name == "list_builtin_docs"));
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
        ..agent
    };
    let tools = factory.create_tools(&agent_without_project_scope).await;
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();

    assert!(names.iter().any(|name| name == "list_knowledge_packs"));
    assert!(names.iter().any(|name| name == "read_knowledge_doc"));
    assert!(names.iter().any(|name| name == "search_knowledge"));
    assert!(names.iter().any(|name| name == "search_knowledge_paths"));
    assert!(!names.iter().any(|name| name == "list_projects"));
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
        id: Uuid::new_v4(),
        name: "tester".into(),
        description: None,
        prompt_config: PromptConfig::default(),
        color: None,
        model_id: None,
        domain_ids: vec![],
        platform_scopes: vec!["projects:write".into()],
        mcp_server_ids: vec![],
        ability_ids: vec![],
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
    assert_eq!(agents.agents[0].id, visible_agent.id);
    assert!(
        backend
            .get_agent(AgentsGetParams {
                id: hidden_agent.id
            })
            .await
            .is_err()
    );

    let abilities = backend.list_abilities().await.unwrap();
    assert_eq!(abilities.abilities.len(), 1);
    assert_eq!(abilities.abilities[0].id, visible_ability.id);
    assert!(
        backend
            .get_ability(AbilitiesGetParams {
                id: hidden_ability.id
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
            .any(|domain| domain.id == visible_domain.id)
    );
    assert!(
        backend
            .get_domain(DomainsGetParams {
                id: hidden_domain.id
            })
            .await
            .is_err()
    );
}
