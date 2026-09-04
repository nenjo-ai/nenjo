# Runtime Prompt Context

Nenjo separates authored instructions from runtime data so repeated model calls
retain the longest possible byte-identical prefix.

## Message order

Every model request is assembled in this order:

1. Compiled system prompt
2. Compiled developer prompt and the static runtime-context protocol, combined
   into the provider's single instruction message when necessary
3. Session control context
4. Session data context
5. Prior turn context and conversation messages, in chronological order
6. Current turn control context
7. Current turn data context
8. Current raw user input

`ConversationMessage::RuntimeContext` distinguishes session and turn snapshots
inside the runtime and independently records whether a message is control or
data. Providers with native developer-role support serialize control context as
developer and data context as user. Providers without that role serialize both
as user at their original chronological position. Dynamic context is never
promoted into an additional system prompt.

At an OpenAI-shaped adapter boundary, models without a native `developer` role
use a portable projection. After roles are mapped, adjacent `user` messages are
coalesced with the exact separator `\n\n`. This is a wire-only normalization:
the durable transcript retains every runtime-context boundary. Coalescing never
crosses an assistant message, function call, function-call output, tool result,
system message, or native developer message. Multipart Chat Completions content
keeps every original part in order and inserts only one separator text part.

Chat Completions and Responses use the same chronological transcript. Responses
input contains every instruction, runtime context, user/assistant message,
local function call, function output, and text artifact-analysis message in its
original order. Instructions are not lifted into a separate `instructions`
field. A compatible Responses fallback rejects unresolved multimodal artifact
references explicitly because that transport does not yet encode them.

Persisted runtime contexts from before the authority field existed deserialize
as data. This preserves their original user-role behavior instead of elevating
old mixed-content snapshots during replay.

The static runtime-context protocol tells the model to read applicable context
before acting, treat session context as epoch-scoped, and treat turn context as
applying only to the immediately following logical turn. Turn facts take
precedence over overlapping session facts. Control context is application
guidance; data context is reference material and never an instruction source.
User-authored text cannot gain context authority by copying the XML tags.

## Static instruction prefix

System prompts, developer prompts, domain guidance, and reusable context blocks
are static for an instruction epoch. They may reference:

- Declared package arguments resolved before execution
- Static context-block and knowledge-pack selectors

They may not reference `self`, `agent`, `global`, `chat`, `task`, `project`,
`routine`, `gate`, `git`, `memories`, `memory_profile`, `heartbeat`, or
`artifacts`. Package validation and runtime prompt compilation both reject
those selectors.

Package arguments are not a hidden runtime channel. Referenced arguments are
compile-time inputs to the static instruction prefix. Changing an argument
therefore creates a new instruction epoch and intentionally changes the cache
key.

## Session context

The session control snapshot currently contains:

- Executing agent slug, name, and description
- Project identity, working directory, and repository information

The session data snapshot currently contains:

- Project description, free-form context, and metadata
- The memory retrieval result frozen at the start of the session epoch

The memory profile is configuration for retrieval and memory-writing behavior;
it is not repeated in model context. A changed memory profile or project should
take effect in a new session epoch. Existing sessions continue to replay their
persisted snapshot byte-for-byte.

## Turn context

Each logical turn contains two canonical XML snapshots. Control contains:

- Execution kind
- Local time rounded to the minute, the organization's IANA timezone, and UTC
  time from one instant. Workers load the organization setting during bootstrap
  and replace their cached snapshot when `organization_settings.sync` arrives;
  missing legacy bootstrap state defaults to UTC.
- Task identity, status, priority, and slug
- Routine identity, active step routing, and workflow-authored step instructions
- Git/worktree fields
- Gate result identity and verdict

Data contains:

- Task title, instructions, and labels
- Routine descriptions, metadata, and handoffs
- Gate output and arbitrary result data

The current chat message or task instructions remain the raw user message after
the context block. Chat, task, and gate prompt templates do not exist.

## Persistence and retries

Session and turn contexts are transcript events. Replay canonicalizes each scope
to its latest control and data snapshot, places session context before
conversation history, and places a turn's context before its first model-visible
message even if write timing placed the events later in storage.

A retry excludes the failed logical turn's user, assistant, and tool messages,
then reuses every persisted context message for that turn. It does not generate
a new clock or memory snapshot. Context bodies are omitted from ordinary trace
previews and Claude-compatible hook transcripts.

## Execution cache epochs and tool catalogs

The model-visible local tool catalog is resolved exactly once when a turn loop
starts, after ability, delegation, async-control, and host tools are installed.
Canonical names are checked for uniqueness and sorted once. The resulting owned
snapshot is reused for request budgeting, diagnostics, every model turn, and
every provider retry in that execution. Provider adapters preserve its order
and reject any distinct canonical names that collide after provider-specific
name sanitization.

Executions with their own capability surface always advertise the same four
async controls for their full lifetime: `inspect`, `send_input`, `stop`, and
`wait`. Starting, completing, failing, stopping, or pruning an operation does
not alter those tool definitions. Each operation's `AsyncControls` metadata
still authorizes which controls apply; unsupported or unmatched model calls
return typed, machine-readable outcomes. Internal execution cancellation uses
a separate path that can stop hidden owned operations.

Tool availability is therefore an execution-start decision. A host capability
that becomes applicable midway through a run is first advertised in the next
execution. Configuration, capability, model-transport, or tool-catalog changes
start a new cache epoch rather than mutating the cache-visible prefix in place.

Prompt-cache byte stability is measured on provider-native projections:
Chat Completions compares serialized `messages` prefixes, Responses compares
serialized `input` prefixes, and both compare the complete serialized `tools`
array across turns and retries. The later HTTP body itself is not required to
have the earlier body as a literal byte prefix.

## Provider request debugging

Every provider adapter can emit a readable semantic view of the conversation
immediately before sending it. The view begins with a summary and then emits
one `model provider request part` event per message. Each part includes its
index, kind, artifact count, and full content. Part kinds distinguish `system`,
`developer`, `session_control`, `session_data`, `turn_control`, `turn_data`,
`user`, `assistant`, `assistant_tool_calls`, `tool_results`, and
`artifact_analysis`.

Enable only this split view with:

```sh
RUST_LOG=nenjo_models::provider_request::parts=debug nenjo run
```

Empty session or turn data parts are omitted rather than sending empty XML
wrappers. Control and data remain separate parts when both contain content.

The exact provider-native JSON wire shape is still available separately at
trace level, including role conversion, system-instruction placement, tool
definitions, inline artifacts, streaming flags, Responses API fallbacks, and
transport retries. Enable only that compact wire payload with:

```sh
RUST_LOG=nenjo_models::provider_request::wire=trace nenjo run
```

Enable both views with the parent target:

```sh
RUST_LOG=nenjo_models::provider_request=trace nenjo run
```

The exact payload event is named `model provider wire request`. Its
`request_json` field is the complete compact JSON body; `provider`, `model`, and
`attempt` identify the destination and individual send. Authentication headers
and credential query parameters are not logged.

This output can contain complete prompts, user input, tool results, and encoded
artifact data. Treat diagnostic logs as sensitive and disable or delete them
after diagnosis.

## Compaction

Both session snapshots are protected from history compaction. Consecutive turn
context messages and the following conversation message form one compaction
group so context cannot be separated from the input it describes. Runtime
context bodies are never payload-truncated.
