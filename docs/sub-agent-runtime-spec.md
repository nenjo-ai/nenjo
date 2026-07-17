# Native Sub-Agent Runtime Spec

## Purpose

Sub-agents are child runtime executions of normal Nenjo agents. They are not a
separate manifest type.

```text
Agent manifest = program definition
Parent agent run = main process
Sub-agent = child Tokio task running another agent manifest
```

The sub-agent runtime is scoped to one `AgentRunner` execution. It is not a
platform service, not a manifest MCP API, and not a council or routine
abstraction.

This spec replaces the legacy one-shot `delegate_to` tool with a native
thread/service-style runtime controlled by model-facing tools.

## Current Code Seams

The implementation should fit the existing runner rather than create a parallel
execution system.

Current relevant flow:

```text
AgentBuilder::build()
  -> creates AgentInstance
  -> AgentRunner::new(instance, ...)
       currently injects assigned ability tools
       currently injects delegate_to when delegation is enabled
  -> AgentRunner::execute_stream(run)
       builds prompts/messages
       spawns turn_loop::run(...)
```

Existing event plumbing:

```text
crates/nenjo/src/agents/runner/turn_loop.rs
  CURRENT_EVENTS_TX
  CURRENT_CHAT_HISTORY
  CURRENT_NESTED_TOKEN_USAGE
```

The sub-agent runtime should be created in `AgentRunner::execute_stream()` for
that execution only. Parent tools should be injected into the cloned execution
instance before calling `turn_loop::run(...)`.

## Tool Surface

Parent-facing tools:

```text
spawn_sub_agents
send_sub_agents
inspect_sub_agents
stop_sub_agents
wait
```

Child-facing tools:

```text
update_parent_agent
ask_parent_agent
```

Canonical removed/deprecated model-facing tools:

```text
delegate_to
await_sub_agents
poll_sub_agents
```

There is no `await_sub_agents` tool. The parent agent should not block on a
long join. Instead, it calls `wait`, which yields the parent run briefly while
children continue. `wait` returns a compact digest of queued sub-agent signals.

There is no model-facing `poll_sub_agents` tool. Signal polling is folded into
`wait` to reduce tool surface area and avoid micro-management.

## Parent Tool: spawn_sub_agents

Starts one or more child agent runs.

Input:

```yaml
agents:
  - agent: security_reviewer
    slug: security_review
    prompt: Act as a focused security review worker. Be concise and evidence-driven.
    task:
      title: Review the auth/session changes for security issues.
      instructions: Check for privilege escalation risk and cite evidence for every issue.
      labels:
        - security
      priority: high
    context:
      files:
        - crates/auth/src/session.rs
    result_format:
      fields:
        - name: summary
          type: string
          description: One-paragraph result summary.
        - name: issues
          type: list
          description: Issues found. Each item should include title, severity, evidence, and recommendation.
        - name: confidence
          type: string
          description: Low, medium, or high confidence in the review.
```

Fields:

```text
agent          required, ephemeral child agent name
slug           optional, caller-facing child handle
prompt         optional child identity/guidance supplied by the parent
task           required platform-shaped task object; title is required and instructions, slug,
               labels, status, and priority are optional
context        optional simple structured JSON metadata
result_format  optional lightweight final result contract
```

The child prompt is built only from `prompt`, `task`, `context`, and
`result_format`. The child is an ephemeral agent created at spawn time; it does
not need a persisted agent manifest.

SDK hosts can create the same kind of minimal runtime agent manifest with:

```rust
use nenjo::manifest::AgentManifest;

let agent = AgentManifest::builder()
    .with_name("security_reviewer")
    .with_system_prompt("Act as a focused security review worker.")
    .with_developer_prompt("Be concise and evidence-driven.")
    .with_task_template("Task: {{ task.title }}\n\n{{ task.description }}")
    .build()?;
```

The `result_format` is intentionally not full JSON Schema. It is a compact
contract the parent can author easily. The runtime turns it into child prompt
instructions and may validate only field presence and rough type.

