# Async Operation Runtime Plan

## Purpose

Nenjo needs a shared async operation model for long-running tool work. The
immediate goal is to redesign `use_ability` so it behaves like native
sub-agent tooling: start work, return a handle, allow progress updates, allow
questions, support polling/inspection, and support cancellation.

The design should not be ability-specific. It should support sub-agents,
abilities, shell commands, and future long-running tool types while still
letting each type expose a model-facing tool surface that fits its domain.

## Current State

Sub-agents already have the desired interaction model:

```text
spawn_sub_agents
send_sub_agents
inspect_sub_agents
stop_sub_agents
wait
```

Child sub-agents can also report back to the parent:

```text
update_parent_agent
ask_parent_agent
```

`use_ability` is currently synchronous. It directly runs a nested turn loop and
returns only final output to the caller. Shell execution is also synchronous,
with timeout and output truncation implemented inside the concrete shell tool.

The existing dashboard displays live work through stream events such as
`ToolCalls`, `ToolCompleted`, `AbilityActivated`, and `AbilityCompleted`.
Frontend live state is currently a flat `activeTools` array with optional
children, keyed mostly by tool name. That is not sufficient for multiple
concurrent abilities, sub-agents, or shell operations with the same display
name.

## Design Direction

Add a shared async operation runtime in `crates/nenjo`, then make sub-agents,
abilities, and later shell operations adapters over it.

The shared runtime owns lifecycle mechanics:

- stable operation ids
- status transitions
- bounded signal queues
- bounded transcript/output queues
- parent-to-operation inbox
- cancellation
- operation joins
- wait/notify behavior
- cleanup on drop

Concrete tool families own domain-specific behavior:

- how an operation is started
- what tools are exposed to the model
- what child tools are available inside the operation
- how payloads are summarized
- what security/tool scopes are inherited

## Core Runtime Types

Add a new internal module, likely:

```text
crates/nenjo/src/agents/async_ops/
```

Proposed core types:

```rust
pub(crate) struct AsyncOpId(...);

pub(crate) enum AsyncOpKind {
    Ability,
    SubAgent,
    Shell,
}

pub(crate) enum AsyncOpStatus {
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Stopped,
}

pub(crate) enum AsyncOpSignal {
    Started { summary: String },
    Progress { summary: String, details: Option<String> },
    NeedsInput { question: String, context: Option<String> },
    Completed { summary: String, output: Option<serde_json::Value> },
    Failed { error: String },
    Stopped { reason: Option<String> },
}
```

The runtime should expose operations similar to the sub-agent runtime today:

```text
start
send_input
inspect
stop
wait
drain_signals
push_signal
push_transcript
```

Use typed enums instead of a `kind` string plus optional fields internally.
Convert to wire/display strings only at the event boundary.

## Tool Surface

Keep model-facing tools tailored by domain.

Sub-agent tools remain:

```text
spawn_sub_agents
send_sub_agents
inspect_sub_agents
stop_sub_agents
wait
```

Ability tools should become:

```text
list_assigned_abilities
use_ability
inspect_abilities
send_abilities
stop_abilities
wait
```

`use_ability` should return immediately with an operation handle:

```json
{
  "ability": "research",
  "operation_id": "ability_research_1",
  "status": "running",
  "control_tools": {
    "inspect": "inspect_abilities",
    "send_input": "send_abilities",
    "stop": "stop_abilities",
    "wait": "wait"
  }
}
```

Ability sub-executions should get the same parent communication tools used by
sub-agents:

```text
update_parent_agent
ask_parent_agent
```

The `wait` tool should become a generic operation wait while preserving the
current sub-agent behavior during migration.

## Event Model

Add canonical runtime events:

```rust
TurnEvent::AsyncOperationEvent {
    operation_id: String,
    kind: String,
    label: String,
    parent_operation_id: Option<String>,
    parent_tool_name: Option<String>,
    status: String,
    signal: String,
    summary: Option<String>,
    model_visible: bool,
}

TurnEvent::AsyncOperationTranscript {
    operation_id: String,
    kind: String,
    label: String,
    event: AsyncOperationTranscriptEvent,
}
```

Then add matching `nenjo_events::StreamEvent` variants for frontend delivery:

```ts
type AsyncOperationEvent = {
  operation_id: string;
  kind: "ability" | "sub_agent" | "shell";
  label: string;
  parent_operation_id?: string;
  parent_tool_name?: string;
  status: "running" | "waiting_for_input" | "completed" | "failed" | "stopped";
  signal: "started" | "progress" | "needs_input" | "completed" | "failed" | "stopped";
  summary?: string;
  payload?: Record<string, unknown>;
  encrypted_payload?: EncryptedPayload;
};

type AsyncOperationTranscript = {
  operation_id: string;
  kind: "ability" | "sub_agent" | "shell";
  label: string;
  event: {
    kind:
      | "input"
      | "assistant_message"
      | "tool_call"
      | "tool_result"
      | "output_chunk"
      | "error";
    summary: string;
    tool?: string;
    success?: boolean;
  };
};
```

