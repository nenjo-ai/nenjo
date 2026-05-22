use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ProviderRuntime;
use crate::tools::{Tool, ToolCategory, ToolResult};

use super::format::ResultFormat;
use super::runtime::{ChildRuntimeHandle, SpawnRequest, SubAgentHandle, SubAgentTask};
use super::slug::SubAgentSlug;

pub(crate) fn parent_tools<P: ProviderRuntime>(
    handle: SubAgentHandle<P>,
) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(SpawnSubAgentsTool {
            handle: handle.clone(),
        }),
        std::sync::Arc::new(SendSubAgentsTool {
            handle: handle.clone(),
        }),
        std::sync::Arc::new(InspectSubAgentsTool {
            handle: handle.clone(),
        }),
        std::sync::Arc::new(StopSubAgentsTool {
            handle: handle.clone(),
        }),
        std::sync::Arc::new(WaitTool { handle }),
    ]
}

pub(crate) fn child_tools<P: ProviderRuntime>(
    handle: ChildRuntimeHandle<P>,
) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(UpdateParentAgentTool {
            handle: handle.clone(),
        }),
        std::sync::Arc::new(AskParentAgentTool { handle }),
    ]
}

struct SpawnSubAgentsTool<P: ProviderRuntime> {
    handle: SubAgentHandle<P>,
}

struct SendSubAgentsTool<P: ProviderRuntime> {
    handle: SubAgentHandle<P>,
}

struct InspectSubAgentsTool<P: ProviderRuntime> {
    handle: SubAgentHandle<P>,
}

struct StopSubAgentsTool<P: ProviderRuntime> {
    handle: SubAgentHandle<P>,
}

struct WaitTool<P: ProviderRuntime> {
    handle: SubAgentHandle<P>,
}

struct UpdateParentAgentTool<P: ProviderRuntime> {
    handle: ChildRuntimeHandle<P>,
}

struct AskParentAgentTool<P: ProviderRuntime> {
    handle: ChildRuntimeHandle<P>,
}

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    #[serde(default)]
    agents: Vec<RawSpawnAgent>,
}

#[derive(Debug, Deserialize)]
struct RawSpawnAgent {
    agent: String,
    slug: Option<String>,
    prompt: Option<String>,
    task: RawSubAgentTask,
    context: Option<serde_json::Value>,
    result_format: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawSubAgentTask {
    description: String,
    goal: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
}

#[async_trait]
impl<P: ProviderRuntime> Tool for SpawnSubAgentsTool<P> {
    fn name(&self) -> &str {
        "spawn_sub_agents"
    }

    fn description(&self) -> &str {
        "Start one or more child agent runs. Each child is addressed by a slug scoped to this parent run."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agents": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": {"type": "string"},
                            "slug": {"type": "string"},
                            "prompt": {
                                "type": "string",
                                "description": "Optional identity, role, style, or operating guidance for this isolated worker."
                            },
                            "task": {
                                "type": "object",
                                "properties": {
                                    "description": {"type": "string"},
                                    "goal": {"type": "string"},
                                    "acceptance_criteria": {
                                        "type": "array",
                                        "items": {"type": "string"}
                                    }
                                },
                                "required": ["description", "goal"]
                            },
                            "context": {"type": "object"},
                            "result_format": {"type": "object"}
                        },
                        "required": ["agent", "task"]
                    }
                }
            },
            "required": ["agents"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: SpawnArgs = serde_json::from_value(args)?;
        if parsed.agents.is_empty() {
            return Ok(error("agents must contain at least one child request"));
        }
        let mut requests = Vec::with_capacity(parsed.agents.len());
        for agent in parsed.agents {
            if agent.agent.trim().is_empty()
                || agent.task.description.trim().is_empty()
                || agent.task.goal.trim().is_empty()
            {
                return Ok(error("agent, task.description, and task.goal are required"));
            }
            let slug = match agent.slug {
                Some(raw) => Some(SubAgentSlug::parse(raw).map_err(|err| anyhow::anyhow!(err))?),
                None => None,
            };
            let result_format = match agent.result_format {
                Some(value) => {
                    Some(ResultFormat::parse(&value).map_err(|err| anyhow::anyhow!(err))?)
                }
                None => None,
            };
            requests.push(SpawnRequest {
                agent_name: agent.agent,
                slug,
                prompt: agent.prompt,
                task: SubAgentTask {
                    description: agent.task.description,
                    goal: agent.task.goal,
                    acceptance_criteria: agent.task.acceptance_criteria,
                },
                context: agent.context,
                result_format,
            });
        }

        let results = self.handle.spawn_many(requests).await;
        let mut spawned = Vec::new();
        let mut failures = Vec::new();
        for result in results {
            match result {
                Ok(item) => spawned.push(item),
                Err(err) => failures.push(err.to_string()),
            }
        }
        if failures.is_empty() {
            Ok(ok(json!({ "sub_agents": spawned })))
        } else {
            Ok(ToolResult {
                success: false,
                output: json!({ "sub_agents": spawned, "errors": failures }).to_string(),
                error: Some("one or more sub-agents failed to spawn".into()),
            })
        }
    }
}