Supported `result_format.fields[].type` values:

```text
string
number
boolean
list
object
```

If `type` is omitted, it defaults to `string`.

Slug rules:

```text
- model-facing identity only
- scoped to one parent run
- lowercase letters, numbers, underscores, and hyphens
- max length 64
- if omitted, derived from agent name
- collisions are deduped: security_reviewer, security_reviewer_2
```

Output:

```yaml
sub_agents:
  - slug: security_review
    agent: security_reviewer
    status: running
```

No UUIDs are exposed to the model. Internal run IDs may still be UUIDs.

## Parent Tool: send_sub_agents

Sends messages to one or more running child agents.

Input:

```yaml
messages:
  - slug: security_review
    message: Also check the migration for privilege escalation risk.
```

Output:

```yaml
sent:
  - slug: security_review
    status: delivered
```

Delivery semantics:

```text
- cooperative
- message is queued for the child
- child receives it at the next runtime checkpoint or wait point
- if the child is complete, return not_delivered with a reason
```

If the child is waiting after `ask_parent_agent`, a delivered parent message
should move it out of `WaitingForInput` if no other blocking asks remain.

## Parent Tool: inspect_sub_agents

Reads bounded child state and transcript deltas for correction/debugging.

Input:

```yaml
sub_agents:
  - security_review
include_transcript: true
limit: 30
```

Output:

```yaml
sub_agents:
  - slug: security_review
    agent: security_reviewer
    status: running
    latest_signal: Reviewing tests
    transcript_delta:
      - kind: assistant_message
        summary: I am checking token refresh behavior.
      - kind: tool_call
        tool: read_file
        summary: crates/auth/src/session.rs
```

Semantics:

```text
- advances an inspect cursor
- never dumps the full transcript by default
- bounded by requested limit and runtime caps
- separate from wait signal queues
- used for correction, debugging, and recovery
```

## Parent Tool: stop_sub_agents

Gracefully cancels one or more child runs.

Input:

```yaml
sub_agents:
  - security_review
reason: No longer needed.
```

Output:

```yaml
stopped:
  - slug: security_review
    status: stopped
```

Semantics:

```text
- graceful cancellation first
- runtime may abort on parent drop or timeout
- stopped child emits a runtime-generated stopped signal
```

## Parent Tool: wait

Yields the parent agent briefly while child agents continue running.

Input:

```yaml
seconds: 10
reason: Let the reviewers make progress.
```

Fields:

```text
seconds optional, default 10
min 1
max 30
reason optional
```

Wake conditions:

```text
- timeout elapsed
- child calls ask_parent_agent
- child completes
- user message or interruption wakes the parent run
- cancellation or stop
```

Normal progress updates do not wake the parent immediately unless the timeout
has elapsed. They are batched to reduce tool calls and context churn.

Output:

```yaml
elapsed_seconds: 8
woken_by: sub_agent_result
updates:
  - slug: security_review
    events:
      - kind: progress
        summary: Read auth/session code and migration.
      - kind: progress
        summary: Found one auth issue and started validating impact.
      - kind: completed
        summary: Security review complete.
  - slug: test_review
    events:
      - kind: progress
        summary: Identified missing integration tests.
```

Semantics:

```text
- drains queued curated signal events
- returns compact digest as the tool result
- caps events per sub-agent
- collapses repeated progress where possible
- always includes needs_input, completed, failed, and stopped events
```

The `wait` tool is the parent event-loop yield. It is not specific to one child,
but its output includes sub-agent signals.

## Child Tool: update_parent_agent

Allows a sub-agent to emit progress to the parent.

Input:

```yaml
summary: Read auth/session modules and found one issue to verify.
details: Optional bounded detail.
```

Output:

```yaml
queued: true
```

Semantics:

```text
- writes a Progress signal to the parent signal queue
- does not wake parent immediately for normal progress
- should be used sparingly at meaningful milestones
```