`operation_id` is required. Tool names are not stable enough when multiple
operations of the same type are running.

`parent_operation_id` is required for nested display. This lets the dashboard
render activity such as:

```text
UseAbility(research)
  SearchWeb(...)
  Shell(...)
  AskParentAgent(...)
```

or:

```text
SubAgent(code-reviewer)
  ReadFile(...)
  UpdateParentAgent(...)
```

During migration, keep old `AbilityActivated`, `AbilityCompleted`,
`ToolCalls`, and `ToolCompleted` events available as compatibility shims.
The new async operation event should become the canonical frontend contract.

## Dashboard Plan

The dashboard should normalize async operation events into an operation map
instead of relying on a flat tool array as source of truth.

Proposed live state:

```ts
type LiveOperation = {
  id: string;
  kind: "ability" | "sub_agent" | "shell";
  label: string;
  status: "running" | "waiting_for_input" | "completed" | "failed" | "stopped";
  signal?: string;
  summary?: string;
  parentId?: string;
  children: string[];
  transcript: OperationTranscriptEvent[];
  updatedAt: number;
};
```

The current `ToolEntry[]` rendering can be derived from this map initially.
That keeps the existing `TypingIndicator` mostly intact while fixing the data
model for concurrent operations.

Display rules:

- running operations show active progress
- `waiting_for_input` uses an action-needed visual state
- completed operations show done state
- failed/stopped operations show failure or muted stopped state
- progress signals update or append the operation summary
- transcript events become nested children or expandable details

Questions from async operations should be visually distinct from normal
progress. A `needs_input` signal means the parent agent may need to answer with
`send_abilities`, `send_sub_agents`, or a future generic send-input tool.

## Migration Phases

### Phase 1: Add Generic Async Operation Runtime

Create the runtime module and lifecycle tests. Do not change model-facing tool
behavior yet.

Test:

- start
- progress
- needs input
- resume after input
- complete
- fail
- stop
- bounded signal queue behavior
- wait wake behavior

### Phase 2: Move Sub-Agents Onto The Shared Runtime

Refactor the current sub-agent runtime to use the async operation manager
internally while preserving the existing tool surface and output shape.

This phase should be behavior-preserving. If existing sub-agent tests fail, the
abstraction is wrong or incomplete.

### Phase 3: Add Async Operation Events

Add `TurnEvent` and `StreamEvent` variants for async operation events and
transcripts.

Start by emitting them alongside current ability/sub-agent events. Avoid
removing compatibility events until the dashboard consumes the new shape.

### Phase 4: Update Dashboard Event Handling

Add reducer support for `AsyncOperationEvent` and
`AsyncOperationTranscript`.

Derive the existing live tool display from the new operation map. Keep fallback
handling for old events.

### Phase 5: Redesign `use_ability`

Change `use_ability` to start an async ability operation and return an
operation handle immediately.

Preserve current ability scoping rules:

- ability platform scopes
- ability MCP server assignments
- ability runtime env names
- host tool inheritance policy
- no recursive abilities inside ability runs
- nested token usage accounting

Ability nested turn events should be bridged into async operation transcript
and progress events.

### Phase 6: Add Shell Async Support

Do not make shell async first. After abilities work, add shell support over the
same runtime.

Recommended initial shape:

- keep current `shell` synchronous for short commands
- add `start_shell`, or add `mode: "async"` to `shell`
- stream stdout/stderr chunks into operation transcript/output
- support inspect, wait, send input if interactive shells are supported, and
  stop
- preserve current security policy, env filtering, timeout, and output caps

Shell should validate that the shared runtime is not just a sub-agent-specific
abstraction with a generic name.

## Open Decisions

- Should `use_ability` be async-only, or temporarily support
  `await_completion: true` for compatibility?
- Should parent control tools remain tailored (`inspect_abilities`,
  `inspect_sub_agents`) or eventually become generic (`inspect_operations`)?
- Should operation state live only for the current agent run, or persist across
  turns/sessions through harness/platform storage?
- Should `wait` return all operation updates by default, or require filters by
  operation kind/id?

Recommended defaults:

- make `use_ability` async-only
- keep model-facing tools tailored
- keep operation state in-memory and scoped to the current run first
- evolve `wait` into a generic operation wait while preserving sub-agent output
  shape during migration
