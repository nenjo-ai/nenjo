//! Tests for ability broker behavior.

use super::*;

#[test]
fn use_ability_schema_requires_self_contained_input() {
    let ability = AbilityManifest {
        slug: crate::Slug::derive("review"),
        name: "review".into(),
        path: Some("review".into()),
        description: Some("Reviews code".into()),
        activation_condition: "When code review is needed".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "review code".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let registry = Arc::new(AbilityRegistry::new(&[ability]).unwrap());
    let tool = UseAbilityTool::new(registry, Arc::new(test_instance_with_active_domain()));

    let description = tool.description();
    let schema = tool.parameters_schema();
    let task_description = schema["properties"]["input"]["description"]
        .as_str()
        .unwrap_or_default();

    assert!(description.contains("Invoke one ability"));
    assert!(task_description.contains("self-contained delegated task"));
    assert!(task_description.contains("code snippets"));
    assert!(task_description.contains("without access to the caller conversation"));
}

#[tokio::test]
async fn list_assigned_abilities_returns_all_assigned_ability_metadata() {
    let review = AbilityManifest {
        slug: crate::Slug::derive("code-review"),
        name: "Code Review".into(),
        path: Some("review".into()),
        description: Some("Reviews code".into()),
        activation_condition: "When code review is needed".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "review code".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let docs = AbilityManifest {
        slug: crate::Slug::derive("search-docs"),
        name: "Search Docs!".into(),
        path: Some("docs".into()),
        description: None,
        activation_condition: "When documentation lookup is needed".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "search docs".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let registry = Arc::new(AbilityRegistry::new(&[review, docs]).unwrap());
    let tool = ListAssignedAbilitiesTool::new(registry);

    let result = tool.execute(serde_json::json!({})).await.unwrap();
    let output: serde_json::Value = serde_json::from_str(result.output.as_text().unwrap()).unwrap();

    assert!(result.success);
    assert_eq!(output["abilities"][0]["name"], "Code Review");
    assert!(output["abilities"][0].get("ability_id").is_none());
    assert!(output["abilities"][0].get("display_name").is_none());
    assert_eq!(
        output["abilities"][0]["activation_condition"],
        "When code review is needed"
    );
    assert_eq!(output["abilities"][1]["name"], "Search Docs!");
}

#[test]
fn ability_tool_filter_does_not_match_manifest_resource_list_tool() {
    assert!(is_ability_tool("list_assigned_abilities"));
    assert!(is_ability_tool("use_ability"));
    assert!(!is_ability_tool("inspect"));
    assert!(!is_ability_tool("stop"));
    assert!(!is_ability_tool("wait"));
    assert!(!is_ability_tool("list_abilities"));
}

#[test]
fn duplicate_ability_ids_are_rejected() {
    let first = AbilityManifest {
        slug: crate::Slug::derive("frontend-code-review"),
        name: "code_review".into(),
        path: Some("frontend".into()),
        description: None,
        activation_condition: "frontend".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "frontend".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };
    let second = AbilityManifest {
        slug: crate::Slug::derive("backend-code-review"),
        name: "code_review".into(),
        path: Some("backend".into()),
        description: None,
        activation_condition: "backend".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "backend".into(),
        },
        platform_scopes: vec![],
        mcp_servers: vec![],
        script_tools: vec![],
        media: vec![],
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    let error = AbilityRegistry::new(&[first, second]).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("duplicate ability_id 'code_review'")
    );
}
