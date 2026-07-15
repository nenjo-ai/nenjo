use nenjo::ToolSpec;

use super::abilities::ability_tools;
use super::agents::agent_tools;
use super::commands::command_tools;
use super::context_blocks::context_block_tools;
use super::councils::council_tools;
use super::domains::domain_tools;
use super::library::library_tools;
use super::models::model_tools;
use super::projects::project_tools;
use super::routines::routine_tools;

/// Return the complete manifest MCP tool registry.
pub fn all_tools() -> Vec<ToolSpec> {
    let mut tools = Vec::new();
    tools.extend(agent_tools());
    tools.extend(ability_tools());
    tools.extend(command_tools());
    tools.extend(domain_tools());
    tools.extend(project_tools());
    tools.extend(library_tools());
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
            "configure_ability",
            "configure_domain",
            "configure_context_block",
            "list_commands",
            "get_command",
            "configure_command",
        ] {
            assert!(names.contains(expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn manifest_tool_registry_exposes_library_mutation_tools_only() {
        let names: HashSet<_> = all_tools().into_iter().map(|tool| tool.name).collect();

        for expected in [
            "create_knowledge_pack",
            "update_knowledge_pack",
            "create_knowledge_doc",
            "update_knowledge_doc",
            "delete_knowledge_doc",
        ] {
            assert!(names.contains(expected), "missing tool: {expected}");
        }

        for removed in [
            "list_knowledge_packs",
            "read_knowledge_doc",
            "search_knowledge",
            "list_knowledge_neighbors",
            "list_knowledge_docs",
            "read_knowledge_doc_manifest",
            "search_knowledge_paths",
            "list_knowledge_tree",
            "delete_knowledge_pack",
        ] {
            assert!(!names.contains(removed), "removed tool exposed: {removed}");
        }
    }

    #[test]
    fn manifest_list_tools_have_empty_object_params() {
        let tools = all_tools();

        for tool_name in [
            "list_agents",
            "list_abilities",
            "list_commands",
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
    fn configure_routine_schema_exposes_step_instructions_config() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|tool| tool.name == "configure_routine")
            .expect("missing configure_routine");
        let config_schema = &tool.parameters["properties"]["graph"]["properties"]["steps"]["items"]
            ["properties"]["config"];

        assert!(
            tool.parameters["properties"].get("id").is_none(),
            "configure_routine should not expose platform UUIDs to agents"
        );
        assert_eq!(config_schema["type"], "object");
        assert_eq!(config_schema["additionalProperties"], false);
        assert_eq!(
            config_schema["properties"]["instructions"]["type"],
            "string"
        );
        assert_eq!(
            config_schema["properties"]["metadata"]["type"],
            serde_json::json!(["object", "array", "string"])
        );
        assert!(
            config_schema["properties"]["instructions"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("Step-specific task instructions"),
            "routine step config should document instructions"
        );
        assert!(
            config_schema["description"]
                .as_str()
                .unwrap_or_default()
                .contains("edge metadata.max_attempts"),
            "routine step config should tell agents where retry budgets belong"
        );
    }

    #[test]
    fn configure_routine_schema_exposes_edge_handoff_schema_contract() {
        let tools = all_tools();
        let tool = tools
            .iter()
            .find(|tool| tool.name == "configure_routine")
            .expect("missing configure_routine");
        let graph_schema = &tool.parameters["properties"]["graph"];
        let metadata_schema =
            &graph_schema["properties"]["edges"]["items"]["properties"]["metadata"];
        let handoff_schema = &metadata_schema["properties"]["handoff_schema"];

        assert_eq!(graph_schema["type"], "object");
        assert!(
            graph_schema["description"]
                .as_str()
                .unwrap_or_default()
                .contains("do not serialize that object into a string"),
            "configure_routine must make graph's object shape explicit"
        );
        assert_eq!(handoff_schema["type"], "object");
        assert_eq!(handoff_schema["required"], serde_json::json!(["type"]));
        assert_eq!(
            handoff_schema["properties"]["type"]["enum"],
            serde_json::json!(["object"])
        );
        assert!(
            handoff_schema["description"]
                .as_str()
                .unwrap_or_default()
                .contains("Required for every edge whose source step is agent or gate"),
            "configure_routine must tell agents when handoff_schema is required"
        );
        assert!(
            metadata_schema["description"]
                .as_str()
                .unwrap_or_default()
                .contains("must define handoff_schema"),
            "edge metadata guidance must identify handoff_schema as the route contract"
        );
    }

    #[test]
    fn scoped_resource_mutation_tools_do_not_accept_platform_scopes() {
        let tools = all_tools();

        for tool_name in [
            "configure_agent",
            "configure_ability",
            "configure_domain",
            "configure_context_block",
            "create_knowledge_pack",
            "update_knowledge_pack",
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
    fn library_knowledge_doc_tools_describe_slug_workflow() {
        let tools = all_tools();
        let create = tools
            .iter()
            .find(|tool| tool.name == "create_knowledge_doc")
            .expect("missing create_knowledge_doc");
        let update = tools
            .iter()
            .find(|tool| tool.name == "update_knowledge_doc")
            .expect("missing update_knowledge_doc");
        let delete = tools
            .iter()
            .find(|tool| tool.name == "delete_knowledge_doc")
            .expect("missing delete_knowledge_doc");

        assert_eq!(
            create.parameters["required"],
            serde_json::json!(["pack", "filename", "content"])
        );
        assert_eq!(
            update.parameters["required"],
            serde_json::json!(["pack", "slug"])
        );
        assert_eq!(
            delete.parameters["required"],
            serde_json::json!(["pack", "slug"])
        );
        assert!(
            create
                .description
                .contains("returns it as knowledge_doc.slug"),
            "create_knowledge_doc must tell agents where to get the generated slug"
        );
        assert!(
            create
                .description
                .contains("derives the document slug from path plus filename"),
            "create_knowledge_doc must describe how the slug is derived"
        );
        assert!(create.parameters["properties"].get("slug").is_none());
        assert!(create.parameters["properties"].get("doc").is_none());
        assert!(update.parameters["properties"].get("doc").is_none());
        assert!(delete.parameters["properties"].get("doc").is_none());
        assert!(
            update.description.contains("Requires slug"),
            "update_knowledge_doc must require the returned document slug"
        );
        assert!(
            update.parameters["properties"]["related"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("replaces every outbound edge"),
            "related schema must document full replacement semantics"
        );
        assert!(
            create.parameters["properties"]["related"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("Targets must already exist"),
            "create related schema must explain target existence"
        );
        assert_eq!(
            update.parameters["properties"]["related"]["items"]["properties"]["type"]["enum"],
            serde_json::json!([
                "references",
                "depends_on",
                "defines",
                "part_of",
                "extends",
                "related_to",
                "governs",
                "classifies"
            ])
        );
    }

    #[test]
    fn manifest_tool_registry_matches_expected_name_and_category_snapshot() {
        assert_eq!(
            tool_snapshot(),
            vec![
                ("list_agents".into(), ToolCategory::Read),
                ("get_agent".into(), ToolCategory::Read),
                ("configure_agent".into(), ToolCategory::Write),
                ("list_abilities".into(), ToolCategory::Read),
                ("get_ability".into(), ToolCategory::Read),
                ("configure_ability".into(), ToolCategory::Write),
                ("list_commands".into(), ToolCategory::Read),
                ("get_command".into(), ToolCategory::Read),
                ("configure_command".into(), ToolCategory::Write),
                ("list_domains".into(), ToolCategory::Read),
                ("get_domain".into(), ToolCategory::Read),
                ("configure_domain".into(), ToolCategory::Write),
                ("list_projects".into(), ToolCategory::Read),
                ("get_project".into(), ToolCategory::Read),
                ("create_project".into(), ToolCategory::Write),
                ("update_project".into(), ToolCategory::Write),
                ("delete_project".into(), ToolCategory::Write),
                ("create_knowledge_pack".into(), ToolCategory::Write),
                ("update_knowledge_pack".into(), ToolCategory::Write),
                ("create_knowledge_doc".into(), ToolCategory::Write),
                ("delete_knowledge_doc".into(), ToolCategory::Write),
                ("update_knowledge_doc".into(), ToolCategory::Write),
                ("list_routines".into(), ToolCategory::Read),
                ("get_routine".into(), ToolCategory::Read),
                ("configure_routine".into(), ToolCategory::Write),
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
                ("configure_context_block".into(), ToolCategory::Write),
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
