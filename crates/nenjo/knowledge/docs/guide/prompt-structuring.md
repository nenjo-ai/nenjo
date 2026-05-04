# Prompt Structuring

## Purpose

Prompt structuring is the practice of composing agent instructions, context blocks, runtime template variables, memory, project knowledge, and tool-discovered evidence into a prompt that is specific enough to act but small enough to stay reliable.

Use this guide when designing agent prompts, context blocks, task templates, gate templates, routine step prompts, and knowledge-heavy assistants.

For the factual list of variables and rendered shapes, read `nenjo.reference.template_vars`.

## Mental Model

Nenjo prompts have four practical layers:

1. Stable identity: who the agent is and what role it performs.
2. Reusable operating context: policies, methodology, standards, project, and domain rules.
3. Runtime context: user message, task, routine, gate, memory, and available resources.
4. Evidence gathered during the turn: tool results, builtin docs, project docs, memory recall, and file reads.

Good prompt structure keeps these layers separate. Stable instructions belong in system prompts and context blocks. Runtime facts belong in templates through variables. Evidence should be retrieved when needed instead of preloaded everywhere.

## Layer Responsibilities

### System Prompt

Use the system prompt for stable role and durable behavior.

Good fit:

- Role and responsibility.
- Decision principles.
- Communication style.
- Safety or policy constraints.
- Required use of tools for certain classes of work.

Avoid:

- One-off task details.
- Project-specific document dumps.
- Transient acceptance criteria.
- Long lists of available resources unless the agent always needs them.

Example:

```jinja
You are {{ self }}.

Operate as a pragmatic implementation agent. Use project documents and builtin
knowledge when the user asks about project conventions or Nenjo platform
concepts. Keep answers grounded in retrieved evidence.
```

### Developer Prompt and Context Blocks

Use context blocks for reusable operating knowledge that multiple agents, abilities, domains, or routines share.

Good fit:

- Team engineering standards.
- Review methodology.
- Domain-specific operating rules.
- Delegation policy.
- Prompt-writing conventions.
- Project documents
- Memories
- Compliance or governance rules.
- Git/worktree operating rules for agents that inspect or modify repository files.

Example:

```jinja
Relevant project knowledge:
{{ project.documents }}

Relevant memory:
{{ memories.project }}

Shared review standards:
{{ custom.engineering.review_standards }}

Repository worktree rules:
{{ coding.git_worktree }}

Delegation method:
{{ nenjo.core.delegation }}
```

Avoid duplicating the same policy text in many agent prompts. Put it in one context block and reference it where needed.

### Runtime Templates

Use chat, task, gate, cron, and heartbeat templates for execution-specific context.

Good fit:

- `{{ chat.message }}`
- `{{ task }}`
- `{{ routine }}`
- `{{ gate.criteria }}`
- `{{ gate.previous_output }}`
- `{{ heartbeat.last_run_at }}`

Example task template:

```jinja
Task:
{{ task }}

Complete the task against the acceptance criteria. If project documents are
insufficient, use available tools to inspect the workspace before deciding.
```

### Tool-Discovered Evidence

Use tools for specific facts that should not be dumped into every prompt.

Good fit:

- Reading selected project documents.
- Searching builtin knowledge.
- Inspecting graph neighbors.
- Reading files.
- Checking git state.
- Recalling targeted memory.

Pattern:

```text
Search compact metadata first.
Inspect graph neighbors when concepts relate.
Read only selected full documents.
Answer from the retrieved evidence.
```

## Context Budgeting

Start with the smallest context that can correctly route the work.

Recommended order:

1. Agent identity and role.
2. User request or task.
3. Project identity if project-specific.
4. Relevant available abilities or agents if routing matters.
5. Memory only when prior learned context matters.
6. Project documents only when explicit knowledge artifacts matter.
7. Builtin knowledge index only when Nenjo platform concepts matter.
8. Tool retrieval for detailed evidence.

Do not use a universal prompt that includes every variable. A prompt that always includes `{{ available_agents }}`, and `{{ available_abilities }}` will often distract the model and inflate context without improving decisions.

## Variable Selection Rules

Use `{{ agent.name }}` and `{{ agent.description }}` when the prompt needs identity and role.

Use `{{ chat.message }}` for normal conversational turns.

Use `{{ task }}` for project task execution, acceptance criteria, and task-specific planning.

Use `{{ project }}` when workspace, project identity, or project settings matter.

Use `{{ project.documents }}` when project knowledge should influence the answer. It is especially useful for architecture decisions, docs-aware planning, onboarding, and implementation tasks that must follow project conventions.

Use `{{ coding.git_worktree }}` for agents that work with synced Git repositories, repository files, or project task executions that produce code changes. Repository-backed task executions use an isolated worktree flow, so code-working agents should have the worktree rules available in either the system prompt or developer prompt.

Use `{{ builtin.documents }}` when the user asks about Nenjo concepts, resource design, context engineering, routines, agents, abilities, scopes, domains, memory, tasks, or project knowledge.

Use `{{ available_abilities }}` when the agent should decide whether to call specialist behavior.

Use `{{ available_agents }}` when delegation, councils, routing, or handoff is expected.

Use `{{ available_domains }}` when the user can explicitly activate modes or when the agent should explain available modes.

Use `{{ memories }}` when learned preferences, repeated project facts, or durable user/team conventions should influence the answer.

Use `{{ resources }}` when saved artifacts or workspace resources matter.

Use `{{ routine }}` in routine step prompts.

Use `{{ gate.criteria }}` and `{{ gate.previous_output }}` in gate prompts.

## Recommended Patterns

### General Chat Agent

Use when the agent answers user questions and occasionally consults platform knowledge.

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}