There is no domain-specific signal type such as `finding` and no generic
`output` kind. Agent-specific findings or outputs should be expressed as
progress text or in the child final answer.

## Child Tool: ask_parent_agent

Allows a sub-agent to request parent input.

Input:

```yaml
question: Should migration risk be included in scope?
context: I found related auth schema changes.
```

Output:

```yaml
queued: true
parent_wake_requested: true
```

Semantics:

```text
- writes a NeedsInput signal
- wakes parent wait early
- child status becomes WaitingForInput until parent sends input or policy allows continuing
```

## Internal Runtime Types

Suggested module layout:

```text
crates/nenjo/src/agents/sub_agents/
  mod.rs
  runtime.rs
  slug.rs
  events.rs
  tools.rs
  error.rs
```

Core shape:

```rust
pub(crate) struct SubAgentRuntime<P: ProviderRuntime> {
    provider: P,
    parent_agent_id: Uuid,
    runs: DashMap<SubAgentSlug, SubAgentRun>,
    limits: SubAgentLimits,
}

pub(crate) struct SubAgentRun {
    run_id: Uuid,
    slug: SubAgentSlug,
    agent_name: String,
    status: SubAgentStatus,
    join: JoinHandle<Result<TurnOutput>>,
    signal_queue: SignalQueue,
    transcript_buffer: TranscriptBuffer,
    inbox: ChildInbox,
}
```

Use typed status:

```rust
enum SubAgentStatus {
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Stopped,
}
```

Use typed signals:

```rust
enum SubAgentSignal {
    Started {
        task_summary: String,
    },
    Progress {
        summary: String,
        details: Option<String>,
    },
    NeedsInput {
        question: String,
        context: Option<String>,
    },
    Completed {
        summary: String,
        structured_result: Option<serde_json::Value>,
        result_format_valid: Option<bool>,
    },
    Failed {
        error: String,
    },
    Stopped {
        reason: Option<String>,
    },
}
```

Transcript events are separate and bounded:

```rust
enum SubAgentTranscriptEvent {
    Input {
        summary: String,
    },
    AssistantMessage {
        summary: String,
    },
    ToolCall {
        tool: String,
        summary: String,
    },
    ToolResult {
        tool: String,
        success: bool,
        summary: String,
    },
    Error {
        summary: String,
    },
}
```

Lightweight result contract:

```rust
struct ResultFormat {
    fields: Vec<ResultField>,
}

struct ResultField {
    name: ResultFieldName,
    field_type: ResultFieldType,
    description: String,
}

enum ResultFieldType {
    String,
    Number,
    Boolean,
    List,
    Object,
}
```

`ResultFieldName` should be a parsed newtype, not a bare `String`, so the
runtime can guarantee field names are valid before storing them.

## Runner Integration

Implementation shape inside `AgentRunner::execute_stream()`:

```rust
let mut inst = (*self.instance).clone();

if let Some(provider) = inst.runtime.provider_runtime.clone() {
    let runtime = SubAgentRuntime::new(provider, inst.agent_id(), limits, events_tx.clone());
    inst.runtime.tools.extend(parent_sub_agent_tools(runtime.handle()));
}

let inst = Arc::new(inst);
turn_loop::run(&inst, messages, Some(events_tx), Some(loop_pause)).await
```

When spawning a child:

```text
1. parse and reserve the slug
2. create an ephemeral child `AgentManifest` from name and prompt
3. enforce max depth
4. build child runner through `provider.new_agent()` in `Child` execution mode
5. inject child tools: update_parent_agent, ask_parent_agent
6. execute the child through its configured task template using structured
   `task`, `context`, and `result_format` fields
7. start child execution as a Tokio task
8. bridge child TurnEvent values into the child transcript buffer and parent trace stream
9. store final output and emit Completed or Failed
```

The child uses the parent run's model unless the runtime host supplies a
different model policy. Persisted agent manifests are not required for child
identity, prompt context, scope, ability, memory, or platform tool expansion.

