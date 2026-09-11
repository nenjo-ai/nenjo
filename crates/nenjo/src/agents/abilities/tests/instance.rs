//! Tests for ability instance behavior.

use super::*;

#[tokio::test]
async fn ability_assignments_and_environment_are_deduplicated_without_mutating_the_caller() {
    let mut caller = test_instance_with_active_domain();
    caller.runtime.security = Arc::new(ToolSecurity {
        forwarded_env_names: vec!["BASE".into()],
        ..ToolSecurity::default()
    });
    let ability = AbilityManifest::builder()
        .with_slug(crate::Slug::derive("scoped"))
        .with_name("scoped")
        .with_prompt("Use only assigned tools.")
        .with_platform_scopes(vec!["agents:read".into(), "agents:read".into()])
        .with_mcp_servers(vec![
            crate::Slug::derive("bookmarks"),
            crate::Slug::derive("bookmarks"),
        ])
        .with_metadata(serde_json::json!({"runtime": {"env_names": [
            "TOKEN", "TOKEN", "", "bad-name", "9INVALID", null, "_VALID", "é", "BASE"
        ]}}))
        .build()
        .unwrap();
    let child = build_ability_instance(&caller, &ability).await;
    assert_eq!(child.manifest.platform_scopes, ["agents:read"]);
    assert_eq!(
        child.manifest.mcp_servers,
        [crate::Slug::derive("bookmarks")]
    );
    assert_eq!(
        child.runtime.security.forwarded_env_names,
        ["BASE", "TOKEN", "_VALID"]
    );
    assert_eq!(caller.runtime.security.forwarded_env_names, ["BASE"]);
    assert!(caller.manifest.mcp_servers.is_empty());
}

#[tokio::test]
async fn ability_sub_instance_uses_ability_prompt_without_domain_addon() {
    let mut caller = test_instance_with_active_domain();
    caller.manifest.abilities = vec![crate::Slug::derive("caller_ability")];
    caller.manifest.domains = vec![crate::Slug::derive("creator")];
    let ability = AbilityManifest {
        slug: crate::Slug::derive("agent-builder"),
        name: "agent_builder".into(),
        path: Some("nenjo/platform".into()),
        description: Some("Builds agents".into()),
        activation_condition: "When building agents".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "ability developer".into(),
        },
        platform_scopes: vec!["agents:write".into()],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let sub_instance = build_ability_instance(&caller, &ability).await;
    let prompts = sub_instance
        .build_prompts(&AgentRun::chat(ChatInput {
            message: "build an agent".into(),
            history: vec![],
            project: None,
            replayed_turn_contexts: Vec::new(),
            artifacts: Vec::new(),
            timezone: chrono_tz::UTC,
        }))
        .unwrap();

    assert_eq!(prompts.system, "caller system");
    assert_eq!(prompts.developer, "ability developer");
    assert!(!prompts.developer.contains("domain addon"));
    assert!(!sub_instance.prompt.context.append_active_domain_addon);
    assert!(sub_instance.prompt.context.active_domain.is_none());
    assert_eq!(sub_instance.manifest.platform_scopes, vec!["agents:write"]);
    assert!(sub_instance.manifest.abilities.is_empty());
    assert!(sub_instance.manifest.domains.is_empty());
    let tool_names: Vec<_> = sub_instance
        .runtime
        .tools
        .iter()
        .map(|tool| tool.name())
        .collect();
    assert!(tool_names.contains(&"list_agents"));
    assert!(tool_names.contains(&"create_agent"));
    assert!(!tool_names.contains(&DELEGATE_TO_TOOL_NAME));
}

