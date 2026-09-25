# Script Runtime Spec

Agent scripting capability (BOO-61): a QuickJS-backed `script` tool that lets
an agent run a short JavaScript program dispatching its available tools
directly, plus package-shipped script tools on the same engine.

## Surface

One model-facing tool, `script` (crates/worker/src/tools/script/):

```jsonc
{ "script": "...", "timeout_ms": 30000 } // timeout clamped to 120000
```

Every tool the agent could call directly is exposed as an async method on a
`ctx` namespace. Namespaces materialize from the agent's granted tools — there
is no scope enum for the model to select:

- `ctx.mcp.<toolName>({...args})` — MCP tools (`ToolOrigin::Mcp`)
- `ctx.runtime.<toolName>({...args})` — host/platform tools
- `ctx.mcp.list()` / `ctx.runtime.list()` — `{name, description, inputSchema}`
- `ctx.log(...)` — streamed progress notes
- `ctx.harness` — `{sessionId, project}` identity

## Engine

`rquickjs` (QuickJS 0.9, `full`+`futures`+`parallel`), one `AsyncRuntime` per
run. The sandbox is structural: the interpreter receives no bindings besides
the injected `ctx` — no fs, network, env, or clock.

Native/JS boundary: the host exposes one string-in/string-out async dispatch
function (`__nenjo_dispatch(name, argsJson) -> resultJson`) plus a static JSON
tool catalog; a small JS shim builds the namespaces from them. No native
closure captures `Ctx`, which keeps every native future `Send` and avoids
rquickjs lifetime hazards. Tool arguments/results cross as JSON; each
dispatch returns `{ok, content, error}` — a denial is data, identical to what
a direct model call would observe.

Limits: 128 MiB heap, 1 MiB stack, 1 MiB return-value cap, 1k log lines /
256 KiB log cap. Wall-clock default 30 s, max 120 s, enforced by a dedicated
deadline OS thread cancelling a `CancellationToken`; the token trips the
QuickJS interrupt handler, so CPU-bound loops die even on a current-thread
executor. Return value reaches the model as a `{result, log}` envelope
(pretty-printed JSON, machine-parseable even with log lines present).

## Async-operation lifecycle

Scripts run on a spawned task from call start. If still running after 2 s and
an async-operation runtime is in scope, the run is promoted to
`AsyncOperationKind::Script` with Inspect/Stop/Wait controls; the model
receives an `operation_started` receipt. The finisher task streams `ctx.log`
lines into the operation transcript (300 ms poll) and settles the operation
with the `{result, log}` envelope. Operation stop bridges the operation's
cancel token into the engine token. The engine task is scoped with the
current async-operation runtime, so a script dispatching `shell` still gets
shell's own promotion.

## Package script tools

`nenjo.script_tool.v1` manifests assigned via `AgentManifest.script_tools`
resolve against the `script_tools.json` cache and become first-class model
tools (`PackageScriptTool`). Entry `command.path` resolves relative to the
package `root_dir` (+ optional `root_path`); absolute paths and `..` are
rejected. The script receives the model's tool arguments as its `args`
parameter. `timeout_seconds` maps to the engine timeout, `read_only` to the
tool category. Package scripts share the agent's `ctx` surface and gain no
privileges the agent lacks; they never promote (synchronous to their
declared timeout) and are not dispatchable from the interactive script tool.

## Exclusions

`script_dispatchable` (script/mod.rs) permits only `Mcp | Host | Platform`
origins. `ToolOrigin::Harness` — ability brokers (`use_ability`,
`list_assigned_abilities`), delegation (`delegate_to`,
`list_delegatable_agents`), sub-agents (`spawn_sub_agents`), parent-channel
tools (`update_parent_agent`, `ask_parent_agent`), and async-operation
controls (`inspect`, `send_input`, `stop`, `wait`) — is stripped in
`ScriptTool::new`, `PackageScriptTool::from_manifest`, and the factory
partition. A script must not spawn or steer agents, nor manipulate the
operation lifecycle it may run under. Skills (`use_skill`,
`list_installed_skills`) are Harness-origin and therefore excluded too.

## Dispatch parity

Script tool calls re-enter `Tool::execute` exactly as direct model calls do:
same admission, `SecurityPolicy`, denial rules, and rate limits. Scripts gain
control flow, never privileges. Recursion is impossible: the `script` tool is
excluded from every namespace.

## Layout

- `script/engine.rs` — interpreter, sandbox, limits, converters
- `script/lifecycle.rs` — promotion receipt, log-streaming finisher, stop bridge
- `script/mod.rs` — `ScriptTool`, dispatchability predicate, result envelope
- `script/package.rs` — `PackageScriptTool`, slug resolver
- `tools/factory.rs` — namespace partition + package tool registration