Use builtin Nenjo knowledge when the user asks about Nenjo concepts:
{{ builtin.documents }}

User request:
{{ chat.message }}
```

Why this works:

- Keeps identity stable.
- Keeps the user request explicit.
- Provides a discovery path without dumping full docs.

### Project Implementation Agent

Use when the agent works inside a project and must respect project documents and memory.

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}

Project:
{{ project }}

Task:
{{ task }}

Project knowledge:
{{ project.documents }}

Project memory:
{{ memories.project }}

Workspace resources:
{{ resources.project }}

Git worktree rules:
{{ coding.git_worktree }}

Use project documents as explicit knowledge and memory as learned context.
When either is insufficient, inspect the workspace before making implementation
claims.
```

Why this works:

- Separates project documents from memory.
- Grounds execution in the current task.
- Supplies worktree rules for repository-backed task execution.
- Leaves detailed evidence retrieval to tools.

### Ability-Aware Agent

Use when the agent has specialist abilities and should choose when to invoke them.

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}

Available abilities:
{{ available_abilities }}

Task or request:
{{ task }}
{{ chat.message }}

Use an ability when its activation condition matches the work. Do not call
abilities that do not materially improve the result.
```

Why this works:

- Makes ability selection explicit.
- Keeps activation conditions visible.
- Avoids unnecessary specialist calls.

### Delegating Agent

Use when an agent can delegate to other agents or coordinate a council.

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}

Available agents:
{{ available_agents }}

Task:
{{ task }}

Delegate only when another agent has clearly better expertise or when parallel
review materially reduces risk. Summarize delegated results before deciding.
```

Why this works:

- Gives the model enough context to route work.
- Limits delegation to useful cases.

### Gate Evaluator

Use when the agent must judge evidence against criteria.

```jinja
You are a gate evaluator.

Criteria:
{{ gate.criteria }}

Evidence:
{{ gate.previous_output }}

Return pass only when the evidence satisfies every required criterion. Return
fail with specific missing criteria otherwise.
```

Why this works:

- Removes unrelated context.
- Keeps evaluation deterministic.
- Focuses on evidence and criteria.

### Routine Step Agent

Use when a routine step needs workflow context plus a focused task.

```jinja
You are {{ agent.name }}.

Routine:
{{ routine }}

Current task:
{{ task }}

Project:
{{ project }}

Complete only the current routine step. If the step requires a decision, make
the decision explicit and explain what downstream step should consume.
```

Why this works:

- Keeps step-local work bounded.
- Preserves routine context.

### Knowledge-Heavy Nenjo Advisor

Use when the user asks how to design agents, routines, abilities, memory, scopes, context blocks, or prompt structure.

```jinja
You are {{ agent.name }}.

Role:
{{ agent.description }}

Builtin Nenjo knowledge:
{{ builtin.documents }}

User intent:
{{ chat.message }}

Classify the intent into likely Nenjo concepts. Search builtin docs, inspect
graph neighbors for related concepts, then read selected docs before answering.
Answer with a practical design recommendation and name the knowledge used.
```

Why this works:

- Treats builtin docs as a graph, not a flat search index.
- Forces retrieval before recommendation.

### Memory-Aware Agent

Use when learned preferences or recurring context should shape the answer.

```jinja
You are {{ agent.name }}.

Memory profile:
{{ memory_profile }}

Relevant memories:
{{ memories }}

User request:
{{ chat.message }}

Use memory for learned preferences and recurring facts. Do not treat memory as
authoritative project documentation when project documents disagree.
```

Why this works:

- Makes memory purpose explicit.
- Reduces confusion between remembered facts and source-of-truth docs.

## Structuring Context Blocks

Context blocks should be modular and reusable.

Good context block:

```jinja
# Code Review Standards

Review for correctness, maintainability, security, test coverage, and migration
risk. Prioritize concrete findings with file references. Keep summaries brief.
```

Weak context block:

```jinja
Use this project's task title: {{ task.title }}
Remember the user asked for release notes yesterday.
```

The weak example mixes runtime state and memory-like facts into reusable prompt text. Put task facts in task templates and learned facts in memory.

## Knowledge Retrieval Pattern

When a prompt exposes `{{ builtin_documents }}` or `{{ project.documents }}`, the model should not rely only on the index. The index is for discovery.

Recommended retrieval flow:

1. Search compact paths or manifests.
2. Read candidate manifests.
3. Inspect graph neighbors when concepts are connected.
4. Read selected full documents.
5. Answer from the retrieved evidence.

Use graph expansion for:

- "What should I use with X?"
- "How do I structure X?"
- "What concepts are related to X?"
- "What should I read before implementing X?"
- "How do agents, abilities, scopes, memory, and projects fit together?"

## Anti-Patterns

- Including every variable in every prompt.
- Treating `{{ builtin.documents }}` as a full documentation dump.
- Treating `{{ project.documents }}` as memory.
- Treating `{{ memories }}` as project documentation.
- Putting task details in system prompts.
- Putting durable team standards in one-off user messages.
- Exposing `{{ available_agents }}` when delegation is not allowed.
- Exposing `{{ available_abilities }}` when abilities are not available.
- Asking gates to solve the task instead of evaluating evidence.
- Asking routine steps to reason about the whole workflow when they only own one step.

## Review Checklist

Before shipping a prompt:

- Does each included variable support a specific decision?
- Are stable instructions separated from runtime facts?
- Are project documents and memory clearly distinguished?
- Are available abilities or agents exposed only when useful?
- Is builtin knowledge used as a discovery path instead of a dump?
- Does the prompt say when to retrieve more evidence?
- Is the output format appropriate for the runtime mode?
