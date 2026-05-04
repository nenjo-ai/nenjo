# Councils — Structured Multi-Agent Collaboration

## Purpose
Councils are structured multi-agent collaboration groups designed for work that benefits from multiple perspectives, decomposition, synthesis, adversarial review, or deliberate decision-making. They exist for situations where a single agent pass is insufficient.

## When to Use a Council

Use a council when the work requires one or more of the following:

- Multiple specialist perspectives
- Parallel subtasking
- Adversarial review or critique
- Explicit synthesis of different outputs
- Voting or comparative judgment
- Complex decision-making that benefits from structured debate

**Do not use a council** when a single strong agent (with one or two abilities) would be simpler and clearer.

## Core Model

A council consists of:

- **Leader Agent** — Coordinates the collaboration and usually synthesizes the final output
- **Members** — One or more additional agents, each with a defined role and priority
- **Delegation Strategy** — Defines how work is distributed among members

## Delegation Strategies

| Strategy      | Description                                                                 | Best Used When |
|---------------|-----------------------------------------------------------------------------|----------------|
| **Dynamic**   | Leader decides at runtime who to involve                                    | Unpredictable work shape |
| **Broadcast** | All members receive the same task independently; leader synthesizes         | Need independent views without cross-contamination |
| **Decompose** | Leader breaks work into distinct subtasks, one per member                   | Naturally separable work |
| **Round Robin** | Members work sequentially, each building on the previous output            | Staged refinement is valuable |
| **Vote**      | Members respond independently; leader chooses or summarizes the decision    | Selection between clear alternatives |

## Council Member Design

Good council members should be **deliberately distinct**. Each member should have:

- A different role or lens
- Different tools or scopes where appropriate
- A clear reason to exist in the council

**Anti-pattern**: Creating a council of near-identical agents adds cost without adding signal.

## Key Relationships (Canonical)

- `part_of` → `nenjo.domain.nenjo`
- `references` → Agents (leader + members)
- `references` → Delegation Strategies
- `defines` → collaboration topology and member roles

## Runtime Behavior

When a council step is reached in a routine:

1. The leader agent receives the task and current context
2. Depending on the delegation strategy, work is distributed to members
3. Members execute in parallel or sequence as defined
4. The leader synthesizes the final output (or selects from votes)
5. The council result is passed to the next step in the routine

Councils are typically used **inside routines** as a single step, not as standalone execution units.

## Agent Guidance

**Reference this block when:**
- Designing complex workflows that need multiple perspectives
- Choosing between a single agent, ability, or full council
- Explaining collaboration patterns to users
- Troubleshooting council output quality or member contribution

## Common Patterns

- **Code Review Council** — One agent writes code, another reviews, leader synthesizes
- **Risk Assessment Council** — Multiple domain experts evaluate different risk dimensions
- **Strategic Planning Council** — Leader decomposes problem, specialists contribute, leader synthesizes
- **Adversarial Review Council** — One member proposes, another critiques, leader decides

## Pitfalls to Avoid

- Creating councils just to sound sophisticated
- Using duplicate member roles with no distinct perspective
- Leaving the leader role underspecified
- Using a council where a single agent + one ability would suffice
- Forgetting to define a clear delegation strategy

## Best Practices

- Keep councils small (3–5 members max)
- Make member roles explicit and distinct
- Choose the delegation strategy deliberately based on the nature of the work
- Use councils inside routines rather than as top-level execution
- Document why each member exists in the council