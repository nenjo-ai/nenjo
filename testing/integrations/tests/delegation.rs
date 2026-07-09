//! Real LLM integration tests for native sub-agents.
//!
//! Requires `OPENROUTER_API_KEY` environment variable.
//! Tests are skipped automatically if the key is not set.

use std::sync::Arc;

use anyhow::Result;
use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, PromptTemplates,
    model_manifest_slug,
};
use nenjo::provider::{ModelProviderFactory, NoopToolFactory, Provider};
use nenjo::{AgentConfig, Slug};
use nenjo_models::ModelProvider;
use nenjo_models::openrouter::OpenRouterProvider;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct OpenRouterFactory {
    api_key: String,
}

impl ModelProviderFactory for OpenRouterFactory {
    fn create(&self, _provider_name: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(OpenRouterProvider::new(Some(&self.api_key))))
    }
}

fn get_api_key() -> Option<String> {
    match std::env::var("OPENROUTER_API_KEY") {
        Ok(key) if !key.is_empty() => Some(key),
        _ => None,
    }
}

fn make_model() -> ModelManifest {
    ModelManifest {
        slug: model_manifest_slug("openrouter", "deepseek/deepseek-v4-flash"),
        name: "openrouter".into(),
        description: None,
        model: "deepseek/deepseek-v4-flash".into(),
        model_provider: "openrouter".into(),
        temperature: Some(0.7),
        context_window: None,
        base_url: None,
        native_tools: vec![],
    }
}

fn make_project() -> ProjectManifest {
    ProjectManifest {
        name: "test-project".into(),
        slug: Slug::derive("test-project"),
        description: None,
        settings: serde_json::Value::Null,
    }
}

fn make_agent(name: &str, model: &ModelManifest, system_prompt: &str) -> AgentManifest {
    AgentManifest {
        name: name.into(),
        slug: Slug::derive(name),
        description: Some(format!("Test agent: {name}")),
        prompt_config: PromptConfig {
            system_prompt: system_prompt.into(),
            templates: PromptTemplates {
                chat_task: "{{ chat.message }}".into(),
                task_execution: String::new(),
                gate_eval: String::new(),
                ..Default::default()
            },
            ..Default::default()
        },
        color: None,
        model: Some(model_manifest_slug(&model.model_provider, &model.model)),
        domains: vec![],
        platform_scopes: vec![],
        mcp_servers: vec![],
        abilities: vec![],
        script_tools: vec![],
        media: vec![],
        prompt_locked: false,
        heartbeat: None,
        source_type: None,
        metadata: serde_json::json!({}),
    }
}