#[tokio::test]
async fn ability_sub_instance_does_not_inherit_caller_scopes_or_assignments() {
    let mut caller = test_instance_with_active_domain();
    caller.manifest.platform_scopes = vec!["agents:read".into()];
    caller.manifest.abilities = vec![crate::Slug::derive("caller_ability")];
    caller.manifest.domains = vec![crate::Slug::derive("creator")];
    let ability = AbilityManifest {
        slug: crate::Slug::derive("isolated"),
        name: "isolated".into(),
        path: Some("nenjo/platform".into()),
        description: Some("Runs isolated".into()),
        activation_condition: "When isolation is needed".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "ability developer".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let sub_instance = build_ability_instance(&caller, &ability).await;
    let tool_names: Vec<_> = sub_instance
        .runtime
        .tools
        .iter()
        .map(|tool| tool.name())
        .collect();

    assert!(!tool_names.contains(&"list_agents"));
    assert!(!tool_names.contains(&"create_agent"));
    assert!(!tool_names.contains(&"respond_to_user"));
    assert_eq!(
        sub_instance.runtime.execution_mode,
        AgentExecutionMode::Ability
    );
    assert!(sub_instance.manifest.platform_scopes.is_empty());
    assert!(sub_instance.manifest.abilities.is_empty());
    assert!(sub_instance.manifest.domains.is_empty());
    assert!(sub_instance.prompt.context.active_domain.is_none());
}

#[tokio::test]
async fn ability_sub_instance_renders_context_blocks_and_session_identity() {
    let mut caller = test_instance_with_active_domain();
    caller.manifest.prompt_config.system_prompt = "{{ pkg.nenjo.core.methodology }}".into();
    caller.prompt.renderer = ContextRenderer::from_blocks(&[
        RenderContextBlock {
            name: "methodology".into(),
            path: "pkg/nenjo/core".into(),
            template: "<methodology>METHOD</methodology>".into(),
            package_name: None,
            package_version: None,
        },
        RenderContextBlock {
            name: "tool_usage".into(),
            path: "pkg/nenjo/core".into(),
            template: "<tool_usage>TOOLS</tool_usage>".into(),
            package_name: None,
            package_version: None,
        },
    ]);
    let ability = AbilityManifest {
        slug: crate::Slug::derive("agent-builder"),
        name: "agent_builder".into(),
        path: Some("nenjo/platform".into()),
        description: Some("Builds agents".into()),
        activation_condition: "When building agents".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "{{ pkg.nenjo.core.tool_usage }}".into(),
        },
        platform_scopes: vec!["agents:write".into()],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let sub_instance = build_ability_instance(&caller, &ability).await;
    let prompts = sub_instance
        .build_prompts(&AgentRun::chat(ChatInput {
            message: "build an agent".into(),
            history: vec![],
            project: None,
            replayed_turn_contexts: Vec::new(),
            artifacts: Vec::new(),
            timezone: chrono_tz::UTC,
        }))
        .unwrap();

    assert_eq!(prompts.system, "<methodology>METHOD</methodology>");
    assert_eq!(prompts.developer, "<tool_usage>TOOLS</tool_usage>");
    assert!(prompts.session_context.messages().iter().any(|context| {
        context.authority() == nenjo_models::RuntimeContextAuthority::Control
            && context.content().contains("name=\"nenji:agent_builder\"")
            && context.content().contains("description=\"Builds agents\"")
    }));
    assert!(
        prompts
            .turn_context
            .messages()
            .iter()
            .any(|context| context.content().contains("kind=\"chat\""))
    );
}

#[tokio::test]
async fn ability_sub_instance_preserves_non_factory_tools_without_duplicates() {
    let mut caller = test_instance_with_active_domain();
    caller.runtime.tools = vec![
        Arc::new(TestTool {
            name: "shell",
            origin: ToolOrigin::Host,
        }),
        Arc::new(TestTool {
            name: "remember_fact",
            origin: ToolOrigin::Host,
        }),
        Arc::new(TestTool {
            name: DELEGATE_TO_TOOL_NAME,
            origin: ToolOrigin::Host,
        }),
    ];
    let ability = AbilityManifest {
        slug: crate::Slug::derive("agent-builder"),
        name: "agent_builder".into(),
        path: Some("nenjo/platform".into()),
        description: Some("Builds agents".into()),
        activation_condition: "When building agents".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "ability developer".into(),
        },
        platform_scopes: vec!["agents:write".into()],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let sub_instance = build_ability_instance(&caller, &ability).await;
    let tool_names: Vec<_> = sub_instance
        .runtime
        .tools
        .iter()
        .map(|tool| tool.name())
        .collect();

    assert_eq!(
        tool_names.iter().filter(|name| **name == "shell").count(),
        1
    );
    assert!(tool_names.contains(&"remember_fact"));
    assert!(!tool_names.contains(&DELEGATE_TO_TOOL_NAME));
}

#[tokio::test]
async fn ability_sub_instance_does_not_inherit_caller_mcp_tools() {
    let mut caller = test_instance_with_active_domain();
    caller.manifest.mcp_servers = vec![crate::Slug::derive("caller-mcp")];
    caller.runtime.tools = vec![
        Arc::new(TestTool {
            name: "shell",
            origin: ToolOrigin::Host,
        }),
        Arc::new(TestTool {
            name: "caller_mcp_tool",
            origin: ToolOrigin::Mcp,
        }),
    ];
    let ability = AbilityManifest {
        slug: crate::Slug::derive("agent-builder"),
        name: "agent_builder".into(),
        path: Some("nenjo/platform".into()),
        description: Some("Builds agents".into()),
        activation_condition: "When building agents".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "ability developer".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let sub_instance = build_ability_instance(&caller, &ability).await;
    let tool_names: Vec<_> = sub_instance
        .runtime
        .tools
        .iter()
        .map(|tool| tool.name())
        .collect();

    assert!(tool_names.contains(&"shell"));
    assert!(!tool_names.contains(&"caller_mcp_tool"));
    assert!(sub_instance.manifest.mcp_servers.is_empty());
}
