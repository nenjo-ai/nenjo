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

## Compaction

Both session snapshots are protected from history compaction. Consecutive turn
context messages and the following conversation message form one compaction
group so context cannot be separated from the input it describes. Runtime
context bodies are never payload-truncated.