fn tool_call_names(output: &nenjo::TurnOutput) -> Vec<String> {
    output
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| serde_json::from_str::<serde_json::Value>(&message.content).ok())
        .filter_map(|value| {
            value
                .get("tool_calls")
                .and_then(|calls| calls.as_array())
                .cloned()
        })
        .flatten()
        .filter_map(|call| {
            call.get("name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect()
}

fn transcript_text(output: &nenjo::TurnOutput) -> String {
    output
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_payloads(output: &nenjo::TurnOutput) -> Vec<serde_json::Value> {
    output
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| serde_json::from_str::<serde_json::Value>(&message.content).ok())
        .filter_map(|message| {
            message
                .get("content")
                .and_then(|content| content.as_str())
                .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        })
        .collect()
}

fn delivered_to_slug(payloads: &[serde_json::Value], slug: &str) -> bool {
    payloads
        .iter()
        .filter_map(|payload| payload.get("sent").and_then(|sent| sent.as_array()))
        .flatten()
        .any(|item| {
            item.get("slug").and_then(|value| value.as_str()) == Some(slug)
                && item.get("status").and_then(|value| value.as_str()) == Some("delivered")
        })
}

fn stopped_slug(payloads: &[serde_json::Value], slug: &str) -> bool {
    payloads
        .iter()
        .filter_map(|payload| {
            payload
                .get("stopped")
                .and_then(|stopped| stopped.as_array())
        })
        .flatten()
        .any(|item| {
            item.get("slug").and_then(|value| value.as_str()) == Some(slug)
                && item.get("status").and_then(|value| value.as_str()) == Some("stopped")
        })
}

// ===========================================================================
// Tests
// ===========================================================================

/// A parent agent can spawn a normal agent manifest as a native sub-agent using
/// the model-facing sub-agent tools.
#[tokio::test]
async fn sub_agent_real_llm() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();

    let leader = make_agent(
        "leader",
        &model,
        r#"You are a deterministic native sub-agent smoke-test coordinator.

You MUST use native tool calls, not prose, to follow this exact protocol:
1. Call spawn_sub_agents with one sub-agent:
   - agent "specialist", slug "basic_specialist", prompt:
     "You are specialist. Return exactly SPECIALIST_BASIC_DONE."
   - task description "Return the required sentinel", goal "Produce SPECIALIST_BASIC_DONE",
     acceptance criteria ["Return exactly SPECIALIST_BASIC_DONE"]
2. Call wait for up to 20 seconds.
3. Final answer exactly: SUB_AGENT_BASIC_OK

Do not answer directly. Do not skip tool calls."#,
    );

    let specialist = make_agent(
        "specialist",
        &model,
        "You are a specialist agent. When you receive a task, respond with a \
         concise, helpful answer. Always include the word 'SPECIALIST' in \
         your response so we can verify delegation happened.",
    );

    let manifest = Manifest {
        agents: vec![leader, specialist],
        models: vec![model],
        projects: vec![make_project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("leader")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();

    let output = runner
        .chat("Run the native sub-agent smoke-test protocol now.")
        .await
        .expect("chat should succeed");

    println!("--- Sub-Agent Test ---");
    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);
    println!("Input tokens: {}", output.input_tokens);
    println!("Output tokens: {}", output.output_tokens);

    let tool_names = tool_call_names(&output);
    assert!(
        tool_names.iter().any(|name| name == "spawn_sub_agents"),
        "leader should have called spawn_sub_agents; saw {tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == "wait"),
        "leader should have called wait; saw {tool_names:?}"
    );

    let transcript = transcript_text(&output);
    assert!(
        transcript.contains("basic_specialist"),
        "sub-agent slug should appear in transcript: {transcript}"
    );
    assert!(
        output.text.contains("SUB_AGENT_BASIC_OK"),
        "final response should confirm protocol completion: {}",
        output.text
    );
}

/// Exercise the full native sub-agent tool surface with a real LLM provider:
///
/// - parent tools: spawn_sub_agents, wait, inspect_sub_agents,
///   send_sub_agents, stop_sub_agents
/// - child tools: update_parent_agent, ask_parent_agent
#[tokio::test]
async fn sub_agent_all_tools_real_llm() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();
    let leader = make_agent(
        "leader",
        &model,
        r#"You are a deterministic integration-test coordinator.

You MUST use native tool calls, not prose, to follow this exact protocol:
1. Call spawn_sub_agents with two sub-agents:
   - agent "communicator", slug "needs_input", prompt:
     "You are communicator. First call update_parent_agent with summary READY_FOR_SEND.
      Then call ask_parent_agent asking for the unlock token.
      After the parent replies, call update_parent_agent with summary RECEIVED_TOKEN and
      include the exact parent reply in details. Then finish with COMMUNICATOR_DONE."
     task description "Exercise child update and ask tools", goal "Ask parent for token",
     acceptance criteria ["Use update_parent_agent", "Use ask_parent_agent"]
   - agent "sleeper", slug "stop_target", prompt:
     "You are sleeper. Immediately call ask_parent_agent asking whether to continue.
      Do not finish unless the parent replies."
     task description "Remain waiting until stopped", goal "Wait for parent input",
     acceptance criteria ["Use ask_parent_agent and wait"]
2. Call wait for up to 20 seconds.
3. Call inspect_sub_agents for ["needs_input", "stop_target"] with include_transcript true.
4. Call send_sub_agents with one message to slug "needs_input": "BLUE-ORCHID".
5. Call wait for up to 20 seconds.
6. Call inspect_sub_agents for ["needs_input"] with include_transcript true.
7. Call stop_sub_agents for ["stop_target"] with reason "integration test complete".
8. Final answer exactly: SUB_AGENT_ALL_TOOLS_OK

Do not skip any step."#,
    );

    let manifest = Manifest {
        agents: vec![leader],
        models: vec![model],
        projects: vec![make_project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("leader")
        .await
        .unwrap()
        .with_config(AgentConfig {
            max_turns: 30,
            parallel_tools: false,
            ..Default::default()
        })
        .build()
        .await
        .unwrap();

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(240),
        runner.chat("Run the native sub-agent tool integration protocol now."),
    )
    .await
    .expect("sub-agent all-tools scenario timed out")
    .expect("chat should succeed");

    println!("--- Sub-Agent All Tools Test ---");
    println!("Response: {}", output.text);
    println!("Tool calls: {}", output.tool_calls);
    println!("Messages:\n{}", transcript_text(&output));

    let tool_names = tool_call_names(&output);
    for expected in [
        "spawn_sub_agents",
        "wait",
        "inspect_sub_agents",
        "send_sub_agents",
        "stop_sub_agents",
    ] {
        assert!(
            tool_names.iter().any(|name| name == expected),
            "expected parent tool {expected}; saw {tool_names:?}"
        );
    }

    let transcript = transcript_text(&output);
    for expected in [
        "needs_input",
        "stop_target",
        "READY_FOR_SEND",
        "BLUE-ORCHID",
        "RECEIVED_TOKEN",
        "update_parent_agent",
        "ask_parent_agent",
    ] {
        assert!(
            transcript.contains(expected),
            "expected transcript to contain {expected}; transcript:\n{transcript}"
        );
    }

    let payloads = tool_result_payloads(&output);
    assert!(
        delivered_to_slug(&payloads, "needs_input"),
        "send_sub_agents should deliver to needs_input; payloads: {payloads:?}"
    );
    assert!(
        stopped_slug(&payloads, "stop_target"),
        "stop_sub_agents should stop stop_target; payloads: {payloads:?}"
    );

    assert!(
        output.text.contains("SUB_AGENT_ALL_TOOLS_OK"),
        "final response should confirm protocol completion: {}",
        output.text
    );
}

/// Verify the canonical runtime no longer exposes legacy delegate_to on the
/// stored runner instance.
#[tokio::test]
async fn delegate_to_is_not_model_facing() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            eprintln!("OPENROUTER_API_KEY not set — skipping");
            return;
        }
    };

    let model = make_model();

    let manifest = Manifest {
        agents: vec![
            make_agent("alpha", &model, "You are alpha."),
            make_agent("beta", &model, "You are beta."),
            make_agent("gamma", &model, "You are gamma."),
        ],
        models: vec![model],
        projects: vec![make_project()],
        ..Default::default()
    };

    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(OpenRouterFactory { api_key })
        .with_tool_factory(NoopToolFactory)
        .build()
        .await
        .unwrap();

    let runner = provider
        .agent("alpha")
        .await
        .unwrap()
        .build()
        .await
        .unwrap();
    let specs = runner.instance().tool_specs();
    assert!(specs.iter().all(|spec| spec.name != "delegate_to"));
}