#[derive(Debug, Deserialize)]
struct SendArgs {
    #[serde(default)]
    messages: Vec<SendMessage>,
}

#[derive(Debug, Deserialize)]
struct SendMessage {
    slug: String,
    message: String,
}

#[async_trait]
impl<P: ProviderRuntime> Tool for SendSubAgentsTool<P> {
    fn name(&self) -> &str {
        "send_sub_agents"
    }

    fn description(&self) -> &str {
        "Send queued input to one or more running child agents."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "slug": {"type": "string"},
                            "message": {"type": "string"}
                        },
                        "required": ["slug", "message"]
                    }
                }
            },
            "required": ["messages"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: SendArgs = serde_json::from_value(args)?;
        let mut messages = Vec::with_capacity(parsed.messages.len());
        for message in parsed.messages {
            messages.push((SubAgentSlug::parse(message.slug)?, message.message));
        }
        Ok(ok(json!({ "sent": self.handle.send(messages).await })))
    }
}

#[derive(Debug, Deserialize)]
struct InspectArgs {
    #[serde(default)]
    sub_agents: Vec<String>,
    #[serde(default)]
    include_transcript: bool,
    #[serde(default = "default_inspect_limit")]
    limit: usize,
}

fn default_inspect_limit() -> usize {
    30
}

#[async_trait]
impl<P: ProviderRuntime> Tool for InspectSubAgentsTool<P> {
    fn name(&self) -> &str {
        "inspect_sub_agents"
    }

    fn description(&self) -> &str {
        "Inspect bounded child state and optional transcript deltas for correction or debugging."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "sub_agents": {"type": "array", "items": {"type": "string"}},
                "include_transcript": {"type": "boolean"},
                "limit": {"type": "number"}
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: InspectArgs = serde_json::from_value(args)?;
        let slugs = parsed
            .sub_agents
            .into_iter()
            .map(SubAgentSlug::parse)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ok(json!({
            "sub_agents": self.handle.inspect(slugs, parsed.include_transcript, parsed.limit).await
        })))
    }
}

#[derive(Debug, Deserialize)]
struct StopArgs {
    #[serde(default)]
    sub_agents: Vec<String>,
    reason: Option<String>,
}

#[async_trait]
impl<P: ProviderRuntime> Tool for StopSubAgentsTool<P> {
    fn name(&self) -> &str {
        "stop_sub_agents"
    }

    fn description(&self) -> &str {
        "Gracefully stop one or more child agent runs."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "sub_agents": {"type": "array", "items": {"type": "string"}},
                "reason": {"type": "string"}
            },
            "required": ["sub_agents"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: StopArgs = serde_json::from_value(args)?;
        let slugs = parsed
            .sub_agents
            .into_iter()
            .map(SubAgentSlug::parse)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ok(
            json!({ "stopped": self.handle.stop(slugs, parsed.reason).await }),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct WaitArgs {
    #[serde(default = "default_wait_seconds")]
    seconds: u64,
    reason: Option<String>,
}

fn default_wait_seconds() -> u64 {
    10
}

#[async_trait]
impl<P: ProviderRuntime> Tool for WaitTool<P> {
    fn name(&self) -> &str {
        "wait"
    }

    fn description(&self) -> &str {
        "Yield briefly while sub-agents continue running, then return queued sub-agent signals."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "seconds": {"type": "number", "minimum": 1, "maximum": 30},
                "reason": {"type": "string"}
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: WaitArgs = serde_json::from_value(args)?;
        let _reason = parsed.reason;
        Ok(ok(json!(self.handle.wait(parsed.seconds).await)))
    }
}

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    summary: String,
    details: Option<String>,
}

#[async_trait]
impl<P: ProviderRuntime> Tool for UpdateParentAgentTool<P> {
    fn name(&self) -> &str {
        "update_parent_agent"
    }

    fn description(&self) -> &str {
        "Send a compact progress update to the parent agent without waking it immediately."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "details": {"type": "string"}
            },
            "required": ["summary"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: UpdateArgs = serde_json::from_value(args)?;
        self.handle.progress(parsed.summary, parsed.details).await;
        Ok(ok(json!({ "queued": true })))
    }
}

#[derive(Debug, Deserialize)]
struct AskArgs {
    question: String,
    context: Option<String>,
}

#[async_trait]
impl<P: ProviderRuntime> Tool for AskParentAgentTool<P> {
    fn name(&self) -> &str {
        "ask_parent_agent"
    }

    fn description(&self) -> &str {
        "Ask the parent agent for input and wait until it sends a response."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {"type": "string"},
                "context": {"type": "string"}
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: AskArgs = serde_json::from_value(args)?;
        let reply = self.handle.ask(parsed.question, parsed.context).await;
        Ok(ok(json!({
            "queued": true,
            "parent_wake_requested": true,
            "parent_response": reply
        })))
    }
}

fn ok(value: serde_json::Value) -> ToolResult {
    ToolResult {
        success: true,
        output: value.to_string(),
        error: None,
    }
}

fn error(message: impl Into<String>) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(message.into()),
    }
}
