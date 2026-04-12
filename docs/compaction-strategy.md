# Compaction Strategy

## Goal

The turn loop keeps chat history within the model context budget while preserving the most useful recent state for the next LLM call.

The implementation lives in `crates/nenjo/src/agents/runner/turn_loop.rs`.

## Chronological Strategy

Compaction runs at the start of each turn-loop iteration after deferred tool-argument truncation and before the next provider chat call.

### Pre-step: Deferred Tool Argument Truncation

Before compaction starts, the turn loop may truncate old assistant tool-call arguments when the conversation is already near the model limit.

This step:

- activates at the configured compaction trigger percent of the context budget
- defaults to 60% via `AgentConfig.context_compaction_trigger_percent`
- preserves recent tool-call arguments intact
- reduces the chance that the model sees and imitates truncation markers in fresh tool calls

### Phase 1: Truncate Old Tool Results

If the message buffer is still over budget, the turn loop first truncates large old `tool` messages outside the protected recent tail.

This phase:

- preserves the tool result message structure
- keeps `tool_call_id` intact
- rewrites only the bulky result content
- exits early if the history is back under budget

### Phase 2: Compact Old Assistant Tool Calls

If phase 1 is insufficient, the turn loop rewrites older assistant tool-call JSON messages so their `arguments` fields become `{}`.

This phase:

- keeps the assistant tool-call message parseable
- preserves tool names and call IDs
- keeps provider tool-call reconstruction working
- exits early if the history is back under budget

### Phase 2.5: Truncate Large Plain-Text Assistant Messages

If the buffer is still too large, the turn loop truncates older plain-text assistant messages, such as artifact-heavy outputs from domain sessions.

This phase:

- only targets assistant messages that are not tool-call JSON
- preserves recent assistant messages
- exits early if the history is back under budget

### Phase 3: Summarize Old Completed Turn Groups

If deterministic shrinking is still insufficient, the turn loop attempts semantic summarization.

This phase:

- selects the oldest eligible span outside the protected recent tail
- operates on complete message groups, not arbitrary individual messages
- never splits an assistant tool-call message from its following tool-result messages
- skips existing synthetic summary messages
- replaces the selected span with one synthetic assistant summary message beginning with `[history summary]`

The summarizer is an internal provider call with no tools enabled. It is instructed to preserve:

- user goals and requests
- decisions and conclusions
- important tool calls and outputs
- files, paths, branches, artifacts, and other durable references
- unresolved work and constraints

The summary is accepted only if:

- the provider returns plain text rather than tool calls
- the result is non-empty
- the result fits within the summary character cap
- the replacement materially reduces the token estimate for the summarized span

If summarization succeeds, the turn loop emits `TurnEvent::MessageCompacted`, which the harness bridges to `StreamEvent::MessageCompacted`.

### Phase 4: Drop Oldest Messages as Fallback

If the history is still over budget after summarization, the turn loop falls back to destructive dropping.

This phase:

- removes the oldest non-system messages
- keeps at least the system message plus a recent tail
- removes trailing tool-result messages when their preceding assistant tool-call message is dropped

This is the final fallback when truncation and summarization are not enough.

## Protected Invariants

Across all phases, the implementation preserves these invariants:

- the system message at index `0` is never removed by compaction
- the recent protected tail is preserved as much as possible
- assistant tool-call messages and their following tool results remain grouped
- synthetic history summaries are represented as plain assistant text
- compaction failure never fails the user-facing turn

## Persistence

The compacted message list becomes `TurnOutput.messages`.

The harness persists that array to the session content store, so summarized history is durable and will be visible on future resumed turns.

This means the persisted transcript may contain synthetic summary messages instead of the fully expanded older span.

## Eventing

When phase 3 succeeds, the turn loop emits:

- `TurnEvent::MessageCompacted { messages_before, messages_after }`

The harness converts that to:

- `StreamEvent::MessageCompacted { messages_before, messages_after }`

The current event does not distinguish between compaction strategies. It is specifically emitted for summary replacement in the current implementation.

## Failure Behavior

Summarization is best-effort only.

If the provider call fails, returns tool calls, returns empty text, exceeds the summary cap, or does not reduce the summarized span enough, the turn loop discards the summary attempt and continues into phase 4 fallback.

Compaction remains an optimization, not a correctness dependency.

## Current Summary Format

Synthetic summary messages are stored as plain assistant text and begin with this exact marker:

```text
[history summary]
```

This marker is used so later compaction passes can recognize existing synthetic summaries and avoid re-summarizing them immediately.

## Test Coverage

The current test coverage in `turn_loop.rs` verifies:

- deterministic phases still behave as before
- phase 3 inserts a synthetic summary marker
- phase 3 lowers token pressure
- phase 3 emits a compaction event
- phase 3 candidate selection keeps assistant tool-call groups intact
