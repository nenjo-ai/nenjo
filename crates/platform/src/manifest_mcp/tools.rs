use nenjo::ToolSpec;

use super::abilities::ability_tools;
use super::agents::agent_tools;
use super::context_blocks::context_block_tools;
use super::councils::council_tools;
use super::domains::domain_tools;
use super::models::model_tools;
use super::projects::project_tools;
use super::routines::routine_tools;

/// Return the complete manifest MCP tool registry.
pub fn all_tools() -> Vec<ToolSpec> {
    let mut tools = Vec::new();
    tools.extend(agent_tools());
    tools.extend(ability_tools());
    tools.extend(domain_tools());
    tools.extend(project_tools().into_iter().filter(|tool| {
        matches!(
            tool.name.as_str(),
            "list_projects"
                | "get_project"
                | "list_project_documents"
                | "read_project_document_manifest"
                | "read_project_document"
                | "search_project_documents"
                | "search_project_document_paths"
                | "list_project_document_tree"
                | "list_project_document_neighbors"
                | "create_project"
                | "update_project"
                | "delete_project"
                | "create_project_document"
                | "update_project_document_content"
                | "delete_project_document"
        )
    }));
    tools.extend(routine_tools());
    tools.extend(model_tools());
    tools.extend(council_tools());
    tools.extend(context_block_tools());
    tools
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use nenjo::ToolCategory;

    use super::all_tools;

    fn tool_snapshot() -> Vec<(String, ToolCategory)> {
        all_tools()
            .into_iter()
            .map(|tool| (tool.name, tool.category))
            .collect()
    }

    #[test]
    fn manifest_tool_registry_has_no_duplicate_names() {
        let tools = all_tools();
        let mut names = HashSet::new();

        for tool in &tools {
            assert!(
                names.insert(tool.name.clone()),
                "duplicate manifest tool registered: {}",
                tool.name
            );
        }
    }

    #[test]
    fn manifest_tool_registry_contains_expected_subresource_tools() {
        let names: HashSet<_> = all_tools().into_iter().map(|tool| tool.name).collect();

        for expected in [
            "get_agent_prompt",
            "update_agent_prompt",
            "get_ability_prompt",
            "update_ability_prompt",
            "get_domain_prompt",
            "update_domain_prompt",
            "get_context_block_content",
            "update_context_block_content",
        ] {
            assert!(names.contains(expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn manifest_list_tools_have_empty_object_params() {
        let tools = all_tools();

        for tool_name in [
            "list_agents",
            "list_abilities",
            "list_domains",
            "list_projects",
            "list_routines",
            "list_models",
            "list_councils",
            "list_context_blocks",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name == tool_name)
                .unwrap_or_else(|| panic!("missing tool: {tool_name}"));
            assert_eq!(tool.parameters["type"], "object");
            assert_eq!(tool.parameters["additionalProperties"], false);
            assert_eq!(tool.parameters["properties"], serde_json::json!({}));
        }
    }

    #[test]
    fn scoped_resource_mutation_tools_do_not_accept_platform_scopes() {
        let tools = all_tools();

        for tool_name in [
            "create_agent",
            "update_agent",
            "create_ability",
            "update_ability",
            "create_domain",
            "update_domain",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name == tool_name)
                .unwrap_or_else(|| panic!("missing tool: {tool_name}"));
            assert!(
                tool.parameters["properties"]
                    .get("platform_scopes")
                    .is_none(),
                "{tool_name} should not expose platform_scopes"
            );
            assert_eq!(tool.parameters["additionalProperties"], false);
        }
    }

    #[test]
    fn manifest_tool_registry_matches_expected_name_and_category_snapshot() {
        assert_eq!(
            tool_snapshot(),
            vec![
                ("list_agents".into(), ToolCategory::Read),
                ("get_agent".into(), ToolCategory::Read),
                ("get_agent_prompt".into(), ToolCategory::Read),
                ("create_agent".into(), ToolCategory::Write),
                ("update_agent".into(), ToolCategory::Write),
                ("update_agent_prompt".into(), ToolCategory::Write),
                ("delete_agent".into(), ToolCategory::Write),
                ("list_abilities".into(), ToolCategory::Read),
                ("get_ability".into(), ToolCategory::Read),
                ("get_ability_prompt".into(), ToolCategory::Read),
                ("create_ability".into(), ToolCategory::Write),
                ("update_ability".into(), ToolCategory::Write),
                ("update_ability_prompt".into(), ToolCategory::Write),
                ("delete_ability".into(), ToolCategory::Write),
                ("list_domains".into(), ToolCategory::Read),
                ("get_domain".into(), ToolCategory::Read),
                ("get_domain_prompt".into(), ToolCategory::Read),
                ("create_domain".into(), ToolCategory::Write),
                ("update_domain".into(), ToolCategory::Write),
                ("update_domain_prompt".into(), ToolCategory::Write),
                ("delete_domain".into(), ToolCategory::Write),
                ("list_projects".into(), ToolCategory::Read),
                ("get_project".into(), ToolCategory::Read),
                ("list_project_documents".into(), ToolCategory::Read),
                ("read_project_document_manifest".into(), ToolCategory::Read),
                ("read_project_document".into(), ToolCategory::Read),
                ("search_project_documents".into(), ToolCategory::Read),
                ("search_project_document_paths".into(), ToolCategory::Read),
                ("list_project_document_tree".into(), ToolCategory::Read),
                ("list_project_document_neighbors".into(), ToolCategory::Read),
                ("create_project".into(), ToolCategory::Write),
                ("update_project".into(), ToolCategory::Write),
                ("delete_project".into(), ToolCategory::Write),
                ("create_project_document".into(), ToolCategory::Write),
                ("delete_project_document".into(), ToolCategory::Write),
                (
                    "update_project_document_content".into(),
                    ToolCategory::Write
                ),
                ("list_routines".into(), ToolCategory::Read),
                ("get_routine".into(), ToolCategory::Read),
                ("create_routine".into(), ToolCategory::Write),
                ("update_routine".into(), ToolCategory::Write),
                ("delete_routine".into(), ToolCategory::Write),
                ("list_models".into(), ToolCategory::Read),
                ("get_model".into(), ToolCategory::Read),
                ("create_model".into(), ToolCategory::Write),
                ("update_model".into(), ToolCategory::Write),
                ("delete_model".into(), ToolCategory::Write),
                ("list_councils".into(), ToolCategory::Read),
                ("get_council".into(), ToolCategory::Read),
                ("create_council".into(), ToolCategory::Write),
                ("update_council".into(), ToolCategory::Write),
                ("add_council_member".into(), ToolCategory::Write),
                ("update_council_member".into(), ToolCategory::Write),
                ("remove_council_member".into(), ToolCategory::Write),
                ("delete_council".into(), ToolCategory::Write),
                ("list_context_blocks".into(), ToolCategory::Read),
                ("get_context_block".into(), ToolCategory::Read),
                ("get_context_block_content".into(), ToolCategory::Read),
                ("create_context_block".into(), ToolCategory::Write),
                ("update_context_block".into(), ToolCategory::Write),
                ("update_context_block_content".into(), ToolCategory::Write),
                ("delete_context_block".into(), ToolCategory::Write),
            ]
        );
    }

    #[test]
    fn manifest_tool_registry_descriptions_are_non_empty() {
        for tool in all_tools() {
            assert!(
                !tool.description.trim().is_empty(),
                "tool {} has empty description",
                tool.name
            );
        }
    }
}
