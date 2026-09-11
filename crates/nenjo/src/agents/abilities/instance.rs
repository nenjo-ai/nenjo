//! Construct an isolated ability instance with its own tools and assignments.

use std::collections::HashSet;
use std::sync::Arc;

use crate::agents::async_ops::AsyncOpManager;
use crate::agents::delegation::DELEGATE_TO_TOOL_NAME;
use crate::agents::instance::{AgentExecutionMode, AgentInstance, AgentPromptState, AgentRuntime};
use crate::manifest::{AbilityManifest, AgentManifest, PromptConfig};
use crate::provider::{ProviderRuntime, ToolContext, ToolFactory, knowledge_read_scope_granted};
use crate::tools::ToolOrigin;

/// Build a child with the caller's model/system context and the ability's assignments.
///
/// Factory tools are rebuilt from the ability's scopes, MCP servers, and media.
/// Caller host tools are preserved by name, while caller platform/MCP tools,
/// domains, ability assignments, and hooks do not leak into the child.
pub(super) async fn build_ability_instance<P>(
    caller: &AgentInstance<P>,
    ability: &AbilityManifest,
) -> AgentInstance<P>
where
    P: ProviderRuntime,
{
    let prompt_config = PromptConfig {
        system_prompt: caller.prompt_config().system_prompt.clone(),
        developer_prompt: ability.prompt_config.developer_prompt.clone(),
        memory_profile: caller.prompt_config().memory_profile.clone(),
    };

    let mut scoped_manifest = scoped_tool_manifest(caller, ability);

    let mut scoped_security = (*caller.runtime.security).clone();
    for env_name in ability_runtime_env_names(ability) {
        if !scoped_security
            .forwarded_env_names
            .iter()
            .any(|existing| existing == &env_name)
        {
            scoped_security.forwarded_env_names.push(env_name);
        }
    }
    let scoped_security = Arc::new(scoped_security);

    let mut tools = if let Some(provider) = caller.runtime.provider_runtime.as_ref() {
        let mut tools = provider
            .tool_factory()
            .create_tools_with_context(
                &scoped_manifest,
                scoped_security.clone(),
                ToolContext {
                    project_slug: Some(caller.prompt.context.current_project.slug.to_string()),
                    current_session_id: caller.runtime.current_session_id,
                },
            )
            .await;
        if knowledge_read_scope_granted(&scoped_manifest.platform_scopes) {
            let knowledge_policy = crate::package_resolve::policy_from_agent_metadata(
                Some(&ability.source_type),
                Some(&ability.metadata),
            );
            tools.extend(provider.create_knowledge_tools_with_policy(knowledge_policy));
        }
        tools
    } else {
        Vec::new()
    };

    let mut tool_names: HashSet<String> =
        tools.iter().map(|tool| tool.name().to_string()).collect();
    for tool in
        caller.runtime.tools.iter().filter(|tool| {
            tool.origin() == ToolOrigin::Host && tool.name() != DELEGATE_TO_TOOL_NAME
        })
    {
        if tool_names.insert(tool.name().to_string()) {
            tools.push(tool.clone());
        }
    }

    let mut prompt_context = caller.prompt.context.clone();
    prompt_context.active_domain = None;
    prompt_context.append_active_domain_addon = false;

    scoped_manifest.name = format!("{}:{}", caller.name(), ability.name);
    scoped_manifest.description = Some(
        ability
            .description
            .clone()
            .unwrap_or_else(|| caller.description().to_string()),
    );
    scoped_manifest.prompt_config = prompt_config;

    let execution_cancel = caller.runtime.execution_cancel.child_token();
    let async_ops = AsyncOpManager::with_cancel(execution_cancel.clone());

    AgentInstance {
        manifest: scoped_manifest,
        model_manifest: caller.model_manifest.clone(),
        model: caller.model.clone(),
        prompt: AgentPromptState {
            context: prompt_context,
            renderer: caller.prompt.renderer.clone(),
            memory_context: caller.prompt.memory_context.clone(),
        },
        runtime: AgentRuntime {
            tools,
            security: scoped_security,
            config: caller.runtime.config.clone(),
            provider_runtime: caller.runtime.provider_runtime.clone(),
            sub_agent_ctx: caller.runtime.sub_agent_ctx.clone(),
            async_ops,
            execution_cancel,
            execution_mode: AgentExecutionMode::Ability,
            hook_runtime: None,
            current_session_id: caller.runtime.current_session_id,
        },
    }
}

/// Scope factory inputs before replacing identity and prompts for the child runner.
fn scoped_tool_manifest<P: ProviderRuntime>(
    caller: &AgentInstance<P>,
    ability: &AbilityManifest,
) -> AgentManifest {
    let mut manifest = caller.manifest.clone();
    manifest.platform_scopes = unique_assignments(&ability.platform_scopes);
    manifest.mcp_servers = unique_assignments(&ability.mcp_servers);
    manifest.media = ability.media.clone();
    manifest.abilities.clear();
    manifest.domains.clear();
    manifest
}

/// Remove duplicates while retaining the assignment order supplied by the manifest.
fn unique_assignments<T: Clone + PartialEq>(values: &[T]) -> Vec<T> {
    let mut unique = Vec::with_capacity(values.len());
    for value in values {
        if !unique.contains(value) {
            unique.push(value.clone());
        }
    }
    unique
}

/// Read only valid environment-variable names declared in ability metadata.
fn ability_runtime_env_names(ability: &AbilityManifest) -> Vec<String> {
    ability
        .metadata
        .pointer("/runtime/env_names")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .filter(|name| {
                    let mut chars = name.chars();
                    let first = chars.next().unwrap_or_default();
                    (first.is_ascii_alphabetic() || first == '_')
                        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