Execution modes:

```text
Parent -> normal agent execution; can receive provider/platform tools, memory
          tools, abilities, and parent sub-agent management tools.
Child  -> isolated sub-agent worker; receives only parent-authored
          prompt/task/context and child communication tools.
```

## Lifecycle

Ownership:

```text
parent run owns all child runs
```

Cleanup:

```text
parent completes -> stop/abort live children
runtime drop -> abort remaining tasks
completed children retained only for current parent execution
bounded queues prevent memory growth
```

Cancellation:

```text
stop_sub_agents -> graceful stop
parent abort/interruption -> runtime drop -> cancel tokens + abort child supervisors
ExecutionHandle drop -> abort underlying turn-loop task
drop/timeout -> abort as final cleanup
```

The runtime should behave like a small supervised Tokio service: child tasks are
registered, named by slug, cancellable, and cleaned up when the parent scope
ends.

Child tools and supervisor tasks must not keep strong references that prevent
the parent-owned runtime from dropping. Child communication handles should use
weak runtime/run references so parent interruption can cancel live children
deterministically.

## Observer Events

`wait` remains the only model-facing signal drain. Separately, the runtime emits
observer-only `TurnEvent::SubAgentEvent` values for UI/session/runtime consumers:

```text
slug           model-facing child handle
agent_name     target execution agent name
kind           started | progress | needs_input | completed | failed | stopped
summary        compact human-readable summary
model_visible  false for observer events emitted outside wait
```

Worker/platform bridges should preserve these as a distinct sub-agent envelope
instead of translating them into parent tool calls or legacy delegation events.

Child transcript snippets are also emitted as parent-owned
`TurnEvent::SubAgentTranscript` values. They are persisted as trace evidence on
the parent session, keyed by child slug and target agent name. They do not create
child sessions and they do not become transcript replay history for the parent
model.

## Token And Event Efficiency

Rules:

```text
- wait returns compact signal digest
- progress is collapsed when repeated or too frequent
- needs_input, completed, failed, and stopped are always surfaced
- inspect is bounded and cursor-based
- durable child transcript traces are bounded snippets, not full transcript dumps
```

The parent model should not be woken for every progress update. The child can
emit progress, and the parent sees batched updates at the next `wait` wake.

## Security

Initial policy:

```text
- child cannot exceed parent runtime security
- child does not receive provider/platform tools
- child does not receive memory tools or memory prompt vars
- child does not receive abilities, domains, scopes, MCP metadata, routines, or
  available-agent context
- child emission tools are only available inside spawned child runs
- parent management tools are only available to normal parent runs
- model-facing APIs use slugs, not internal IDs
```

## Claude/Nenpm Mapping

This runtime supports marketplace adapters cleanly:

```text
Claude skills    -> nenjo.skill.v1
Claude agents    -> nenjo.agent.v1
Claude commands  -> nenjo.domain.v1
Claude MCP       -> nenjo.mcp_server.v1
```

Imported Claude agents are normal Nenjo agents. They can be started directly or
spawned as sub-agents through the native runtime.

## Test Plan

Add tests for:

```text
slug validation
slug auto-generation and collision handling
spawn one child
spawn multiple children
wait drains signal digest
wait wakes early on ask_parent_agent
wait wakes early on child completion
normal progress does not wake immediately
inspect advances transcript cursor
send_sub_agents queues parent input
stop_sub_agents transitions status
parent drop aborts children
parent abort/interruption aborts children
sub-agent observer events are streamed with a distinct envelope
sub-agent transcript snippets are persisted as parent-session traces
child update_parent_agent queues progress
child ask_parent_agent queues needs_input and wakes parent
result_format prompt instructions are included for child runs
result_format parses valid final JSON
result_format reports invalid or missing fields without failing the run
model-facing outputs contain slugs, not UUIDs
delegate_to is no longer injected in canonical mode
```
