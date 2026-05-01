# Routines — Structured Workflow Graphs

**Path:** `nenjo.guide.routines`  
**Kind:** Guide  
**Status:** stable

## Purpose
Routines are directed workflow graphs that orchestrate agents, gates, councils, and terminal steps. They provide deterministic, auditable, and maintainable execution paths instead of relying on a single unstructured agent pass. Routines are the primary way to express complex, multi-step business logic in Nenjo.

## Core Model

A routine is a directed graph consisting of:

- **Steps** — Individual units of work (agent, gate, council, etc.)
- **Edges** — Connections that define execution flow and dependencies
- **Trigger** — What causes the routine to start (`task` or `cron`)
- **Optional metadata** — Custom configuration per routine

Routines are **acyclic by default**, with the explicit exception of controlled review loops (a gate can send work back for refinement before final success or failure).

## Triggers

| Trigger | Description                              | Common Use Case                     |
|---------|------------------------------------------|-------------------------------------|
| `task`  | Started when a project task is ready     | Normal project work execution       |
| `cron`  | Started on a schedule                    | Periodic maintenance, monitoring    |

## Step Types

| Step Type      | Description                                                                 | Typical Use |
|----------------|-----------------------------------------------------------------------------|-------------|
| `agent`        | Executes a single agent with full task or context                           | Core work   |
| `gate`         | Evaluates evidence and branches (pass/fail, approve/reject)                 | Quality control, validation |
| `council`      | Runs a structured multi-agent collaboration                                 | Review, synthesis, voting |
| `cron`         | Internal scheduled step (rare)                                              | Recurring sub-work |
| `terminal`     | Successful end of the routine                                               | Success path |
| `terminal_fail`| Explicit failure path                                                       | Error handling |

## Failure & Verdict Handling

**Important runtime behavior:**

- Every **agent step** and **gate step** is automatically exposed to a `pass_verdict` tool.
- This tool allows the agent to explicitly return a structured pass/fail verdict along with a reason.

**Failure semantics differ by step type:**

- **Agent Step Failure**: If an agent step fails (either by returning a failure verdict or by throwing an error), the **entire routine fails** immediately. There is no `on_fail` edge for agent steps.
- **Gate Step Failure**: If a gate step fails (returns a failure verdict), execution follows the explicitly defined `on_fail` edge. This allows for graceful degradation, retry logic, or routing to a failure path.

This distinction ensures that critical work steps are treated as atomic (failure = routine failure), while gates can implement sophisticated error handling and branching.

## Common Workflow Patterns

Routines are typically built using well-known patterns:

- **Gated Pipeline** — Linear flow with validation gates at key stages
- **Fan Out** — Parallel independent work followed by a join/review
- **Linear Pipeline** — Simple sequential execution
- **Review Pipeline** — Generation → Review → Approval/Rejection loop

## Graph Rules

- Routines are **acyclic** except for deliberate review loops
- Circular dependencies between steps are rejected
- A gate can create a controlled loop back to an earlier step for rework
- Every routine must have at least one reachable `terminal` or `terminal_fail` step

## Key Relationships (Canonical)

- `part_of` → `nenjo.domain.nenjo_platform`
- `references` → Agents (as step executors)
- `references` → Councils (as collaboration steps)
- `references` → Workflow Patterns (Gated Pipeline, Fan Out, Review Pipeline, Linear Pipeline)
- `defines` → its own execution topology and step ordering

## Runtime Behavior

When a routine is triggered:

1. The trigger (task or cron) provides initial context
2. Steps execute according to the graph edges
3. Gates evaluate output and decide the next path
4. Councils run multi-agent collaboration when reached
5. The routine ends at a `terminal` (success) or `terminal_fail` (failure)

Routines provide strong observability — every step transition, gate decision, and council output is recorded.

## Agent Guidance

**Reference this block when:**
- Designing multi-step or multi-agent workflows
- Deciding between a single agent vs a full routine
- Explaining workflow structure to users or stakeholders
- Troubleshooting execution paths or branching logic

## Pitfalls to Avoid

- Creating overly complex graphs with too many steps (prefer composition via sub-routines when possible)
- Using gates without clear acceptance criteria
- Forgetting to define both success and failure terminal paths
- Creating accidental cycles instead of controlled review loops
- Mixing too many council steps without clear delegation strategy

## Best Practices

- Start simple (Linear Pipeline or Gated Pipeline)
- Use councils only when multiple perspectives or synthesis are genuinely needed
- Keep individual steps focused — move complex logic into abilities or sub-agents
- Version routines carefully when they are used in production