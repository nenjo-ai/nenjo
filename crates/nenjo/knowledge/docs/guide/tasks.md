# Tasks — Units of Project Work

## Purpose
Tasks are the atomic units of work within a Project. Each task carries a human-readable title, detailed description, acceptance criteria, priority, type, complexity, and can be assigned to either a single agent **or** a full routine — never both. Tasks support rich dependency graphs and drive the majority of execution in Nenjo.

## Core Concepts

- Tasks represent **discrete, trackable units of work**
- Every task has a clear **definition of done** via acceptance criteria
- Tasks are the primary trigger for Routines and agent execution
- Dependencies are resolved automatically using topological sorting

## Lifecycle & Statuses

| Status        | Description                                                                 | User Settable? | Notes |
|---------------|-----------------------------------------------------------------------------|----------------|-------|
| `open`        | Initial state after creation                                                | Yes            | Default |
| `backlog`     | Waiting on unmet dependencies                                               | No (auto)      | Set automatically by execution engine |
| `ready`       | All dependencies satisfied — eligible for execution                         | Yes            | Can be set manually |
| `assigned`    | Claimed by a worker (internal state)                                        | No             | Not user-settable |
| `in_progress` | Currently being executed                                                    | No             | Set by execution engine |
| `done`        | Successfully completed (`resolved_at` timestamp set)                        | No             | Terminal success |
| `failed`      | Execution failed (`closed_at` + `last_error` set)                           | No             | Terminal failure |

## Key Fields

- `title` — `VARCHAR(500)`, required. Human-readable name.
- `description` — `TEXT`, optional. Detailed explanation of the work.
- `acceptance_criteria` — `TEXT`, optional. Defines “done” and is used by gates for evaluation.
- `status` — Enum (see table above), defaults to `open`.
- `priority` — `low` | `medium` | `high` | `critical`, defaults to `medium`.
- `type` — `bug` | `feature` | `task` | `improvement`, defaults to `task`.
- `complexity` — `SMALLINT` (1–5), optional. Used for estimation and ordering.
- `tags` — `TEXT[]`, freeform labels for filtering.
- `required_tags` — `TEXT[]`, tags an executing agent **must** have.
- `slug` — `VARCHAR(255)`, auto-generated from title (must be unique per project).
- `order_index` — `INT`, secondary sort key within the same dependency level.
- `metadata` — `JSONB`, arbitrary key-value data.
- `assigned_agent_id` — `UUID`, optional (direct agent assignment).
- `routine_id` — `UUID`, optional (executes via routine DAG).
- `execution_run_id` — `UUID`, set automatically when linked to an execution.

## Assignment Rules (Strict)

A task **must** have **exactly one** of the following:

- `assigned_agent_id` → Executes as a single agent call
- `routine_id` → Executes through a full routine (multiple steps, gates, councils)

**Setting both is rejected.** Setting one clears the other on update.

## Dependencies

Tasks can depend on other tasks **within the same project**

**Rules:**
- A task cannot depend on itself
- Dependencies must be within the same project
- Circular dependencies are detected and rejected (DFS cycle detection)
- A task moves from `backlog` → `ready` only when **all** dependencies reach a terminal state (`done` or `failed`)
- Execution order is determined by topological sort (Kahn’s algorithm) with `order_index` as tiebreaker

## Key Relationships (Canonical)

- `part_of` → `nenjo.guide.projects`
- `references` → Routines (when `routine_id` is set)
- `references` → Agents (when `assigned_agent_id` is set)
- `defines` → its own schema, lifecycle, and dependency rules

## Runtime Behavior

When an execution run starts:

1. Tasks with unmet dependencies are moved to `backlog`
2. Tasks whose dependencies are satisfied move to `ready`
3. Ready tasks are dispatched up to the execution’s `parallel_count`
4. Assigned tasks move through `assigned` → `in_progress` → `done`/`failed`
5. Dependent tasks are re-evaluated as tasks complete

## Agent Guidance

**Reference this block when:**
- Creating, triaging, or updating tasks
- Designing routines that consume tasks
- Explaining task lifecycle, dependencies, or assignment rules to users
- Troubleshooting why a task is stuck in `backlog` or not executing

## Common Patterns

- **Simple Agent Task** — Direct `assigned_agent_id` with clear acceptance criteria
- **Routine-Driven Task** — `routine_id` pointing to a Gated Pipeline or Review Pipeline
- **Dependency Chain** — Series of tasks with clear `order_index` and acceptance criteria
- **Parallel Work** — Multiple independent tasks feeding into a single downstream task

## Pitfalls to Avoid

- Setting both `assigned_agent_id` and `routine_id` on the same task
- Creating tasks without `acceptance_criteria` (gates have nothing to evaluate)
- Forgetting that `slug` must be unique within a project
- Creating circular dependencies (use gate loops inside routines instead)
- Trying to create a task directly in `done` or `in_progress` status (only `open`, `ready`, or `backlog` allowed)

## Best Practices

- Always include meaningful `acceptance_criteria` — this is the contract between the task and any gate
- Use `required_tags` to ensure only qualified agents can work on sensitive tasks
- Prefer `routine_id` for complex or multi-step work
- Keep task titles clear and action-oriented
- Use `complexity` and `priority` together for intelligent scheduling